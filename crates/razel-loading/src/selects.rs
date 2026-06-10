//! select(): deferred values, condition matching, attr-value resolution.

use crate::state::{canon_label, session};
use allocative::Allocative;
use starlark::any::ProvidesStaticType;
use starlark::coerce::Coerce;
use starlark::eval::Evaluator;
use starlark::starlark_complex_value;
use starlark::values::list::ListRef;
use starlark::values::{
    Freeze, Heap, NoSerialize, StarlarkValue, Trace, Value, ValueLifetimeless, ValueLike,
    starlark_value,
};
use std::fmt;
use crate::labels::LabelV;


// ---- select: deferred values + resolution (Bazel's select-as-value model) ---------------------


/// One deferred `select({...})`: raw (key, value) branch pairs, resolved at attr consumption.
/// Freeze-generic — module-level selects in `.bzl` freeze with their module.
#[derive(Debug, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub(crate) struct SelectBranchesGen<V: ValueLifetimeless> {
    pub(crate) branches: Vec<(V, V)>,
}


starlark_complex_value!(pub(crate) SelectBranches);


impl<V: ValueLifetimeless> fmt::Display for SelectBranchesGen<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "select({{…}})")
    }
}


#[starlark_value(type = "select")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for SelectBranchesGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    /// `select({...}) + x` — a select expression (Bazel's concatenation).
    fn add(&self, other: Value<'v>, heap: Heap<'v>) -> Option<starlark::Result<Value<'v>>> {
        let me = heap.alloc(SelectBranches {
            branches: self.branches.iter().map(|(k, v)| (k.to_value(), v.to_value())).collect(),
        });
        Some(Ok(heap.alloc(SelectExpr { parts: vec![me, other] })))
    }
    /// `x + select({...})`.
    fn radd(&self, lhs: Value<'v>, heap: Heap<'v>) -> Option<starlark::Result<Value<'v>>> {
        let me = heap.alloc(SelectBranches {
            branches: self.branches.iter().map(|(k, v)| (k.to_value(), v.to_value())).collect(),
        });
        Some(Ok(heap.alloc(SelectExpr { parts: vec![lhs, me] })))
    }
}


/// A select EXPRESSION: ordered parts (plain lists and selects) — `["-a"] + select({…}) + …`.
/// Freeze-generic: real `.bzl` build these in module-level default args (XLA's tsl.bzl).
#[derive(Debug, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub(crate) struct SelectExprGen<V: ValueLifetimeless> {
    pub(crate) parts: Vec<V>,
}


starlark_complex_value!(pub(crate) SelectExpr);


impl<V: ValueLifetimeless> fmt::Display for SelectExprGen<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "select-expr({} parts)", self.parts.len())
    }
}


#[starlark_value(type = "select_expr")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for SelectExprGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn add(&self, other: Value<'v>, heap: Heap<'v>) -> Option<starlark::Result<Value<'v>>> {
        let mut parts: Vec<Value<'v>> = self.parts.iter().map(|p| p.to_value()).collect();
        parts.push(other);
        Some(Ok(heap.alloc(SelectExpr { parts })))
    }
    fn radd(&self, lhs: Value<'v>, heap: Heap<'v>) -> Option<starlark::Result<Value<'v>>> {
        let mut parts = vec![lhs];
        parts.extend(self.parts.iter().map(|p| p.to_value()));
        Some(Ok(heap.alloc(SelectExpr { parts })))
    }
}


/// A select condition key as a canonical-izable string: a string label or a `Label` struct
/// (`.package`/`.name` — `clean_dep()` results in real `.bzl`).
pub(crate) fn key_string<'v>(heap: Heap<'v>, key: Value<'v>) -> Option<String> {
    if let Some(s) = key.unpack_str() {
        return Some(s.to_string());
    }
    if let Some(l) = key.downcast_ref::<LabelV>() {
        return Some(l.to_string());
    }
    let pkg = key.get_attr("package", heap).ok()??.unpack_str()?.to_string();
    let name = key.get_attr("name", heap).ok()??.unpack_str()?.to_string();
    Some(format!("//{pkg}:{name}"))
}


/// Resolve a select's branches against the configuration. `defer_on_undeclared`: return
/// `Ok(None)` when a condition isn't a declared config_setting (the caller defers); at analysis
/// consumption it's `false` and undeclared conditions error loudly.
pub(crate) fn pick_branch<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    pairs: &[(Value<'v>, Value<'v>)],
    defer_on_undeclared: bool,
    allow_load: bool,
) -> anyhow::Result<Option<Value<'v>>> {
    let heap = eval.heap();
    let mut default: Option<Value<'v>> = None;
    let mut matches: Vec<(String, crate::state::ConfigSpec, Value<'v>)> = Vec::new();
    for (k, v) in pairs {
        let Some(cond) = key_string(heap, *k) else {
            return Err(anyhow::anyhow!("select(): condition key is not a label"));
        };
        if cond == "//conditions:default" {
            default = Some(*v);
            continue;
        }
        let sess = session(eval);
        let canon = canon_label(sess, &cond);
        match condition_matches(sess, &canon, allow_load, 16)? {
            Some(true) => {
                let spec = deref_spec(sess, &canon).unwrap_or_default();
                matches.push((cond, spec, *v));
            }
            Some(false) => {}
            None if defer_on_undeclared => return Ok(None),
            None => {
                return Err(anyhow::anyhow!(
                    "select(): `{cond}` is not a declared config_setting"
                ));
            }
        }
    }
    if matches.is_empty() {
        return default
            .map(Some)
            .ok_or_else(|| {
                anyhow::anyhow!("select() matched no condition and has no //conditions:default")
            });
    }
    // Most-specialized wins: the winner's constraint set must contain every other match's.
    matches.sort_by_key(|(_, s, _)| std::cmp::Reverse(s.constraints().len()));
    let win = matches[0].1.constraints();
    for (cond, s, _) in &matches[1..] {
        if !s.constraints().is_subset(&win) {
            return Err(anyhow::anyhow!(
                "select() is ambiguous: `{}` and `{cond}` both match and neither specializes \
                 the other",
                matches[0].0
            ));
        }
    }
    Ok(Some(matches[0].2))
}


