//! The rule-authoring API: Ctx, the rule() object, rule_globals (rule/Label/select/providers). C0.

use crate::state::{AnalyzedTarget, canon_label, qualify, session, with_current};
use crate::values::{Actions, Depset, File, extract_files, file_path, unpack};
use crate::glob::do_glob;
use crate::deps::record_target;
use razel_dds::InstanceId;
use allocative::Allocative;
use starlark::any::ProvidesStaticType;
use starlark::coerce::Coerce;
use starlark::collections::SmallMap;
use starlark::environment::{GlobalsBuilder, Module};
use starlark::eval::{Arguments, Evaluator};
use starlark::starlark_complex_value;
use starlark::values::list::{ListRef, UnpackList};
use starlark::values::tuple::UnpackTuple;
use starlark::values::none::NoneType;
use starlark::values::structs::AllocStruct;
use starlark::values::{
    Freeze, Heap, NoSerialize, StarlarkValue, Trace, Value, ValueLifetimeless, ValueLike,
    starlark_value,
};
use std::cell::RefCell;
use std::fmt;



// ---- ctx ------------------------------------------------------------------------


/// The analysis `ctx`. All fields are heap `Value`s so it traces cleanly, no freezing.
#[derive(Debug, NoSerialize, ProvidesStaticType, Allocative, Trace)]
pub(crate) struct Ctx<'v> {
    attr: Value<'v>,
    actions: Value<'v>,
    label: Value<'v>,
    /// `ctx.outputs.<name>` — predeclared output filenames (package-qualified).
    outputs: Value<'v>,
    /// `ctx.files.<name>` — the source files of label/label_list attrs (qualified).
    files: Value<'v>,
    /// `ctx.executable.<name>` — the runnable output of an executable-label attr.
    executable: Value<'v>,
}


impl fmt::Display for Ctx<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<ctx>")
    }
}


#[starlark_value(type = "ctx")]
impl<'v> StarlarkValue<'v> for Ctx<'v> {
    fn get_attr(&self, attribute: &str, _heap: Heap<'v>) -> Option<Value<'v>> {
        match attribute {
            "attr" => Some(self.attr),
            "actions" => Some(self.actions),
            "label" => Some(self.label),
            "outputs" => Some(self.outputs),
            "files" => Some(self.files),
            "executable" => Some(self.executable),
            _ => None,
        }
    }
}


// ---- rule() + DefaultInfo + select ----------------------------------------------


/// A `rule()` value. Generic over `V` so it has both an unfrozen form (`RuleObj<'v>`,
/// holding a live `Value`) and a frozen form (`FrozenRuleObj`, holding a `FrozenValue`)
/// — which is what lets a rule **survive `module.freeze()`** and therefore be defined
/// in a `.bzl` and `load()`ed, not just inline. The impl function freezes with it.
#[derive(Debug, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub(crate) struct RuleObjGen<V: ValueLifetimeless> {
    implementation: V,
    /// The declared `attrs` schema (name → attr descriptor), frozen with the rule and consulted at
    /// instantiation for defaults / `mandatory` (D1). `None` when no schema was declared.
    attrs: V,
}
starlark_complex_value!(RuleObj);

impl<V: ValueLifetimeless> fmt::Display for RuleObjGen<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<rule>")
    }
}


#[starlark_value(type = "rule")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for RuleObjGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    /// `my_rule(name=…, …)` — RECORD the declaration (E0 phase split). Dep resolution and the
    /// impl run later, in the demand-driven analysis pass ([`drive_decls`]) — which is what makes
    /// forward references within a package resolve (Bazel loads a package before analyzing it).
    fn invoke(
        &self,
        me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let named = args.names_map()?;
        let sess = session(eval);
        let kwargs: Vec<(String, Value<'v>)> =
            named.iter().map(|(k, v)| (k.as_str().to_string(), *v)).collect();
        let name = kwargs
            .iter()
            .find(|(k, _)| k == "name")
            .and_then(|(_, v)| v.unpack_str())
            .unwrap_or_default()
            .to_string();
        let label = canon_label(sess, &name);
        let store = decl_store(eval)?;
        let idx = {
            let mut decls = store.decls.borrow_mut();
            decls.push(Some(Decl { label: label.clone(), body: DeclBody::Rule { rule: me, kwargs } }));
            decls.len() - 1
        };
        sess.pending.borrow_mut().insert(label, idx);
        Ok(Value::new_none())
    }
}


/// Expand genrule `cmd` Make-variables: `$$`→`$`, `$@` (exactly one output), `$<` (exactly one
/// src), `$(SRCS)`/`$(OUTS)` (space-joined), `$(location X)` (exactly one path for the as-written
/// src/label `X`) / `$(locations X)` (all, space-joined). Anything else `$(…)` errors LOUDLY —
/// unmodeled Make variables must never pass through silently (Bazel-compat discipline).
fn expand_genrule_cmd(
    cmd: &str,
    srcs: &[String],
    outs: &[String],
    loc: &[(String, Vec<String>)],
) -> anyhow::Result<String> {
    let lookup = |x: &str| -> anyhow::Result<&Vec<String>> {
        loc.iter()
            .find(|(k, _)| k == x)
            .map(|(_, v)| v)
            .ok_or_else(|| anyhow::anyhow!("$(location {x}): `{x}` is not in this genrule's srcs"))
    };
    let mut out = String::with_capacity(cmd.len());
    let mut it = cmd.chars().peekable();
    while let Some(c) = it.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('$') => out.push('$'),
            Some('@') => match outs {
                [one] => out.push_str(one),
                _ => return Err(anyhow::anyhow!("$@ requires exactly one output (genrule)")),
            },
            Some('<') => match srcs {
                [one] => out.push_str(one),
                _ => return Err(anyhow::anyhow!("$< requires exactly one src (genrule)")),
            },
            Some('(') => {
                let inner: String = it.by_ref().take_while(|&c| c != ')').collect();
                match inner.split_once(' ') {
                    None if inner == "SRCS" => out.push_str(&srcs.join(" ")),
                    None if inner == "OUTS" => out.push_str(&outs.join(" ")),
                    Some(("location", x)) => match lookup(x.trim())?.as_slice() {
                        [one] => out.push_str(one),
                        many => {
                            return Err(anyhow::anyhow!(
                                "$(location {x}) matches {} files — use $(locations …)",
                                many.len()
                            ));
                        }
                    },
                    Some(("locations", x)) => out.push_str(&lookup(x.trim())?.join(" ")),
                    _ => {
                        return Err(anyhow::anyhow!(
                            "$({inner}) is not a modeled genrule Make variable \
                             (razel models SRCS/OUTS/location/locations)"
                        ));
                    }
                }
            }
            other => {
                return Err(anyhow::anyhow!(
                    "unsupported `$` escape `${}` in genrule cmd",
                    other.map(String::from).unwrap_or_default()
                ));
            }
        }
    }
    Ok(out)
}


