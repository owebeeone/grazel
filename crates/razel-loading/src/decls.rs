//! E0 declaration store + demand-driven analysis (record → drive/ensure → harvest).

use crate::state::{AnalyzedTarget, canon_label, qualify, session};
use crate::values::{Actions, File};
use crate::deps::record_target;
use razel_dds::InstanceId;
use allocative::Allocative;
use starlark::any::ProvidesStaticType;
use starlark::coerce::Coerce;
use starlark::collections::SmallMap;
use starlark::environment::Module;
use starlark::eval::{Arguments, Evaluator};
use starlark::starlark_complex_value;
use starlark::values::list::ListRef;
use starlark::values::structs::AllocStruct;
use starlark::values::{
    Freeze, NoSerialize, StarlarkValue, Trace, Value, ValueLifetimeless, ValueLike,
    starlark_value,
};
use std::cell::RefCell;
use std::fmt;
use crate::ctxv::{Ctx, toolchain_map};
use crate::labels::LabelV;
use crate::provider_values::{AspectObj, DepTarget, FrozenAspectObj, instance_callable};
use crate::selects::resolve_attr_value;


// ---- rule() + DefaultInfo + select ----------------------------------------------


/// A `rule()` value. Generic over `V` so it has both an unfrozen form (`RuleObj<'v>`,
/// holding a live `Value`) and a frozen form (`FrozenRuleObj`, holding a `FrozenValue`)
/// — which is what lets a rule **survive `module.freeze()`** and therefore be defined
/// in a `.bzl` and `load()`ed, not just inline. The impl function freezes with it.
#[derive(Debug, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub(crate) struct RuleObjGen<V: ValueLifetimeless> {
    pub(crate) implementation: V,
    /// The declared `attrs` schema (name → attr descriptor), frozen with the rule and consulted at
    /// instantiation for defaults / `mandatory` (D1). `None` when no schema was declared.
    pub(crate) attrs: V,
    /// `rule(outputs = {"attr": "%{name}.ext"})` — implicit-output templates; expanded into
    /// `ctx.outputs.<attr>` Files at instantiation. `None` when undeclared.
    pub(crate) outputs: V,
}


starlark_complex_value!(pub(crate) RuleObj);


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
        // Output-file labels resolve statically (Bazel): register `attr.output`/`output_list`
        // values in the output index at DECLARE time, like genrule outs.
        if let Ok((_, attrs, _)) = rule_parts(me)
            && let Some(schema) = starlark::values::dict::DictRef::from_value(attrs)
        {
            for (k, v) in &kwargs {
                let kind = schema
                    .iter()
                    .find(|(kk, _)| kk.unpack_str() == Some(k.as_str()))
                    .and_then(|(_, d)| d.get_attr("kind", eval.heap()).ok().flatten())
                    .and_then(|x| x.unpack_str().map(String::from))
                    .unwrap_or_default();
                if kind == "output" || kind == "output_list" {
                    let outs: Vec<String> = match v.unpack_str() {
                        Some(s) => vec![s.to_string()],
                        None => crate::values::unpack_strs_any(Some(*v)),
                    };
                    let mut idx = sess.output_index.borrow_mut();
                    for o in outs {
                        idx.insert(canon_label(sess, &o), (label.clone(), qualify(sess, &o)));
                    }
                }
            }
        }
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


// ---- E0: the phase split — declaration store + demand-driven analysis -------------------------


/// The module variable carrying the harvested provider captures across `module.freeze()` —
/// a plain dict {canonical label: [(constructor, instance)]} (builtin containers freeze natively;
/// the instances are freeze-generic).
pub(crate) const CAPTURED_VAR: &str = "__razel_captured";


/// Harvested UNDRIVEN Starlark declarations of a completed package:
/// {canonical label: (pkg, rule, [(kwarg, value)…])} — analyzed on demand cross-package.
pub(crate) const DEFERRED_VAR: &str = "__razel_deferred_decls";


/// Layer 0 pre-freeze step: copy the decl store's captured map into [`CAPTURED_VAR`] as plain
/// dict/list/tuples, then unroot the (unfreezable, already-drained) decl store.
pub(crate) fn stash_captured_for_freeze<'v>(
    module: &Module<'v>,
    sess: &crate::state::Session,
) -> anyhow::Result<()> {
    let heap = module.heap();
    let Some(storev) = module.get(DECLS_VAR) else { return Ok(()) };
    let Some(store) = storev.downcast_ref::<DeclStore<'v>>() else { return Ok(()) };
    let captured = store.captured.borrow();
    let entries: Vec<(Value<'v>, Value<'v>)> = captured
        .iter()
        .map(|(label, pairs)| {
            let items: Vec<Value<'v>> =
                pairs.iter().map(|(c, i)| heap.alloc((*c, *i))).collect();
            (heap.alloc(label.as_str()), heap.alloc(items))
        })
        .collect();
    drop(captured);
    module.set(CAPTURED_VAR, heap.alloc(starlark::values::dict::AllocDict(entries)));
    // Undriven Starlark decls (dependency-loaded packages defer them): harvest as
    // {label: (pkg, rule, [(k, v)…])} so a later demand analyzes them in the consumer's eval.
    let pkg = sess.current_pkg().unwrap_or_default();
    let mut deferred: Vec<(Value<'v>, Value<'v>)> = Vec::new();
    for slot in store.decls.borrow_mut().iter_mut() {
        if let Some(d) = slot.take() {
            sess.pending.borrow_mut().remove(&d.label);
            match d.body {
                DeclBody::Rule { rule, kwargs } => {
                    let kw: Vec<Value<'v>> = kwargs
                        .iter()
                        .map(|(k, v)| heap.alloc((heap.alloc(k.as_str()), *v)))
                        .collect();
                    let tup = heap.alloc((heap.alloc(pkg.as_str()), rule, heap.alloc(kw)));
                    deferred.push((heap.alloc(d.label.as_str()), tup));
                }
                // Undriven natives: bodies are Session-side plain-data closures — demandable
                // from any later eval.
                DeclBody::Native(nidx) => {
                    sess.deferred_natives.borrow_mut().insert(d.label, nidx);
                }
            }
        }
    }
    module.set(DEFERRED_VAR, heap.alloc(starlark::values::dict::AllocDict(deferred)));
    module.set(DECLS_VAR, Value::new_none());
    Ok(())
}