/// Does a condition label match the configuration? Follows `alias()` chains, answers FALSE for
/// host-materialized GPU repos and unmodeled (`flag_values`) settings, recurses through
/// `config_setting_group`s, and (when allowed) loads the condition's package on demand — a failed
/// load SURFACES. `None` ⇒ undeclared (the caller defers or errors).
pub(crate) fn condition_matches(
    sess: &crate::state::Session,
    canon: &str,
    allow_load: bool,
    fuel: u32,
) -> anyhow::Result<Option<bool>> {
    if fuel == 0 {
        return Err(anyhow::anyhow!("condition alias/group nesting too deep at `{canon}`"));
    }
    let aliased = sess.aliases.borrow().get(canon).cloned();
    if let Some(t) = aliased {
        return condition_matches(sess, &t, allow_load, fuel - 1);
    }
    if crate::host::host_false_condition(canon) {
        return Ok(Some(false));
    }
    let spec = sess.config_specs.borrow().get(canon).cloned();
    let spec = match spec {
        Some(s) => s,
        None => {
            if allow_load
                && sess.workspace.is_some()
                && let Some(pkg) = crate::state::pkg_of(canon)
            {
                if let Err(e) = crate::rules::load_package(sess, &pkg)
                    && !sess.config_specs.borrow().contains_key(canon)
                    && !sess.aliases.borrow().contains_key(canon)
                {
                    return Err(anyhow::anyhow!(
                        "select(): loading `{canon}`'s package failed: {e}"
                    ));
                }
                return condition_matches(sess, canon, false, fuel - 1);
            }
            return Ok(None);
        }
    };
    if spec.unmodeled {
        return Ok(Some(false));
    }
    if let Some((all, members)) = &spec.group {
        for m in members {
            match condition_matches(sess, m, allow_load, fuel - 1)? {
                Some(hit) => {
                    if *all && !hit {
                        return Ok(Some(false));
                    }
                    if !*all && hit {
                        return Ok(Some(true));
                    }
                }
                None => {
                    return Err(anyhow::anyhow!(
                        "config_setting_group member `{m}` not declared"
                    ));
                }
            }
        }
        return Ok(Some(*all));
    }
    spec.matches(&sess.global).map(Some).map_err(|e| anyhow::anyhow!(e))
}


/// The (alias-dereffed) spec for specialization ordering; groups/undeclared → empty constraints.
pub(crate) fn deref_spec(sess: &crate::state::Session, canon: &str) -> Option<crate::state::ConfigSpec> {
    let mut cur = canon.to_string();
    for _ in 0..16 {
        match sess.aliases.borrow().get(&cur).cloned() {
            Some(next) => cur = next,
            None => break,
        }
    }
    sess.config_specs.borrow().get(&cur).cloned()
}


/// Resolve any deferred select machinery in an attr VALUE at consumption time (analysis):
/// a deferred select picks its branch; a select expression resolves each part and concatenates
/// list parts. Plain values pass through.
pub(crate) fn resolve_attr_value<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    v: Value<'v>,
) -> anyhow::Result<Value<'v>> {
    let pairs: Option<Vec<(Value<'v>, Value<'v>)>> =
        if let Some(sb) = v.downcast_ref::<SelectBranches<'v>>() {
            Some(sb.branches.clone())
        } else if let Some(sb) = v.downcast_ref::<FrozenSelectBranches>() {
            Some(sb.branches.iter().map(|(k, x)| (k.to_value(), x.to_value())).collect())
        } else {
            None
        };
    if let Some(pairs) = pairs {
        let picked = pick_branch(eval, &pairs, false, true)?.expect("non-deferring pick");
        return resolve_attr_value(eval, picked);
    }
    let expr_parts: Option<Vec<Value<'v>>> =
        if let Some(se) = v.downcast_ref::<SelectExpr<'v>>() {
            Some(se.parts.clone())
        } else if let Some(se) = v.downcast_ref::<FrozenSelectExpr>() {
            Some(se.parts.iter().map(|p| p.to_value()).collect())
        } else {
            None
        };
    if let Some(parts) = expr_parts {
        let mut out: Vec<Value<'v>> = Vec::new();
        for p in parts {
            let r = resolve_attr_value(eval, p)?;
            if let Some(l) = ListRef::from_value(r) {
                out.extend(l.iter());
            } else {
                return Err(anyhow::anyhow!(
                    "select concatenation parts must be lists (got `{r}`)"
                ));
            }
        }
        return Ok(eval.heap().alloc(out));
    }
    Ok(v)
}