// ---- select: deferred values + resolution (Bazel's select-as-value model) ---------------------


/// One deferred `select({...})`: raw (key, value) branch pairs, resolved at attr consumption.
/// Freeze-generic — module-level selects in `.bzl` freeze with their module.
#[derive(Debug, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub(crate) struct SelectBranchesGen<V: ValueLifetimeless> {
    branches: Vec<(V, V)>,
}
starlark_complex_value!(SelectBranches);

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
        let me = heap.alloc_complex_no_freeze(SelectBranches {
            branches: self.branches.iter().map(|(k, v)| (k.to_value(), v.to_value())).collect(),
        });
        Some(Ok(heap.alloc_complex_no_freeze(SelectExpr { parts: vec![me, other] })))
    }
    /// `x + select({...})`.
    fn radd(&self, lhs: Value<'v>, heap: Heap<'v>) -> Option<starlark::Result<Value<'v>>> {
        let me = heap.alloc_complex_no_freeze(SelectBranches {
            branches: self.branches.iter().map(|(k, v)| (k.to_value(), v.to_value())).collect(),
        });
        Some(Ok(heap.alloc_complex_no_freeze(SelectExpr { parts: vec![lhs, me] })))
    }
}

/// A select EXPRESSION: ordered parts (plain lists and selects) — `["-a"] + select({…}) + …`.
/// Lives only within an eval scope (built in BUILD/macro expressions); resolution concatenates.
#[derive(Debug, ProvidesStaticType, NoSerialize, Allocative, Trace)]
pub(crate) struct SelectExpr<'v> {
    parts: Vec<Value<'v>>,
}

impl fmt::Display for SelectExpr<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "select-expr({} parts)", self.parts.len())
    }
}

#[starlark_value(type = "select_expr")]
impl<'v> StarlarkValue<'v> for SelectExpr<'v> {
    fn add(&self, other: Value<'v>, heap: Heap<'v>) -> Option<starlark::Result<Value<'v>>> {
        let mut parts = self.parts.clone();
        parts.push(other);
        Some(Ok(heap.alloc_complex_no_freeze(SelectExpr { parts })))
    }
    fn radd(&self, lhs: Value<'v>, heap: Heap<'v>) -> Option<starlark::Result<Value<'v>>> {
        let mut parts = vec![lhs];
        parts.extend(self.parts.iter().copied());
        Some(Ok(heap.alloc_complex_no_freeze(SelectExpr { parts })))
    }
}

/// A select condition key as a canonical-izable string: a string label or a `Label` struct
/// (`.package`/`.name` — `clean_dep()` results in real `.bzl`).
fn key_string<'v>(heap: Heap<'v>, key: Value<'v>) -> Option<String> {
    if let Some(s) = key.unpack_str() {
        return Some(s.to_string());
    }
    let pkg = key.get_attr("package", heap).ok()??.unpack_str()?.to_string();
    let name = key.get_attr("name", heap).ok()??.unpack_str()?.to_string();
    Some(format!("//{pkg}:{name}"))
}

/// Resolve a select's branches against the configuration. `defer_on_undeclared`: return
/// `Ok(None)` when a condition isn't a declared config_setting (the caller defers); at analysis
/// consumption it's `false` and undeclared conditions error loudly.
fn pick_branch<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    pairs: &[(Value<'v>, Value<'v>)],
    defer_on_undeclared: bool,
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
        // Cross-package condition: load its package on demand (the borrow in the condition
        // drops before the nested eval — the [R1] discipline).
        if !sess.config_specs.borrow().contains_key(&canon)
            && sess.workspace.is_some()
            && let Some(pkg) = crate::state::pkg_of(&canon)
        {
            let _ = crate::rules::load_package(sess, &pkg);
        }
        let spec = match sess.config_specs.borrow().get(&canon).cloned() {
            Some(s) => s,
            None if defer_on_undeclared => return Ok(None),
            None => {
                return Err(anyhow::anyhow!(
                    "select(): `{cond}` is not a declared config_setting"
                ));
            }
        };
        if spec.matches(&sess.global).map_err(|e| anyhow::anyhow!(e))? {
            matches.push((cond, spec, *v));
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
        let picked = pick_branch(eval, &pairs, false)?.expect("non-deferring pick");
        return resolve_attr_value(eval, picked);
    }
    if let Some(se) = v.downcast_ref::<SelectExpr<'v>>() {
        let parts = se.parts.clone();
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


// ---- E0: the phase split — declaration store + demand-driven analysis -------------------------


/// The module variable holding the package's [`DeclStore`] — installed by the analysis entry
/// points before BUILD eval; not addressable from Starlark source.
pub(crate) const DECLS_VAR: &str = "__razel_decls";

/// One recorded rule instantiation, analyzed on demand.
#[derive(Debug, Allocative, Trace)]
struct Decl<'v> {
    label: String,
    body: DeclBody<'v>,
}

/// What analyzing a declaration means: run a Starlark rule (value + raw kwargs) or a deferred
/// native body (an index into `Session.native_decls` — the closure lives off-heap, E0c).
#[derive(Debug, Allocative, Trace)]
enum DeclBody<'v> {
    Rule { rule: Value<'v>, kwargs: Vec<(String, Value<'v>)> },
    Native(usize),
}

/// The package's recorded declarations — a heap value, so the `'v`-bound rule/kwarg values live on
/// the module heap across the eval→analyze boundary. Slots are `take()`n when analyzed.
#[derive(Debug, Default, ProvidesStaticType, NoSerialize, Allocative, Trace)]
pub(crate) struct DeclStore<'v> {
    decls: RefCell<Vec<Option<Decl<'v>>>>,
    /// L2a: the provider instances each analyzed target RETURNED (canonical label →
    /// `[(constructor, instance)]`) — what `dep[MyInfo]` indexes. Heap-resident like the decls
    /// (instances are `'v` values), so the flow is same-package; cross-package custom providers
    /// are a later rung (the typed-algebra channel still crosses).
    captured: RefCell<SmallMap<String, Vec<(Value<'v>, Value<'v>)>>>,
}