/// Layer 0 lookup: a completed package's harvested instances for `label`, re-viewed in the
/// consumer's eval (sound: the Session's OwnedFrozenValues keep the source heaps alive).
pub(crate) fn cross_providers_for<'v>(
    eval: &Evaluator<'v, '_, '_>,
    label: &str,
) -> Vec<(Value<'v>, Value<'v>)> {
    let sess = session(eval);
    // `owned_value(frozen_heap)`: the CONSUMER module's frozen heap takes a reference to the
    // source heap, so the returned values stay alive as long as the consumer module — the
    // sound cross-heap pattern (buck2's). O(1) via the harvest index.
    let owners: Vec<starlark::values::OwnedFrozenValue> =
        match sess.cross_index.borrow().get(label) {
            Some(&i) => vec![sess.cross_captured.borrow()[i].clone()],
            None => return Vec::new(),
        };
    for owned in &owners {
        // SAFETY: `owned_frozen_value` registers the source heap on the CONSUMER module's
        // frozen heap, which outlives every `'v` value of that module — the values stay live
        // for at least `'v` (the safe `owned_value` wrapper merely picks the shorter `'a`).
        let fv = unsafe { owned.owned_frozen_value(eval.frozen_heap()) };
        let dictv: Value<'v> = fv.to_value();
        let Some(d) = starlark::values::dict::DictRef::from_value(dictv) else { continue };
        for (k, vlist) in d.iter() {
            if k.unpack_str() == Some(label) {
                let mut out = Vec::new();
                if let Some(l) = ListRef::from_value(vlist) {
                    for item in l.iter() {
                        if let Some(t) = starlark::values::tuple::TupleRef::from_value(item) {
                            let xs: Vec<Value<'v>> = t.iter().collect();
                            if xs.len() == 2 {
                                out.push((xs[0], xs[1]));
                            }
                        }
                    }
                }
                return out;
            }
        }
    }
    Vec::new()
}


/// The module variable holding the package's [`DeclStore`] — installed by the analysis entry
/// points before BUILD eval; not addressable from Starlark source.
pub(crate) const DECLS_VAR: &str = "__razel_decls";


/// One recorded rule instantiation, analyzed on demand.
#[derive(Debug, Allocative, Trace)]
pub(crate) struct Decl<'v> {
    label: String,
    body: DeclBody<'v>,
}


/// What analyzing a declaration means: run a Starlark rule (value + raw kwargs) or a deferred
/// native body (an index into `Session.native_decls` — the closure lives off-heap, E0c).
#[derive(Debug, Allocative, Trace)]
pub(crate) enum DeclBody<'v> {
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
pub(crate) fn decl_store<'v>(eval: &Evaluator<'v, '_, '_>) -> anyhow::Result<&'v DeclStore<'v>> {
    let v = eval.module().get(DECLS_VAR).ok_or_else(|| {
        anyhow::anyhow!("rule instantiation outside a package analysis (no declaration store)")
    })?;
    v.downcast_ref::<DeclStore>()
        .ok_or_else(|| anyhow::anyhow!("declaration store has the wrong type"))
}


/// A rule value's (implementation, attrs-schema, outputs-templates), frozen or not.
pub(crate) fn rule_parts<'v>(
    rule: Value<'v>,
) -> anyhow::Result<(Value<'v>, Value<'v>, Value<'v>)> {
    if let Some(r) = rule.downcast_ref::<RuleObj<'v>>() {
        Ok((r.implementation, r.attrs, r.outputs))
    } else if let Some(r) = rule.downcast_ref::<FrozenRuleObj>() {
        Ok((r.implementation.to_value(), r.attrs.to_value(), r.outputs.to_value()))
    } else {
        Err(anyhow::anyhow!("declaration's rule is not a rule value"))
    }
}


