//! `decls::capture` — split from `decls.rs` (facade in `mod.rs`).

use crate::state::session;
use starlark::environment::Module;
use starlark::eval::Evaluator;
use starlark::values::list::ListRef;
use starlark::values::{
    Value, ValueLike,
};
// ---- rule() + DefaultInfo + select ----------------------------------------------
use super::*;

/// Layer 0 pre-freeze step: copy the decl store's captured map into [`CAPTURED_VAR`] as plain
/// dict/list/tuples, then unroot the (unfreezable, already-drained) decl store.
pub(crate) fn stash_captured_for_freeze<'v>(
    module: &Module<'v>,
    sess: &crate::state::Session,
) -> anyhow::Result<()> {
    let heap = module.heap();
    let Some(storev) = module.get(DECLS_VAR) else {
        return Ok(());
    };
    let Some(store) = storev.downcast_ref::<DeclStore<'v>>() else {
        return Ok(());
    };
    let captured = store.captured.borrow();
    let entries: Vec<(Value<'v>, Value<'v>)> = captured
        .iter()
        .map(|(label, pairs)| {
            let items: Vec<Value<'v>> = pairs.iter().map(|(c, i)| heap.alloc((*c, *i))).collect();
            (heap.alloc(label.as_str()), heap.alloc(items))
        })
        .collect();
    drop(captured);
    module.set(
        CAPTURED_VAR,
        heap.alloc(starlark::values::dict::AllocDict(entries)),
    );
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
    module.set(
        DEFERRED_VAR,
        heap.alloc(starlark::values::dict::AllocDict(deferred)),
    );
    module.set(DECLS_VAR, Value::new_none());
    Ok(())
}


/// Analysis-failure salvage: publish only provider captures that already completed in this
/// module. Do NOT publish deferred declarations for the failed package; analysis failures are
/// retryable, and the package must not become demand-visible through the deferred index.
pub(crate) fn stash_captured_only_for_freeze<'v>(module: &Module<'v>) -> anyhow::Result<bool> {
    let heap = module.heap();
    let Some(storev) = module.get(DECLS_VAR) else {
        return Ok(false);
    };
    let Some(store) = storev.downcast_ref::<DeclStore<'v>>() else {
        return Ok(false);
    };
    let captured = store.captured.borrow();
    if captured.is_empty() {
        return Ok(false);
    }
    let entries: Vec<(Value<'v>, Value<'v>)> = captured
        .iter()
        .map(|(label, pairs)| {
            let items: Vec<Value<'v>> = pairs.iter().map(|(c, i)| heap.alloc((*c, *i))).collect();
            (heap.alloc(label.as_str()), heap.alloc(items))
        })
        .collect();
    drop(captured);
    module.set(
        CAPTURED_VAR,
        heap.alloc(starlark::values::dict::AllocDict(entries)),
    );
    module.set(DECLS_VAR, Value::new_none());
    Ok(true)
}


/// Layer 0 lookup: a completed package's harvested instances for `label`, re-viewed in the
/// consumer's eval (sound: the Session's OwnedFrozenValues keep the source heaps alive).
pub(crate) fn cross_providers_for<'v>(
    eval: &Evaluator<'v, '_, '_>,
    label: &str,
) -> Option<Vec<(Value<'v>, Value<'v>)>> {
    let sess = session(eval);
    // `owned_value(frozen_heap)`: the CONSUMER module's frozen heap takes a reference to the
    // source heap, so the returned values stay alive as long as the consumer module — the
    // sound cross-heap pattern (buck2's). O(1) via the harvest index.
    let owners: Vec<starlark::values::OwnedFrozenValue> = match sess.cross_index.borrow().get(label)
    {
        Some(&i) => vec![sess.cross_captured.borrow()[i].clone()],
        None => return None,
    };
    for owned in &owners {
        // SAFETY: `owned_frozen_value` registers the source heap on the CONSUMER module's
        // frozen heap, which outlives every `'v` value of that module — the values stay live
        // for at least `'v` (the safe `owned_value` wrapper merely picks the shorter `'a`).
        let fv = unsafe { owned.owned_frozen_value(eval.frozen_heap()) };
        let dictv: Value<'v> = fv.to_value();
        let Some(d) = starlark::values::dict::DictRef::from_value(dictv) else {
            continue;
        };
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
                return Some(out);
            }
        }
    }
    None
}


/// A rule value's (implementation, attrs-schema, outputs-templates), frozen or not.
pub(crate) fn rule_parts<'v>(rule: Value<'v>) -> anyhow::Result<(Value<'v>, Value<'v>, Value<'v>)> {
    if let Some(r) = rule.downcast_ref::<RuleObj<'v>>() {
        Ok((r.implementation, r.attrs, r.outputs))
    } else if let Some(r) = rule.downcast_ref::<FrozenRuleObj>() {
        Ok((
            r.implementation.to_value(),
            r.attrs.to_value(),
            r.outputs.to_value(),
        ))
    } else {
        Err(anyhow::anyhow!("declaration's rule is not a rule value"))
    }
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
        decls.push(Some(Decl {
            label: label.clone(),
            body: DeclBody::Native(nidx),
        }));
        decls.len() - 1
    };
    sess.pending.borrow_mut().insert(label, idx);
    Ok(())
}