impl fmt::Display for DeclStore<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<decls>")
    }
}

#[starlark_value(type = "razel_decls")]
impl<'v> StarlarkValue<'v> for DeclStore<'v> {}

/// Install an empty declaration store on the module. Every BUILD-eval entry point MUST call this
/// before evaluation and [`drive_decls`] after — rule instantiation without it is an error.
pub(crate) fn install_decl_store<'v>(module: &Module<'v>) {
    module.set(DECLS_VAR, module.heap().alloc_complex_no_freeze(DeclStore::default()));
}

/// The store installed on the current module.
fn decl_store<'v>(eval: &Evaluator<'v, '_, '_>) -> anyhow::Result<&'v DeclStore<'v>> {
    let v = eval.module().get(DECLS_VAR).ok_or_else(|| {
        anyhow::anyhow!("rule instantiation outside a package analysis (no declaration store)")
    })?;
    v.downcast_ref::<DeclStore>()
        .ok_or_else(|| anyhow::anyhow!("declaration store has the wrong type"))
}

/// A rule value's (implementation, attrs-schema), frozen or not.
fn rule_parts<'v>(rule: Value<'v>) -> anyhow::Result<(Value<'v>, Value<'v>)> {
    if let Some(r) = rule.downcast_ref::<RuleObj<'v>>() {
        Ok((r.implementation, r.attrs))
    } else if let Some(r) = rule.downcast_ref::<FrozenRuleObj>() {
        Ok((r.implementation.to_value(), r.attrs.to_value()))
    } else {
        Err(anyhow::anyhow!("declaration's rule is not a rule value"))
    }
}

/// Phase 2: analyze every recorded declaration, in declaration order (demand-recursion may pull a
/// forward-referenced one earlier; its slot is then empty when the loop reaches it).
pub(crate) fn drive_decls<'v>(eval: &mut Evaluator<'v, '_, '_>) -> starlark::Result<()> {
    let mut i = 0;
    loop {
        let n = decl_store(eval)?.decls.borrow().len();
        if i >= n {
            return Ok(());
        }
        analyze_decl(eval, i)?;
        i += 1;
    }
}

/// Ensure `label` (canonical) is analyzed: no-op if already in results; demand-analyze if it is a
/// pending local declaration; otherwise leave it to the caller's existing resolution/error path.
pub(crate) fn ensure_analyzed<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    label: &str,
) -> starlark::Result<()> {
    let idx = {
        let sess = session(eval);
        if sess.results.borrow().contains_key(label) {
            return Ok(());
        }
        if sess.analyzing.borrow().contains(label) {
            return Err(anyhow::anyhow!("dependency cycle detected at `{label}`").into());
        }
        sess.pending.borrow().get(label).copied()
    };
    match idx {
        Some(i) => analyze_decl(eval, i),
        None => Ok(()),
    }
}

/// Analyze the declaration at `idx` (no-op if its slot was already taken). Cycle-guarded via
/// `Session.analyzing`.
fn analyze_decl<'v>(eval: &mut Evaluator<'v, '_, '_>, idx: usize) -> starlark::Result<()> {
    let decl = { decl_store(eval)?.decls.borrow_mut()[idx].take() };
    let Some(decl) = decl else { return Ok(()) };
    {
        let sess = session(eval);
        if !sess.analyzing.borrow_mut().insert(decl.label.clone()) {
            return Err(
                anyhow::anyhow!("dependency cycle detected at `{}`", decl.label).into()
            );
        }
        sess.pending.borrow_mut().remove(&decl.label);
    }
    let res = match &decl.body {
        DeclBody::Rule { rule, kwargs } => analyze_rule_decl(eval, *rule, kwargs),
        DeclBody::Native(nidx) => {
            let f = { session(eval).native_decls.borrow_mut()[*nidx].take() };
            match f {
                Some(f) => f(eval).map_err(Into::into),
                None => Ok(()),
            }
        }
    };
    session(eval).analyzing.borrow_mut().remove(&decl.label);
    res
}

/// Record a deferred native-rule analysis (E0c): the rule fn extracts its plain attrs at eval time
/// and hands the body here; the demand-driven pass runs it — so native targets forward-reference
/// (and interleave with Starlark-rule targets) like everything else.
pub(crate) fn record_native<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    label: String,
    f: crate::state::NativeAnalyzeFn,
) -> anyhow::Result<()> {
    let sess = session(eval);
    let nidx = {
        let mut v = sess.native_decls.borrow_mut();
        v.push(Some(f));
        v.len() - 1
    };
    let store = decl_store(eval)?;
    let idx = {
        let mut decls = store.decls.borrow_mut();
        decls.push(Some(Decl { label: label.clone(), body: DeclBody::Native(nidx) }));
        decls.len() - 1
    };
    sess.pending.borrow_mut().insert(label, idx);
    Ok(())
}