/// Phase 2: analyze every recorded declaration, in declaration order (demand-recursion may pull a
/// forward-referenced one earlier; its slot is then empty when the loop reaches it).
pub(crate) fn drive_decls<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    drive_all: bool,
) -> starlark::Result<()> {
    let mut i = 0;
    loop {
        let n = decl_store(eval)?.decls.borrow().len();
        if i >= n {
            return Ok(());
        }
        // Bazel analyzes only DEMANDED targets: a dependency-loaded package defers ALL decls —
        // Starlark ones to the harvest, native ones to `deferred_natives` (their bodies are
        // Session-side and run in any later eval). Driving natives eagerly manufactured false
        // cycles (a genrule's `tools=[//:protoc]` resolving while protoc is mid-analysis).
        if drive_all {
            analyze_decl(eval, i)?;
        }
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
        if sess.analyzing_contains(label) {
            return Err(anyhow::anyhow!("dependency cycle detected at `{label}`").into());
        }
        sess.pending.borrow().get(label).copied()
    };
    // A pending index is only meaningful against the CURRENT module's store (cross-package
    // demand runs in the consumer's eval): verify the slot's label before driving it.
    let local_pending = |eval: &mut Evaluator<'v, '_, '_>, i: usize| -> bool {
        decl_store(eval)
            .ok()
            .map(|st| {
                st.decls
                    .borrow()
                    .get(i)
                    .and_then(|s| s.as_ref())
                    .is_some_and(|d| d.label == label)
            })
            .unwrap_or(false)
    };
    if let Some(i) = idx
        && local_pending(eval, i)
    {
        return analyze_decl(eval, i);
    }
    // Cross-package: load the label's package (a failed load SURFACES), then the target is
    // either locally pending again (entry semantics), harvested-deferred, or genuinely absent.
    {
        let sess = session(eval);
        let mut load_err = None;
        if !sess.results.borrow().contains_key(label)
            && sess.workspace.is_some()
            && let Some(pkg) = crate::state::pkg_of(label)
        {
            load_err = crate::rules::load_package(sess, &pkg).err();
        }
        if sess.results.borrow().contains_key(label) {
            return Ok(());
        }
        let idx = sess.pending.borrow().get(label).copied();
        if let Some(i) = idx
            && local_pending(eval, i)
        {
            return analyze_decl(eval, i);
        }
        if let Some(e) = load_err {
            return Err(anyhow::anyhow!("loading `{label}`'s package failed: {e}").into());
        }
    }
    if let Some(f) = {
        let sess = session(eval);
        let nidx = sess.deferred_natives.borrow().get(label).copied();
        nidx.and_then(|i| sess.native_decls.borrow_mut()[i].take())
    } {
        return run_native_deferred(eval, label, f);
    }
    analyze_deferred(eval, label)
}


/// Run a deferred NATIVE body on demand (cycle-guarded, in the decl's package context).
fn run_native_deferred<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    label: &str,
    f: crate::state::NativeAnalyzeFn,
) -> starlark::Result<()> {
    let sess = session(eval);
    if !sess.analyzing_insert(label) {
        return Err(anyhow::anyhow!("dependency cycle detected at `{label}`").into());
    }
    let prev = match crate::state::pkg_of(label) {
        Some(p) => sess.set_current_pkg(Some(p)),
        None => sess.current_pkg(),
    };
    let res = f(eval).map_err(Into::into);
    session(eval).set_current_pkg(prev);
    session(eval).analyzing_remove(label);
    res
}


/// Analyze a HARVESTED declaration (a Starlark target of an already-completed package) on
/// demand, in the CONSUMER's eval — the cross-package demand-analysis leg. No-op if `label`
/// isn't harvested (the caller's existing error paths apply).
pub(crate) fn analyze_deferred<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    label: &str,
) -> starlark::Result<()> {
    let found = find_deferred(eval, label);
    let sess = session(eval);
    let Some((pkg, rule, kwargs)) = found else { return Ok(()) };
    if !sess.analyzing_insert(label) {
        return Err(anyhow::anyhow!("dependency cycle detected at `{label}`").into());
    }
    // Analyze in the DECL's package context (labels/paths qualify to the origin package).
    let prev = sess.set_current_pkg(Some(pkg));
    let res = analyze_rule_decl(eval, rule, &kwargs);
    session(eval).set_current_pkg(prev);
    session(eval).analyzing_remove(label);
    res
}


/// Scan the Session's harvested deferred decls for `label` → (pkg, rule, kwargs), re-viewed in
/// the consumer's eval. Also the aspect machinery's source of a target's ORIGINAL attrs.
pub(crate) fn find_deferred<'v>(
    eval: &Evaluator<'v, '_, '_>,
    label: &str,
) -> Option<(String, Value<'v>, Vec<(String, Value<'v>)>)> {
    let sess = session(eval);
    // O(1): the harvest index names the owning package's dict; scan only that one.
    let owners: Vec<starlark::values::OwnedFrozenValue> =
        match sess.deferred_index.borrow().get(label) {
            Some(&i) => vec![sess.deferred_decls.borrow()[i].clone()],
            None => return None,
        };
    let mut found: Option<(String, Value<'v>, Vec<(String, Value<'v>)>)> = None;
    'outer: for owned in &owners {
        // SAFETY: as in cross_providers_for — the consumer module's frozen heap keeps the
        // source heap alive for ≥ 'v.
        let fv = unsafe { owned.owned_frozen_value(eval.frozen_heap()) };
        let dictv: Value<'v> = fv.to_value();
        let Some(d) = starlark::values::dict::DictRef::from_value(dictv) else { continue };
        for (k, tup) in d.iter() {
            if k.unpack_str() == Some(label) {
                let Some(t) = starlark::values::tuple::TupleRef::from_value(tup) else {
                    continue;
                };
                let xs: Vec<Value<'v>> = t.iter().collect();
                if xs.len() != 3 {
                    continue;
                }
                let pkg = xs[0].unpack_str().unwrap_or_default().to_string();
                let mut kwargs = Vec::new();
                if let Some(l) = ListRef::from_value(xs[2]) {
                    for item in l.iter() {
                        if let Some(p) = starlark::values::tuple::TupleRef::from_value(item) {
                            let kv: Vec<Value<'v>> = p.iter().collect();
                            if kv.len() == 2
                                && let Some(key) = kv[0].unpack_str()
                            {
                                kwargs.push((key.to_string(), kv[1]));
                            }
                        }
                    }
                }
                found = Some((pkg, xs[1], kwargs));
                break 'outer;
            }
        }
    }
    found
}


