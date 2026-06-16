//! `decls::drive` — split from `decls.rs` (facade in `mod.rs`).

use crate::state::session;
use starlark::eval::Evaluator;
use starlark::values::list::ListRef;
use starlark::values::{
    Value, ValueLike,
};
// ---- rule() + DefaultInfo + select ----------------------------------------------
use super::*;

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
    // F3 (demand futures): the label is DECLARED in a package mid-flight on another worker
    // (the Session-wide `pending` map), but its body lives on that worker's heap —
    // unreachable here (P4a bug #3). Wait for the owner's record (the per-declaration
    // future) or the package's terminal sweep instead of erroring on partial state;
    // an unwaitable wait (true dependency cycle / own-thread re-entry) proceeds-partial.
    {
        let pend_pkg = {
            let sess = session(eval);
            if sess.pending.borrow().contains_key(label) {
                crate::state::pkg_of(label)
            } else {
                None
            }
        };
        if let Some(pkg) = pend_pkg {
            match crate::state::wait_pending_decl(session(eval), label, &pkg) {
                crate::state::PendingWait::Published => return Ok(()),
                // Terminal or unwaitable: the harvest / native / error paths below decide.
                crate::state::PendingWait::PkgTerminal | crate::state::PendingWait::Proceed => {}
            }
        }
    }
    let nidx = { session(eval).deferred_natives.borrow().get(label).copied() };
    if let Some(nidx) = nidx {
        // F2 (demand futures): single-flight the FnOnce demand-run. Without the claim, the
        // loser of a cross-thread race saw an empty slot with no results row yet and erred
        // "neither a declared target nor a source file" — the wrong reason.
        let key = crate::state::ResKey::Decl(label.to_string());
        match crate::state::acquire_resource(session(eval), &key) {
            crate::state::Acquire::Own => {
                let f = { session(eval).native_decls.borrow_mut()[nidx].take() };
                let res = match f {
                    Some(f) => run_native_deferred(eval, label, f),
                    // Consumed before the claim existed (a pre-claim drive) — the results/
                    // memo fallthroughs below decide.
                    None => Ok(()),
                };
                let outcome = match &res {
                    Ok(()) => crate::state::FinishOutcome::Ok,
                    // FnOnce: the run can never retry — cache the real error for waiters
                    // and later demanders (the native_errors memo's wait-graph twin).
                    Err(e) => crate::state::FinishOutcome::FailCached(e.to_string()),
                };
                crate::state::finish_resource(session(eval), &key, outcome);
                return res;
            }
            // The runner recorded it — the caller re-reads results.
            crate::state::Acquire::Ready => return Ok(()),
            crate::state::Acquire::Failed(e) => {
                return Err(anyhow::anyhow!("analysis of `{label}` previously failed: {e}").into());
            }
            // Same-thread re-entry / cross-thread dependency cycle: proceed-partial — the
            // analyzing-set and the callers' error paths report it.
            crate::state::Acquire::Reentry | crate::state::Acquire::CycleProceed => {
                return Ok(());
            }
        }
    }
    analyze_deferred(eval, label)
}


/// Run a deferred NATIVE body on demand (cycle-guarded, in the decl's package context).
pub(crate) fn run_native_deferred<'v>(
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
    let res: starlark::Result<()> = f(eval).map_err(Into::into);
    session(eval).set_current_pkg(prev);
    session(eval).analyzing_remove(label);
    if let Err(e) = &res {
        // The body is consumed (FnOnce) — memo the error so later consumers get the REAL
        // reason instead of "not analyzed" (round 29).
        session(eval)
            .native_errors
            .borrow_mut()
            .insert(label.to_string(), e.to_string());
    }
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
    let Some((pkg, rule, kwargs)) = found else {
        return Ok(());
    };
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
        let Some(d) = starlark::values::dict::DictRef::from_value(dictv) else {
            continue;
        };
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


/// Find the original Starlark rule declaration for aspect `ctx.rule.attr` construction. Unlike
/// [`find_deferred`], this also sees same-package declarations that have already been analyzed.
pub(crate) fn find_original_decl<'v>(
    eval: &Evaluator<'v, '_, '_>,
    label: &str,
) -> Option<(String, Value<'v>, Vec<(String, Value<'v>)>)> {
    let sess = session(eval);
    if let Ok(store) = decl_store(eval)
        && let Some((rule, kwargs)) = store.originals.borrow().get(label).cloned()
    {
        return Some((sess.current_pkg().unwrap_or_default(), rule, kwargs));
    }
    find_deferred(eval, label)
}


/// Analyze the declaration at `idx` (no-op if its slot was already taken). Cycle-guarded via
/// `Session.analyzing`.
pub(crate) fn analyze_decl<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    idx: usize,
) -> starlark::Result<()> {
    let decl = { decl_store(eval)?.decls.borrow_mut()[idx].take() };
    let Some(decl) = decl else { return Ok(()) };
    {
        let sess = session(eval);
        if !sess.analyzing_insert(&decl.label) {
            return Err(anyhow::anyhow!("dependency cycle detected at `{}`", decl.label).into());
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