/// The analysis of one Starlark-rule declaration — dep resolution (demand-driven), schema
/// defaults/`mandatory`, ctx construction, and the impl call. (Ran inside `invoke()` before E0.)
fn analyze_rule_decl<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    rule: Value<'v>,
    kwargs: &[(String, Value<'v>)],
) -> starlark::Result<()> {
    let (implementation, attrs) = rule_parts(rule)?;
    // Deferred selects resolve HERE — attr consumption time (Bazel's model); conditions declared
    // anywhere in the loaded graph are visible by now.
    let kwargs: Vec<(String, Value<'v>)> = kwargs
        .iter()
        .map(|(k, v)| Ok((k.clone(), resolve_attr_value(eval, *v)?)))
        .collect::<anyhow::Result<_>>()?;
    let mut name = String::new();
    let mut dep_labels: Vec<String> = Vec::new();
    let mut fields: Vec<(String, Value<'v>)> = Vec::new();
    for (key, v) in &kwargs {
        let (key, v) = (key.clone(), *v);
        // D1b/c: the schema kind drives label resolution. Look it up once: `label`/`label_list`
        // resolve to provider struct(s); the legacy `deps` is an implicit `label_list`.
        let attr_kind: Option<String> = {
            let mut k = None;
            if let Some(d) = starlark::values::dict::DictRef::from_value(attrs) {
                for (kk, desc) in d.iter() {
                    if kk.unpack_str() == Some(key.as_str()) {
                        if let Ok(Some(kind)) = desc.get_attr("kind", eval.heap()) {
                            k = kind.unpack_str().map(String::from);
                        }
                        break;
                    }
                }
            }
            k
        };
        let is_label =
            key == "deps" || matches!(attr_kind.as_deref(), Some("label") | Some("label_list"));
        let single_label = attr_kind.as_deref() == Some("label");
        match key.as_str() {
            "name" => {
                name = v.unpack_str().unwrap_or_default().to_string();
                fields.push((key, v));
            }
            // A label attr (legacy `deps` or any `attr.label_list`): resolve each label to its
            // analyzed providers as a `struct(files=…, <folded fields>…)`.
            _ if is_label => {
                // Collect the label string(s): a list (`label_list`/`deps`) or a single (`label`).
                let labels: Vec<String> = if let Some(list) = ListRef::from_value(v) {
                    list.iter().filter_map(|x| x.unpack_str().map(String::from)).collect()
                } else if let Some(s) = v.unpack_str() {
                    vec![s.to_string()]
                } else {
                    Vec::new()
                };
                let mut structs: Vec<Value<'v>> = Vec::new();
                for label in &labels {
                    // Canonical label — bare in single-package mode, //pkg:name in a workspace.
                    let dep = canon_label(session(eval), label);
                    // E0: a forward-referenced local declaration analyzes on demand, first.
                    ensure_analyzed(eval, &dep)?;
                    let sess = session(eval);
                    let resolved = {
                        let results = sess.results.borrow();
                        results.get(&dep).map(|t| t.default_info.clone())
                    };
                    let files = match resolved {
                        Some(files) => files,
                        None => {
                            // Bazel file-label semantics (L2): a label naming no declared target
                            // resolves to a SOURCE FILE in the package when that file exists
                            // (`srcs = ["lib.rs"]`). Source files are not target deps.
                            let qualified = qualify(sess, label.trim_start_matches(':'));
                            let on_disk = sess
                                .workspace
                                .as_ref()
                                .is_some_and(|root| root.join(&qualified).is_file());
                            if on_disk {
                                let heap = eval.heap();
                                let f = heap.alloc(qualified);
                                structs.push(heap.alloc_complex_no_freeze(DepTarget {
                                    fields: vec![("files".to_string(), heap.alloc(vec![f]))],
                                    providers: Vec::new(),
                                }));
                                continue;
                            }
                            return Err(anyhow::anyhow!(
                                "`{dep}` is neither a declared target nor a source file in \
                                 this package"
                            )
                            .into());
                        }
                    };
                    let tkey = crate::dds::target_key(InstanceId::SINGLE, &dep)
                        .map_err(|e| anyhow::anyhow!(e))?;
                    // `files` is own-exposed (DefaultInfo); transitive fields fold via the ONE
                    // registry-driven helper over the Session's LIVE store (E0d — no rebuild).
                    let mut sfields: Vec<(String, Vec<String>)> =
                        vec![("files".to_string(), files)];
                    {
                        let dds = crate::dds::session_dds(sess);
                        sfields.extend(crate::dds::fold_dep_fields(&dds, &tkey));
                    }
                    // L2a: a dep is a DepTarget — plain projected fields by attr, plus the dep's
                    // returned provider instances indexable by constructor (`dep[MyInfo]`).
                    let heap = eval.heap();
                    let dfields: Vec<(String, Value<'v>)> =
                        sfields.into_iter().map(|(k, xs)| (k, heap.alloc(xs))).collect();
                    let providers = decl_store(eval)?
                        .captured
                        .borrow()
                        .get(&dep)
                        .cloned()
                        .unwrap_or_default();
                    dep_labels.push(dep);
                    structs.push(
                        heap.alloc_complex_no_freeze(DepTarget { fields: dfields, providers }),
                    );
                }
                // D1c: a single `attr.label` yields ONE struct; a list yields the list of structs.
                if single_label {
                    fields.push((key, structs.into_iter().next().unwrap_or_else(Value::new_none)));
                } else {
                    fields.push((key, eval.heap().alloc(structs)));
                }
            }
            _ => fields.push((key, v)),
        }
    }

    // D1: consult the declared attrs schema — fill omitted attrs from their `default`, error on a
    // missing `mandatory` one. (A2 discarded the schema; the real upstream rules require it.)
    if let Some(schema) = starlark::values::dict::DictRef::from_value(attrs) {
        let passed: std::collections::BTreeSet<&str> =
            kwargs.iter().map(|(k, _)| k.as_str()).collect();
        for (aname, descriptor) in schema.iter() {
            let Some(an) = aname.unpack_str() else { continue };
            if an == "name" || passed.contains(an) {
                continue;
            }
            let default =
                descriptor.get_attr("default", eval.heap())?.unwrap_or_else(Value::new_none);
            if !default.is_none() {
                fields.push((an.to_string(), default));
            } else if descriptor
                .get_attr("mandatory", eval.heap())?
                .and_then(|m| m.unpack_bool())
                .unwrap_or(false)
            {
                return Err(anyhow::anyhow!("mandatory attribute `{an}` not provided").into());
            } else {
                // Bazel TYPE defaults: an attr always exists on ctx.attr (real rules iterate
                // `ctx.attr.deps` unconditionally). Lists → [], dicts → {}, string → "",
                // int → 0, bool → False, label/output → None.
                let heap = eval.heap();
                let kind = descriptor
                    .get_attr("kind", heap)?
                    .and_then(|k| k.unpack_str().map(String::from))
                    .unwrap_or_default();
                let v = match kind.as_str() {
                    "label_list" | "string_list" | "output_list" => {
                        heap.alloc(Vec::<Value<'v>>::new())
                    }
                    "string_dict" | "string_list_dict" | "label_keyed_string_dict" => {
                        heap.alloc(starlark::values::dict::AllocDict::EMPTY)
                    }
                    "string" => heap.alloc(""),
                    "int" => heap.alloc(0),
                    "bool" => Value::new_bool(false),
                    _ => Value::new_none(), // label / output / unknown kinds
                };
                fields.push((an.to_string(), v));
            }
        }
    }

    let sess = session(eval);
    let heap = eval.heap();
    sess.state.borrow_mut().current = Some(AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: dep_labels,
            ..Default::default()
        });

    // ctx.outputs.<attr> — string-valued attrs are predeclared output filenames
    // (package-qualified). ctx.files.<attr> — list-valued attrs are source files
    // (qualified). ctx.executable is empty until an executable-label attr is wired.
    let mk_file = |s: &str| heap.alloc_complex_no_freeze(File { path: qualify(sess, s) });
    let outputs_fields: Vec<(String, Value<'v>)> = kwargs
        .iter()
        .filter_map(|(k, v)| v.unpack_str().map(|s| (k.clone(), mk_file(s))))
        .collect();
    let files_fields: Vec<(String, Value<'v>)> = kwargs
        .iter()
        .filter_map(|(k, v)| {
            ListRef::from_value(*v).map(|list| {
                let items: Vec<Value<'v>> = list
                    .iter()
                    .filter_map(|it| it.unpack_str().map(mk_file))
                    .collect();
                (k.clone(), heap.alloc(items))
            })
        })
        .collect();
    let ctx = heap.alloc_complex_no_freeze(Ctx {
            attr: heap.alloc(AllocStruct(fields)),
            actions: heap.alloc_complex_no_freeze(Actions),
            // `ctx.label` is a Label struct (`.package`/`.name`) — razel's cc:defs.bzl reads them
            // for the path model. package is the current pkg (empty in single-package mode).
            label: {
                let pkg = sess.current_pkg.borrow().clone().unwrap_or_default();
                heap.alloc(AllocStruct([
                    ("package".to_string(), heap.alloc(pkg)),
                    ("name".to_string(), heap.alloc(name.clone())),
                ]))
            },
            outputs: heap.alloc(AllocStruct(outputs_fields)),
            files: heap.alloc(AllocStruct(files_fields)),
            executable: heap.alloc(AllocStruct(Vec::<(String, Value<'v>)>::new())),
        });
    let ret = eval.eval_function(implementation, &[ctx], &[])?;

    // L2a: capture the provider instances the impl RETURNED (keyed by canonical label) — what a
    // dependent's `dep[MyInfo]` reads. DefaultInfo/razel_build.info side-effect channels unchanged.
    {
        let mut captured: Vec<(Value<'v>, Value<'v>)> = Vec::new();
        let mut grab = |item: Value<'v>| {
            if let Some(callable) = instance_callable(item) {
                captured.push((callable, item));
            }
        };
        if let Some(list) = ListRef::from_value(ret) {
            for item in list.iter() {
                grab(item);
            }
        } else {
            grab(ret);
        }
        if !captured.is_empty() {
            let label = canon_label(session(eval), &name);
            decl_store(eval)?.captured.borrow_mut().insert(label, captured);
        }
    }

    // Post-impl: commit the analyzed target via record_target (E0d: it also asserts into the
    // Session's live fact store). Take the in-flight target with a short borrow first.
    let committed = sess.state.borrow_mut().current.take();
    if let Some(c) = committed {
        record_target(sess, c);
    }
    Ok(())
}


/// A `provider()` value (D4.2/L2): a callable constructing provider instances. With `init=`
/// (Bazel's `CcInfo, _raw = provider(init = f)` shape) the kwargs route through `init` (which
/// returns the field dict) and `provider()` returns a 2-tuple `(Provider, raw_ctor)`. Generic
/// over `V` (the `RuleObjGen` pattern) so a `.bzl`-defined provider survives `module.freeze()`.
#[derive(Debug, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub(crate) struct ProviderCallableGen<V: ValueLifetimeless> {
    /// The `init` callback (Starlark `None` ⇒ kwargs are the fields directly).
    init: V,
}
starlark_complex_value!(ProviderCallable);

impl<V: ValueLifetimeless> fmt::Display for ProviderCallableGen<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<provider>")
    }
}

#[starlark_value(type = "provider")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for ProviderCallableGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    /// `MyInfo(field = value, …)` → a [`ProviderInstance`]. The instance remembers its
    /// constructor (`me`) — that identity is what `dep[MyInfo]` indexes by (L2a). With `init`,
    /// the kwargs go through it and its returned dict becomes the fields.
    fn invoke(
        &self,
        me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let named = args.names_map()?;
        let kwargs: Vec<(String, Value<'v>)> =
            named.iter().map(|(k, v)| (k.as_str().to_string(), *v)).collect();
        let pos: Vec<Value<'v>> = args.positions(eval.heap())?.collect();
        let init = self.init.to_value();
        let fields = if !init.is_none() {
            {
                // The call's args go to `init` VERBATIM (its signature is the contract —
                // positional args are legal; rules_cc calls `_ArtifactCategoryInfo("X", …)`).
                let named_refs: Vec<(&str, Value<'v>)> =
                    kwargs.iter().map(|(k, v)| (k.as_str(), *v)).collect();
                let dict = eval.eval_function(init, &pos, &named_refs)?;
                let Some(d) = starlark::values::dict::DictRef::from_value(dict) else {
                    return Err(anyhow::anyhow!(
                        "provider init callback must return a dict of fields"
                    )
                    .into());
                };
                d.iter()
                    .filter_map(|(k, v)| k.unpack_str().map(|k| (k.to_string(), v)))
                    .collect()
            }
        } else {
            if !pos.is_empty() {
                return Err(anyhow::anyhow!(
                    "providers take field values as keyword arguments (no init= declared)"
                )
                .into());
            }
            kwargs
        };
        Ok(eval.heap().alloc(ProviderInstance { callable: me, fields }))
    }
}

/// The raw constructor from `provider(init=…)` — builds instances of the SAME provider
/// (instances carry the CANONICAL provider's identity, so `dep[P]` finds raw-made ones),
/// bypassing `init`.
#[derive(Debug, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub(crate) struct RawCtorGen<V: ValueLifetimeless> {
    canonical: V,
}
starlark_complex_value!(RawCtor);

impl<V: ValueLifetimeless> fmt::Display for RawCtorGen<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<provider raw constructor>")
    }
}

#[starlark_value(type = "provider_raw_constructor")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for RawCtorGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let named = args.names_map()?;
        let fields: Vec<(String, Value<'v>)> =
            named.iter().map(|(k, v)| (k.as_str().to_string(), *v)).collect();
        Ok(eval
            .heap()
            .alloc(ProviderInstance { callable: self.canonical.to_value(), fields }))
    }
}


/// A constructed provider instance (L2a): the fields plus the constructor's identity. Field reads
/// (`info.msg`) go through `get_attr`; a rule impl returning instances gets them captured onto the
/// target, and a dependent retrieves them via `dep[MyInfo]` (identity = same constructor value).
/// Freeze-generic: real `.bzl` construct instances at MODULE level (rules_cc's artifact
/// categories), so instances must survive `module.freeze()`.
#[derive(Debug, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub(crate) struct ProviderInstanceGen<V: ValueLifetimeless> {
    callable: V,
    fields: Vec<(String, V)>,
}
starlark_complex_value!(ProviderInstance);

impl<V: ValueLifetimeless> fmt::Display for ProviderInstanceGen<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<provider instance>")
    }
}