/// Analyze the declaration at `idx` (no-op if its slot was already taken). Cycle-guarded via
/// `Session.analyzing`.
pub(crate) fn analyze_decl<'v>(eval: &mut Evaluator<'v, '_, '_>, idx: usize) -> starlark::Result<()> {
    let decl = { decl_store(eval)?.decls.borrow_mut()[idx].take() };
    let Some(decl) = decl else { return Ok(()) };
    {
        let sess = session(eval);
        if !sess.analyzing_insert(&decl.label) {
            return Err(
                anyhow::anyhow!("dependency cycle detected at `{}`", decl.label).into()
            );
        }
        sess.pending.borrow_mut().remove(&decl.label);
    }
    // Analyze in the DECL's package context — a cross-package demand chain can re-enter this
    // store while current_pkg points at ANOTHER package (compiler→root→compiler), and string
    // attrs (`:dep`) must canonicalize against the decl's origin.
    let origin = crate::state::pkg_of(&decl.label);
    let prev = match origin {
        Some(p) => session(eval).set_current_pkg(Some(p)),
        None => session(eval).current_pkg(),
    };
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
    session(eval).set_current_pkg(prev);
    session(eval).analyzing_remove(&decl.label);
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


/// Apply an aspect to an analyzed dep (L5 MVP): run its impl in THIS eval with
/// `target` = the dep's current providers and `ctx.rule.attr` = the dep's ORIGINAL attrs (deps
/// resolved recursively WITH the aspect — `attr_aspects` propagation). Results memoize in the
/// consumer store's captured map under `aspect::<label>`; returned pairs extend the DepTarget.
pub(crate) fn apply_aspect<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    aspectv: Value<'v>,
    label: &str,
    providers: &mut Vec<(Value<'v>, Value<'v>)>,
) -> starlark::Result<()> {
    let (implementation, asp_attrs) = {
        if let Some(a) = aspectv.downcast_ref::<AspectObj<'v>>() {
            (a.implementation, a.attrs)
        } else if let Some(a) = aspectv.downcast_ref::<FrozenAspectObj>() {
            (a.implementation.to_value(), a.attrs.to_value())
        } else {
            return Ok(()); // not an aspect value (absorbed/None) — nothing to apply
        }
    };
    let memo_key = format!("aspect::{label}");
    if let Some(pairs) = decl_store(eval)?.captured.borrow().get(&memo_key) {
        providers.extend(pairs.iter().copied());
        return Ok(());
    }
    let sess = session(eval);
    if !sess.analyzing_insert(&memo_key) {
        return Ok(()); // cycle: the aspect is being computed up-stack
    }
    let res = apply_aspect_uncached(eval, implementation, asp_attrs, aspectv, label, providers);
    session(eval).analyzing_remove(&memo_key);
    res
}