#[starlark_value(type = "provider_instance")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for ProviderInstanceGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn get_attr(&self, attribute: &str, _heap: Heap<'v>) -> Option<Value<'v>> {
        self.fields.iter().find(|(k, _)| k == attribute).map(|(_, v)| v.to_value())
    }
}

/// The constructor identity of a provider-instance value (frozen or live), for `dep[P]` capture.
pub(crate) fn instance_callable<'v>(item: Value<'v>) -> Option<Value<'v>> {
    if let Some(pi) = item.downcast_ref::<ProviderInstance<'v>>() {
        Some(pi.callable)
    } else if let Some(pi) = item.downcast_ref::<FrozenProviderInstance>() {
        Some(pi.callable.to_value())
    } else {
        None
    }
}


/// A resolved dependency as seen by a rule impl (L2a): the plain projected fields (`files`,
/// `headers`, …) via `get_attr`, plus `dep[MyInfo]` indexing into the dep's captured provider
/// instances (constructor identity — `Value::ptr_eq`).
#[derive(Debug, NoSerialize, ProvidesStaticType, Allocative, Trace)]
pub(crate) struct DepTarget<'v> {
    fields: Vec<(String, Value<'v>)>,
    providers: Vec<(Value<'v>, Value<'v>)>,
}

impl fmt::Display for DepTarget<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<dep>")
    }
}