fn apply_aspect_uncached<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    implementation: Value<'v>,
    asp_attrs: Value<'v>,
    aspectv: Value<'v>,
    label: &str,
    providers: &mut Vec<(Value<'v>, Value<'v>)>,
) -> starlark::Result<()> {
    let heap = eval.heap();
    // The dep's ORIGINAL attrs (the harvested decl): rule.attr for the aspect impl. `deps`
    // resolve recursively WITH this aspect (attr_aspects propagation); other kwargs stay raw.
    let deferred = find_deferred(eval, label);
    // Aspect work runs in the TARGET's package context (declare_file/labels qualify there).
    let origin = crate::state::pkg_of(label);
    let prev = match origin {
        Some(p) => session(eval).set_current_pkg(Some(p)),
        None => session(eval).current_pkg(),
    };
    let result = (|| -> starlark::Result<()> {
        let mut rule_fields: Vec<(String, Value<'v>)> = Vec::new();
        if let Some((_pkg, _rule, kwargs)) = &deferred {
            for (k, v) in kwargs {
                if k == "deps" {
                    let mut sub = Vec::new();
                    let aspect_list = heap.alloc(vec![aspectv]);
                    let vv = resolve_attr_value(eval, *v)?;
                    let resolved =
                        resolve_label_attr_inner(eval, vv, false, &mut sub, aspect_list)?;
                    rule_fields.push((k.clone(), resolved));
                } else {
                    rule_fields.push((k.clone(), *v));
                }
            }
        }
        if !rule_fields.iter().any(|(k, _)| k == "deps") {
            rule_fields.push(("deps".to_string(), heap.alloc(Vec::<Value<'v>>::new())));
        }
        // The aspect's OWN attrs (implicit label defaults resolve like rule-schema defaults).
        let mut own_fields: Vec<(String, Value<'v>)> = Vec::new();
        if let Some(d) = starlark::values::dict::DictRef::from_value(asp_attrs) {
            let entries: Vec<(String, Value<'v>)> = d
                .iter()
                .filter_map(|(k, desc)| k.unpack_str().map(|k| (k.to_string(), desc)))
                .collect();
            drop(d);
            for (an, desc) in entries {
                let default =
                    desc.get_attr("default", heap)?.unwrap_or_else(Value::new_none);
                let kind = desc
                    .get_attr("kind", heap)?
                    .and_then(|k| k.unpack_str().map(String::from))
                    .unwrap_or_default();
                let v = if !default.is_none() && (kind == "label" || kind == "label_list") {
                    let default = resolve_attr_value(eval, default)?;
                    let mut sub = Vec::new();
                    resolve_label_attr_inner(eval, default, kind == "label", &mut sub,
                        Value::new_none())?
                } else {
                    default
                };
                own_fields.push((an, v));
            }
        }
        // target: the dep as seen so far (files + providers accumulated pre-aspect).
        let target = heap.alloc(DepTarget {
            label: label.to_string(),
            fields: vec![("files".to_string(), heap.alloc(Vec::<Value<'v>>::new()))],
            providers: providers.clone(),
        });
        let (repo, rest) = match label.split_once("//") {
            Some((r, rest)) if r.starts_with('@') => (Some(r.to_string()), rest),
            Some((_, rest)) => (None, rest),
            None => (None, label),
        };
        let (lpkg, lname) = rest.split_once(':').unwrap_or(("", rest));
        let ctx = heap.alloc(crate::engine::AbsorbWith {
            overrides: vec![
                (
                    "rule".to_string(),
                    heap.alloc(crate::engine::AbsorbWith {
                        overrides: vec![(
                            "attr".to_string(),
                            heap.alloc(starlark::values::structs::AllocStruct(rule_fields)),
                        )],
                    }),
                ),
                (
                    "attr".to_string(),
                    heap.alloc(starlark::values::structs::AllocStruct(own_fields)),
                ),
                ("actions".to_string(), heap.alloc_complex_no_freeze(crate::values::Actions)),
                (
                    "label".to_string(),
                    heap.alloc(crate::labels::LabelV {
                        repo,
                        package: lpkg.to_string(),
                        name: lname.to_string(),
                    }),
                ),
                ("toolchains".to_string(), crate::ctxv::toolchain_map(heap, session(eval))),
                ("bin_dir".to_string(), heap.alloc(crate::engine::AbsorbWith {
                    overrides: vec![("path".to_string(), heap.alloc("bazel-out/bin"))],
                })),
                ("genfiles_dir".to_string(), heap.alloc(crate::engine::AbsorbWith {
                    overrides: vec![("path".to_string(), heap.alloc("bazel-out/bin"))],
                })),
            ],
        });
        let ret = eval.eval_function(implementation, &[target, ctx], &[])?;
        let mut pairs: Vec<(Value<'v>, Value<'v>)> = Vec::new();
        if let Some(list) = ListRef::from_value(ret) {
            for item in list.iter() {
                if let Some(c) = instance_callable(item) {
                    pairs.push((c, item));
                }
            }
        } else if let Some(c) = instance_callable(ret) {
            pairs.push((c, ret));
        }
        decl_store(eval)?
            .captured
            .borrow_mut()
            .insert(format!("aspect::{label}"), pairs.clone());
        providers.extend(pairs);
        Ok(())
    })();
    session(eval).set_current_pkg(prev);
    result
}

/// Resolve a label/label_list attr VALUE (passed or default) to DepTarget struct(s) — shared by
/// the kwargs arm and the schema-defaults pass (implicit label attrs like rules_cc's
/// `_impl_delegate` must resolve exactly like passed ones).
pub(crate) fn resolve_label_attr<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    v: Value<'v>,
    single_label: bool,
    dep_labels: &mut Vec<String>,
    aspects: Value<'v>,
) -> starlark::Result<Value<'v>> {
    resolve_label_attr_inner(eval, v, single_label, dep_labels, aspects)
}

fn resolve_label_attr_inner<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    v: Value<'v>,
    single_label: bool,
    dep_labels: &mut Vec<String>,
    aspects: Value<'v>,
) -> starlark::Result<Value<'v>> {
    let one = |x: Value<'v>| -> Option<String> {
        x.unpack_str().map(String::from).or_else(|| {
    x.downcast_ref::<LabelV>().map(|l| l.to_string())
        })
    };
    let labels: Vec<String> = if let Some(list) = ListRef::from_value(v) {
        list.iter().filter_map(one).collect()
    } else if let Some(s) = one(v) {
        vec![s]
    } else {
        Vec::new()
    };
        let mut structs: Vec<Value<'v>> = Vec::new();
        for label in &labels {
            // Canonical label — bare in single-package mode, //pkg:name in a workspace.
            let mut dep = canon_label(session(eval), label);
            // E0: a forward-referenced local declaration analyzes on demand, first.
            ensure_analyzed(eval, &dep)?;
            // Aliases forward to their `actual` (provider flow lives on the terminal target).
            for _ in 0..32 {
                let next = session(eval).aliases.borrow().get(&dep).cloned();
                match next {
                    Some(actual) => {
                        dep = actual;
                        ensure_analyzed(eval, &dep)?;
                    }
                    None => break,
                }
            }
            let sess = session(eval);
            // Cross-package dep: load its package on demand (mirrors resolve_dep);
            // a failed load SURFACES (silence turns into bogus "not declared").
            let mut dep_load_err = None;
            if !sess.results.borrow().contains_key(&dep)
                && sess.workspace.is_some()
                && let Some(pkg) = crate::state::pkg_of(&dep)
            {
                dep_load_err = crate::rules::load_package(sess, &pkg).err();
            }
            if let Some(e) = dep_load_err
                && !sess.results.borrow().contains_key(&dep)
            {
                return Err(anyhow::anyhow!(
                    "loading dep `{dep}`'s package failed: {e}"
                )
                .into());
            }
            let resolved = {
                let results = sess.results.borrow();
                results.get(&dep).map(|t| t.default_info.clone())
            };
            let files = match resolved {
                Some(files) => files,
                None => {
                    // A file label naming a GENERATED output: resolve via the output index
                    // (the producer analyzes on demand; the dep's file is the output path).
                    let produced = sess.output_index.borrow().get(&dep).cloned();
                    if let Some((producer, out_path)) = produced {
                        ensure_analyzed(eval, &producer)?;
                        let heap = eval.heap();
                        let f = heap.alloc(crate::values::File { path: out_path });
                        structs.push(heap.alloc(DepTarget {
                            label: dep.clone(),
                            fields: vec![("files".to_string(), heap.alloc(vec![f]))],
                            providers: Vec::new(),
                        }));
                        dep_labels.push(producer);
                        continue;
                    }
                    // Bazel file-label semantics (L2): a label naming no declared target
                    // resolves to a SOURCE FILE in the package when that file exists
                    // (`srcs = ["lib.rs"]`). Source files are not target deps. External
                    // file labels check the vendored repo; their path takes Bazel's
                    // exec-root form (`external/<repo>/…`).
                    let (on_disk, qualified) = if let Some(rest) = dep.strip_prefix('@') {
                        match rest.split_once("//").and_then(|(r, pf)| {
                            pf.split_once(':').map(|(p, f)| (r, p, f))
                        }) {
                            Some((repo, pkg, file)) => {
                                // Host-materialized repo files (`@cc_compatibility_proxy//:
                                // symbols.bzl`) exist by construction — razel compiled them in.
                                let exists = crate::host::host_bzl(&dep).is_some()
                                    || sess
                                        .global
                                        .external_base
                                        .as_ref()
                                        .is_some_and(|base| {
                                            [repo.to_string(), repo.replace('_', "-")]
                                                .iter()
                                                .any(|d| {
                                                    crate::state::path_is_file(
                                                        sess,
                                                        &base.join(d).join(pkg).join(file),
                                                    )
                                                })
                                        });
                                (exists, format!("external/{repo}/{pkg}/{file}"))
                            }
                            None => (false, dep.clone()),
                        }
                    } else if let Some(rest) = dep.strip_prefix("//") {
                        // A workspace file label from ANY package: `//pkg:file` → `pkg/file`.
                        let q = rest.replacen(':', "/", 1).trim_start_matches('/').to_string();
                        let exists = sess
                            .workspace
                            .as_ref()
                            .is_some_and(|root| crate::state::path_is_file(sess, &root.join(&q)));
                        (exists, q)
                    } else {
                        let q = qualify(sess, label.trim_start_matches(':'));
                        let exists = sess
                            .workspace
                            .as_ref()
                            .is_some_and(|root| crate::state::path_is_file(sess, &root.join(&q)));
                        (exists, q)
                    };
                    if on_disk {
                        let heap = eval.heap();
                        let f = heap.alloc(crate::values::File { path: qualified });
                        structs.push(heap.alloc(DepTarget {
                            label: dep.clone(),
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
            if let Some(hit) = sess.fold_cache.borrow().get(&dep) {
                sfields.extend(hit.iter().cloned());
            } else {
                let folded = {
                    let dds = crate::dds::session_dds(sess);
                    crate::dds::fold_dep_fields(&dds, &tkey)
                };
                sess.fold_cache.borrow_mut().insert(dep.clone(), folded.clone());
                sfields.extend(folded);
            }
            // L2a: a dep is a DepTarget — plain projected fields by attr, plus the dep's
            // returned provider instances indexable by constructor (`dep[MyInfo]`).
            let heap = eval.heap();
            // `files` entries become FILE values (impls read .extension/.path on dep files);
            // folded provider fields (cflags/defines/…) stay plain strings.
            let dfields: Vec<(String, Value<'v>)> = sfields
                .into_iter()
                .map(|(k, xs)| {
                    if k == "files" {
                        let files: Vec<Value<'v>> = xs
                            .into_iter()
                            .map(|p| heap.alloc(crate::values::File { path: p }))
                            .collect();
                        (k, heap.alloc(files))
                    } else {
                        (k, heap.alloc(xs))
                    }
                })
                .collect();
            let providers = decl_store(eval)?
                .captured
                .borrow()
                .get(&dep)
                .cloned()
                .unwrap_or_default();
            // Layer 0: cross-package instances come from the Session harvest.
            let providers = if providers.is_empty() {
                cross_providers_for(eval, &dep)
            } else {
                providers
            };
            // Demand-analyzed-by-ANOTHER-consumer: the instances live in that consumer's
            // (unfrozen, mid-load) module — invisible here. Re-analyze the harvested decl in
            // THIS eval (idempotent: results overwrite by label) so instances are local.
            let providers = if providers.is_empty()
                && !session(eval).analyzing_contains(&dep)
            {
                // P4a: a results row is visible the moment its producer RECORDS it, but the
                // producer's captured-instance harvest lands only when its whole package eval
                // completes. If that package is mid-flight on ANOTHER worker, wait for it
                // (load_package: no-op when Done, condvar wait when InFlight), then re-read
                // the harvest before falling back to a local re-analysis.
                if let Some(pkg) = crate::state::pkg_of(&dep) {
                    let _ = crate::rules::load_package(session(eval), &pkg);
                }
                let waited = cross_providers_for(eval, &dep);
                if waited.is_empty() {
                    analyze_deferred(eval, &dep)?;
                    decl_store(eval)?
                        .captured
                        .borrow()
                        .get(&dep)
                        .cloned()
                        .unwrap_or_default()
                } else {
                    waited
                }
            } else {
                providers
            };
            // L5: apply this attr's aspects — extra providers attach to the dep target.
            let mut providers = providers;
            if let Some(l) = ListRef::from_value(aspects) {
                let to_apply: Vec<Value<'v>> = l.iter().collect();
                for a in to_apply {
                    apply_aspect(eval, a, &dep, &mut providers)?;
                }
            }
            let heap = eval.heap();
            structs.push(
                heap.alloc(DepTarget { label: dep.clone(), fields: dfields, providers }),
            );
            dep_labels.push(dep);
        }
        // D1c: a single `attr.label` yields ONE struct; a list yields the list of structs.
        Ok(if single_label {
            structs.into_iter().next().unwrap_or_else(Value::new_none)
        } else {
            eval.heap().alloc(structs)
        })
    }


/// The analysis of one Starlark-rule declaration — dep resolution (demand-driven), schema
/// defaults/`mandatory`, ctx construction, and the impl call. (Ran inside `invoke()` before E0.)
pub(crate) fn analyze_rule_decl<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    rule: Value<'v>,
    kwargs: &[(String, Value<'v>)],
) -> starlark::Result<()> {
    let (implementation, attrs, out_templates) = rule_parts(rule)?;
    // Deferred selects resolve HERE — attr consumption time (Bazel's model); conditions declared
    // anywhere in the loaded graph are visible by now. An EXPLICIT `None` kwarg means UNSET
    // (Bazel drops it; the schema default applies) — TF macros pass `copts = None` through.
    let kwargs: Vec<(String, Value<'v>)> = kwargs
        .iter()
        .filter(|(_, v)| !v.is_none())
        .map(|(k, v)| Ok((k.clone(), resolve_attr_value(eval, *v)?)))
        .collect::<anyhow::Result<_>>()?;
    let mut name = String::new();
    let mut dep_labels: Vec<String> = Vec::new();
    let mut fields: Vec<(String, Value<'v>)> = Vec::new();
    for (key, v) in &kwargs {
        let (key, v) = (key.clone(), *v);
        // D1b/c: the schema kind drives label resolution. Look it up once: `label`/`label_list`
        // resolve to provider struct(s); the legacy `deps` is an implicit `label_list`.
        let (attr_kind, attr_aspects): (Option<String>, Value<'v>) = {
            let mut k = None;
            let mut asp = Value::new_none();
            if let Some(d) = starlark::values::dict::DictRef::from_value(attrs) {
                for (kk, desc) in d.iter() {
                    if kk.unpack_str() == Some(key.as_str()) {
                        if let Ok(Some(kind)) = desc.get_attr("kind", eval.heap()) {
                            k = kind.unpack_str().map(String::from);
                        }
                        if let Ok(Some(a)) = desc.get_attr("aspects", eval.heap()) {
                            asp = a;
                        }
                        break;
                    }
                }
            }
            (k, asp)
        };
        let is_label =
            key == "deps" || matches!(attr_kind.as_deref(), Some("label") | Some("label_list"));
        let single_label = attr_kind.as_deref() == Some("label");
        match key.as_str() {
            "name" => {
                name = v.unpack_str().unwrap_or_default().to_string();
                fields.push((key, v));
            }
            // `label_keyed_string_dict` (TF's build_settings pattern): KEYS resolve to dep
            // targets; string values pass through.
            _ if attr_kind.as_deref() == Some("label_keyed_string_dict") => {
                let heap = eval.heap();
                let mut entries: Vec<(Value<'v>, Value<'v>)> = Vec::new();
                if let Some(d) = starlark::values::dict::DictRef::from_value(v) {
                    let pairs: Vec<(Value<'v>, Value<'v>)> = d.iter().collect();
                    drop(d);
                    for (kk, vv) in pairs {
                        let resolved =
                            resolve_label_attr(eval, kk, true, &mut dep_labels, Value::new_none())?;
                        entries.push((resolved, vv));
                    }
                }
                fields.push((key, heap.alloc(starlark::values::dict::AllocDict(entries))));
            }
            // A label attr (legacy `deps` or any `attr.label_list`): resolve each label to its
            // analyzed providers as a `struct(files=…, <folded fields>…)`.
            _ if is_label => {
                let resolved =
                    resolve_label_attr(eval, v, single_label, &mut dep_labels, attr_aspects)?;
                fields.push((key, resolved));
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
            let kind = descriptor
                .get_attr("kind", eval.heap())?
                .and_then(|k| k.unpack_str().map(String::from))
                .unwrap_or_default();
            if !default.is_none() {
                // Implicit label attrs (rules_cc `_impl_delegate` etc.) resolve like passed ones.
                let v = if kind == "label" || kind == "label_list" {
                    let default = resolve_attr_value(eval, default)?;
                    let asp = descriptor
                        .get_attr("aspects", eval.heap())?
                        .unwrap_or_else(Value::new_none);
                    resolve_label_attr(eval, default, kind == "label", &mut dep_labels, asp)?
                } else {
                    default
                };
                fields.push((an.to_string(), v));
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
    sess.set_current_target(Some(AnalyzedTarget {
        name: canon_label(sess, &name),
        deps: dep_labels,
        ..Default::default()
    }));

    // ctx.outputs.<attr> — string-valued attrs are predeclared output filenames
    // (package-qualified). ctx.files.<attr> — list-valued attrs are source files
    // (qualified). ctx.executable is empty until an executable-label attr is wired.
    let mk_file = |s: &str| heap.alloc(File { path: qualify(sess, s) });
    let mut outputs_fields: Vec<(String, Value<'v>)> = Vec::new();
    // Implicit outputs: rule(outputs = {"attr": "%{name}.ext"}) — templates expand with the
    // target name into package-qualified Files on ctx.outputs.
    if let Some(d) = starlark::values::dict::DictRef::from_value(out_templates) {
        let name = kwargs
            .iter()
            .find(|(k, _)| k == "name")
            .and_then(|(_, v)| v.unpack_str())
            .unwrap_or_default()
            .to_string();
        for (k, tpl) in d.iter() {
            if let (Some(k), Some(tpl)) = (k.unpack_str(), tpl.unpack_str()) {
                let path = qualify(sess, &tpl.replace("%{name}", &name));
                outputs_fields.push((k.to_string(), heap.alloc(File { path })));
            }
        }
    }
    let kw_outputs: Vec<(String, Value<'v>)> = kwargs
        .iter()
        .filter_map(|(k, v)| v.unpack_str().map(|s| (k.clone(), mk_file(s))))
        .collect();
    outputs_fields.extend(kw_outputs);
    let files_fields: Vec<(String, Value<'v>)> = kwargs
        .iter()
        .filter_map(|(k, v)| {
            if let Some(list) = ListRef::from_value(*v) {
                let items: Vec<Value<'v>> = list
                    .iter()
                    .filter_map(|it| it.unpack_str().map(mk_file))
                    .collect();
                Some((k.clone(), heap.alloc(items)))
            } else {
                // Single-string label attrs feed ctx.file.<attr> too (gentbl's td_file).
                v.unpack_str().map(|s| {
                    let one: Vec<Value<'v>> = vec![mk_file(s)];
                    (k.clone(), heap.alloc(one))
                })
            }
        })
        .collect();
    let ctx = heap.alloc_complex_no_freeze(Ctx {
            attr: heap.alloc(AllocStruct(fields)),
            actions: heap.alloc_complex_no_freeze(Actions),
            // `ctx.label` is a real Label value (.package/.name/.workspace_root; canonical str()).
            label: {
                let cur = sess.current_pkg().unwrap_or_default();
                let (repo, pkg) = match cur.strip_prefix('@') {
                    Some(rest) => match rest.split_once("//") {
                        Some((r, p)) => (Some(format!("@{r}")), p.to_string()),
                        None => (None, cur.clone()),
                    },
                    None => (None, cur.clone()),
                };
                heap.alloc(LabelV { repo, package: pkg, name: name.clone() })
            },
            // Defaulting namespace: an OMITTED output_list attr reads as [] (Bazel: the
            // declared-outputs view; gentbl's additional_outputs).
            outputs: heap.alloc_complex_no_freeze(crate::ctxv::FilesNs {
                fields: outputs_fields,
            }),
            files: heap.alloc_complex_no_freeze(crate::ctxv::FilesNs {
                fields: files_fields.clone(),
            }),
            file: heap.alloc_complex_no_freeze(crate::ctxv::FileNs { fields: files_fields }),
            var: heap.alloc(starlark::values::dict::AllocDict([
                (heap.alloc("COMPILATION_MODE"), heap.alloc(sess.global.mode())),
                (heap.alloc("TARGET_CPU"), heap.alloc(crate::state::host_cpu())),
                (heap.alloc("BINDIR"), heap.alloc("bazel-out/bin")),
                // Standard cc-toolchain Make vars (host-true: empty on this platform).
                (heap.alloc("STACK_FRAME_UNLIMITED"), heap.alloc("")),
            ])),
            executable: heap.alloc_complex_no_freeze(crate::ctxv::ExecNs { fields: Vec::new() }),
            toolchains: toolchain_map(heap, sess),
            build_setting_value: kwargs
                .iter()
                .find(|(k, _)| k == "build_setting_default")
                .map(|(_, v)| *v)
                .unwrap_or_else(Value::new_none),
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
    let committed = sess.take_current_target();
    if let Some(c) = committed {
        record_target(sess, c);
    }
    Ok(())
}