#[starlark_value(type = "dep_target")]
impl<'v> StarlarkValue<'v> for DepTarget<'v> {
    fn get_attr(&self, attribute: &str, _heap: Heap<'v>) -> Option<Value<'v>> {
        self.fields.iter().find(|(k, _)| k == attribute).map(|(_, v)| *v)
    }
    /// `dep[MyInfo]` — the instance this dep's rule returned for that provider.
    fn at(&self, index: Value<'v>, _heap: Heap<'v>) -> starlark::Result<Value<'v>> {
        self.providers
            .iter()
            .find(|(c, _)| c.ptr_eq(index))
            .map(|(_, inst)| *inst)
            .ok_or_else(|| {
                starlark::Error::new_other(anyhow::anyhow!(
                    "target does not provide the requested provider"
                ))
            })
    }
}


#[allow(non_snake_case)]
#[starlark::starlark_module]
pub(crate) fn rule_globals(b: &mut GlobalsBuilder) {
    /// `rule(implementation, attrs={})` → a callable rule object.
    fn rule<'v>(
        #[starlark(require = named)] implementation: Value<'v>,
        #[starlark(require = named)] attrs: Option<Value<'v>>,
        // D4: absorb the other rule() kwargs upstream rules pass (build_setting, doc, cfg, toolchains,
        // provides, executable, test, …). Not yet honored — enough to define the rule.
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // D1: keep the declared schema (was discarded) — instantiation consults it for defaults +
        // mandatory. alloc (freezable) so the rule survives module.freeze() (defined in a .bzl + load()ed).
        let attrs = attrs.unwrap_or_else(Value::new_none);
        Ok(eval.heap().alloc(RuleObjGen { implementation, attrs }))
    }

    /// `provider(doc=?, fields=?, init=?)` → a callable provider constructor (D4.2). With `init`
    /// it returns Bazel's 2-tuple `(Provider, raw_ctor)` (the `CcInfo, _raw = provider(...)`
    /// shape — rules_cc). Field-name validation not yet enforced.
    fn provider<'v>(
        #[starlark(args)] _args: UnpackTuple<Value<'v>>,
        #[starlark(kwargs)] kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let heap = eval.heap();
        let init = kw.get("init").copied().unwrap_or_else(Value::new_none);
        let main = heap.alloc(ProviderCallable { init });
        Ok(if init.is_none() {
            main
        } else {
            // Bazel: with init, provider() returns (Provider, raw_ctor).
            heap.alloc((main, heap.alloc(RawCtor { canonical: main })))
        })
    }

    /// Bazel builtin global providers `RunEnvironmentInfo(...)` / `OutputGroupInfo(...)` (D4 compat):
    /// construct an instance `struct` from the kwargs. Stubs so real upstream rules resolve + run; the
    /// instances aren't yet captured/consumed (D4.3+).
    fn RunEnvironmentInfo<'v>(
        #[starlark(kwargs)] kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let fields: Vec<(String, Value<'v>)> =
            kw.iter().map(|(k, v)| (k.clone(), *v)).collect();
        Ok(eval.heap().alloc(AllocStruct(fields)))
    }
    fn OutputGroupInfo<'v>(
        #[starlark(kwargs)] kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let fields: Vec<(String, Value<'v>)> =
            kw.iter().map(|(k, v)| (k.clone(), *v)).collect();
        Ok(eval.heap().alloc(AllocStruct(fields)))
    }

    /// Builtin global provider stubs real rulesets reference (PackageSpecificationInfo:
    /// package_group's provider; RunEnvironmentInfo/OutputGroupInfo are constructed). Globals
    /// referenced-but-not-modeled resolve to constructors that absorb.
    fn PackageSpecificationInfo<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// `repository_rule(implementation, ...)` (compat stub): repo rules DEFINE at load; razel
    /// never fetches (vendored/host repos instead) — invoking one surfaces at analysis. L6/L7.
    fn repository_rule<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(Value::new_none())
    }

    /// `module_extension(implementation, ...)` (compat stub) — bzlmod machinery, same posture.
    fn module_extension<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(Value::new_none())
    }

    /// `tag_class(...)` (compat stub) — bzlmod machinery.
    fn tag_class<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(Value::new_none())
    }

    /// `subrule(implementation, ...)` (compat stub): Bazel's subrule mechanism, absorbed —
    /// rules defining subrules load; invoking one surfaces at analysis (registered debt).
    fn subrule<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(Value::new_none())
    }

    /// `transition(implementation, inputs, outputs)` (D4 compat stub): real rules define config
    /// transitions + pass them as `rule(cfg=…)`. razel doesn't apply transitions yet — absorb the args
    /// so the rule defines; `rule()` already absorbs the `cfg` kwarg.
    fn transition<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// `visibility(...)` — .bzl load-visibility declaration (compat stub: not enforced).
    fn visibility<'v>(
        #[starlark(args)] _a: UnpackTuple<Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// `licenses(...)` — legacy license declaration (compat stub).
    fn licenses<'v>(#[starlark(args)] _a: UnpackTuple<Value<'v>>) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// `exports_files(...)` — source files are package-visible in razel already (compat stub).
    fn exports_files<'v>(
        #[starlark(args)] _a: UnpackTuple<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// `package_group(...)` — visibility grouping (compat stub: visibility not enforced).
    fn package_group<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// `aspect(implementation, ...)` (compat stub): aspects are not modeled (TF uses 10); rules
    /// DEFINING aspects load; applying them is registered debt (L6).
    fn aspect<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// `exec_group(toolchains=?, exec_compatible_with=?)` (compat stub): execution groups are not
    /// modeled (single execution platform); absorb so real rules define.
    fn exec_group<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// `configuration_field(fragment, name)` (D4 compat stub): a late-bound default razel doesn't model
    /// — absorb the args; the attr's default becomes `None`.
    fn configuration_field<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// `Label("//pkg:name")` — a minimal Label exposing `.package`/`.name`/
    /// `.workspace_root`/`.workspace_name`. razel treats everything as the main repo,
    /// so workspace_root/workspace_name are empty (matching Bazel on the main repo).
    fn Label<'v>(
        #[starlark(require = pos)] s: String,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let after = s.rsplit_once("//").map(|(_, a)| a).unwrap_or(s.as_str());
        let (pkg, name) = match after.split_once(':') {
            Some((p, n)) => (p.to_string(), n.to_string()),
            None => (
                after.to_string(),
                after.rsplit('/').next().unwrap_or(after).to_string(),
            ),
        };
        let heap = eval.heap();
        Ok(heap.alloc(AllocStruct([
            ("package".to_string(), heap.alloc(pkg)),
            ("name".to_string(), heap.alloc(name)),
            ("workspace_root".to_string(), heap.alloc(String::new())),
            ("workspace_name".to_string(), heap.alloc(String::new())),
        ])))
    }

    /// BUILD package-declaration builtins. razel doesn't enforce visibility/licenses
    /// and tracks no separate file-export set, so these are no-op declarations —
    /// recognized so real BUILD files evaluate. (`package`, `package_group`,
    /// `licenses`, `exports_files`.)
    fn package<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn package_group<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn licenses<'v>(
        #[starlark(require = pos)] _licenses: UnpackList<String>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn exports_files<'v>(
        #[starlark(require = pos)] _files: UnpackList<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// Native build-graph builtins razel recognizes so real BUILD files evaluate.
    /// `config_setting`/`test_suite`/`alias` carry no buildable output here, so they
    /// record an empty target under their label (analysis-visible, builds to nothing);
    /// `filegroup` forwards its `srcs` as its outputs so dependents resolve them.
    /// `genrule(name, srcs, outs, cmd)` — Bazel's generic shell rule (razelV3): ONE bash action
    /// running `cmd` after Make-variable expansion (`$@`/`$<`/`$(SRCS)`/`$(OUTS)`/`$(location)`;
    /// `$$` escapes). A src that is a label resolves to its files (demand-driven, E0c-deferred).
    /// Unmodeled variables (`$(RULEDIR)`, tools=…) error loudly — registered debt, not silence.
    fn genrule<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(require = named)] outs: UnpackList<String>,
        #[starlark(require = named)] cmd: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let label = canon_label(session(eval), &name);
        record_native(eval, label, crate::state::native_decl(move |eval| {
            let sess = session(eval);
            // Split srcs: labels resolve to their files (their package loads/analyzes on
            // demand); plain names are this package's files. `loc` keys keep the as-written
            // form for `$(location X)`.
            let (mut inputs, mut deps) = (Vec::new(), Vec::new());
            let mut loc: Vec<(String, Vec<String>)> = Vec::new();
            for s in unpack(srcs) {
                if s.starts_with(':') || s.starts_with("//") {
                    let dep = crate::deps::resolve_dep(eval, &s)?;
                    loc.push((s.clone(), dep.libs.clone()));
                    inputs.extend(dep.libs);
                    deps.push(dep.canon);
                } else {
                    let q = qualify(sess, &s);
                    loc.push((s, vec![q.clone()]));
                    inputs.push(q);
                }
            }
            let outs: Vec<String> = outs.items.iter().map(|o| qualify(sess, o)).collect();
            let expanded = expand_genrule_cmd(&cmd, &inputs, &outs, &loc)?;
            record_target(sess, AnalyzedTarget {
                name: canon_label(sess, &name),
                deps,
                actions: vec![crate::state::AnalyzedAction {
                    mnemonic: "Genrule".into(),
                    argv: vec!["/bin/bash".into(), "-c".into(), expanded],
                    inputs,
                    outputs: outs.clone(),
                }],
                default_info: outs,
                ..Default::default()
            });
            Ok(())
        }))?;
        Ok(NoneType)
    }

    /// `config_setting(name, values=?, define_values=?)` — declare a constraint spec `select()`
    /// matches against the structured configuration (razelV3: real resolution, not a placeholder).
    fn config_setting<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] values: Option<SmallMap<String, String>>,
        #[starlark(require = named)] define_values: Option<SmallMap<String, String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let spec = crate::state::ConfigSpec {
            values: values.map(|m| m.into_iter().collect()).unwrap_or_default(),
            define_values: define_values.map(|m| m.into_iter().collect()).unwrap_or_default(),
        };
        sess.config_specs.borrow_mut().insert(canon_label(sess, &name), spec);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }
    fn test_suite<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }
    fn alias<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }
    fn filegroup<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let files: Vec<String> = unpack(srcs).iter().map(|s| qualify(sess, s)).collect();
        // C3a.5: filegroup provides DefaultInfo only — it no longer fakes CcInfo to push files into
        // the cc header channel (that hack was untested; a real cc-on-filegroup case = cc reading dep
        // DefaultInfo files as inputs, Phase D).
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            default_info: files,
            ..Default::default()
        });
        Ok(NoneType)
    }

    /// `DefaultInfo(files=…)` — the standard output provider. `files` may be a list
    /// (of Files/strings) or a `depset`; other kwargs absorbed.
    fn DefaultInfo<'v>(
        #[starlark(require = named)] files: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        if let Some(f) = files {
            let paths = extract_files(f);
            with_current(session(eval), |c| c.default_info = paths);
        }
        Ok(NoneType)
    }

    // C3 (`razel_build.info`): the cc/java provider-capture builtins are gone — razel's `.bzl` rules
    // capture providers through the ONE generic `razel_build.info(provider, fields)` constructor
    // (engine.rs), schema-driven by the registry. No language-named capture builtin in the rule API.

    /// `dedup(list)` — the list with duplicate strings removed, **preserving first occurrence**. The
    /// cross-sibling dedup the rule()-path classpath/header assembly needs: a target's transitive
    /// closure folds *per-dep*, so a diamond (`app -> [x,y] -> base`) would otherwise list base's
    /// jar/header twice. `dedup()` makes the merge the engine's job, not silent duplication (F1).
    fn dedup(#[starlark(require = pos)] list: UnpackList<String>) -> anyhow::Result<Vec<String>> {
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for s in list.items {
            if seen.insert(s.clone()) {
                out.push(s);
            }
        }
        Ok(out)
    }

    /// `depset(direct=[], transitive=[depsets], order=…)` — Bazel's transitive set.
    /// razel folds member paths from `direct` + each `transitive` depset, deduped.
    fn depset<'v>(
        #[starlark(require = pos)] direct: Option<Value<'v>>,
        #[starlark(require = named)] transitive: Option<Value<'v>>,
        #[starlark(require = named)] order: Option<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // razel does not model depset traversal order yet (the 4-order family is reserved). WARN
        // loudly on a non-default order rather than silently produce a possibly-wrong sequence (F36).
        if let Some(o) = &order
            && o != "default"
        {
            eprintln!(
                "razel: warning: depset(order={o:?}) — traversal order not yet modeled, treating as \
                 default (F36; RazelGaps)"
            );
        }
        // Dedup by string path; store the live Value so map_each sees File attributes.
        let mut seen: Vec<String> = Vec::new();
        let mut items: Vec<Value<'v>> = Vec::new();
        let push = |v: Value<'v>, seen: &mut Vec<String>, items: &mut Vec<Value<'v>>| {
            let key = file_path(v);
            if !seen.contains(&key) {
                seen.push(key);
                items.push(v);
            }
        };
        if let Some(d) = direct
            && let Some(list) = ListRef::from_value(d)
        {
            for it in list.iter() {
                push(it, &mut seen, &mut items);
            }
        }
        if let Some(t) = transitive
            && let Some(list) = ListRef::from_value(t)
        {
            for dep in list.iter() {
                if let Some(ds) = dep.downcast_ref::<Depset>() {
                    for v in &ds.items {
                        push(*v, &mut seen, &mut items);
                    }
                }
            }
        }
        Ok(eval.heap().alloc_complex_no_freeze(Depset { items }))
    }

    /// `select({condition: value, …})` — Bazel semantics (razelV3): HYBRID resolution. If every
    /// condition is a declared `config_setting` right now (loading its package on demand), the
    /// branch resolves eagerly (most-specialized wins; `//conditions:default` fallback; loud
    /// no-match/ambiguity errors). If any condition is not yet declared (real `.bzl` build
    /// module-level selects — XLA's tsl.bzl), select returns a DEFERRED value, resolved when an
    /// attr consumes it at analysis (by which time the conditions exist — the E0 split).
    /// Keys may be strings or `Label`s; `select + list` concatenation is supported (SelectExpr).
    fn select<'v>(
        branches: Value<'v>,
        #[starlark(require = named)] _no_match_error: Option<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let Some(d) = starlark::values::dict::DictRef::from_value(branches) else {
            return Err(anyhow::anyhow!("select() takes a dict of conditions"));
        };
        let pairs: Vec<(Value<'v>, Value<'v>)> = d.iter().collect();
        drop(d);
        match pick_branch(eval, &pairs, true)? {
            Some(v) => Ok(v),
            // Defer: a condition isn't declared yet — resolve at attr consumption (analysis).
            None => Ok(eval.heap().alloc(SelectBranches { branches: pairs })),
        }
    }

    /// `define_config(name, compile, archive=None, link=None)` — declare + register a
    /// toolchain transform (D7). Returns a struct of the transform fns (so a rule can call
    /// `cfg.compile(req)`); also records the name engine-side for host-config selection.
    fn define_config<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] compile: Value<'v>,
        #[starlark(require = named)] archive: Option<Value<'v>>,
        #[starlark(require = named)] link: Option<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        session(eval).configs.borrow_mut().push(name);
        let mut fields: Vec<(String, Value<'v>)> = vec![("compile".to_string(), compile)];
        if let Some(a) = archive {
            fields.push(("archive".to_string(), a));
        }
        if let Some(l) = link {
            fields.push(("link".to_string(), l));
        }
        Ok(eval.heap().alloc(AllocStruct(fields)))
    }

    /// `glob(include, exclude=[])` — match the current package's files (workspace
    /// mode) against the patterns, returning package-relative paths. Requires a
    /// package on disk; errors in single-package (no-dir) mode.
    fn glob<'v>(
        #[starlark(require = pos)] include: UnpackList<String>,
        #[starlark(require = named)] exclude: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Vec<String>> {
        do_glob(session(eval), include.items, exclude.map(|l| l.items).unwrap_or_default())
    }
}

