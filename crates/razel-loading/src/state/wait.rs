//! `state::wait` — split from `state.rs` (facade in `mod.rs`).

use super::*;

/// F3 (demand futures): the result of waiting on a declaration of a MID-FLIGHT package.
pub(crate) enum PendingWait {
    /// The owner recorded the declaration (`record_target` published) — read `results`.
    Published,
    /// The owning package reached a terminal state — re-resolve via harvest / its error.
    PkgTerminal,
    /// Waiting is unsound here (owner is this thread, or a true dependency cycle) —
    /// proceed against partial state; the callers' error paths report.
    Proceed,
}


/// Wait for a declaration that is `pending` in a package another worker owns: its body
/// lives on that worker's heap (P4a bug #3 — unreachable here), but its `record_target`
/// IS cross-thread-visible. Creates a PROXY entry `Decl(label) = InFlight(pkg owner)` in
/// the wait graph (atomically re-verifying the package is still mid-flight elsewhere),
/// parks through the shared condvar, and resolves on publish or the package-finish sweep.
/// Cycle rule as in [`acquire_resource`]: park through breaker-carrying cycles (waking the
/// breaker), proceed-partial on all-Decl ones. Sequentially this NEVER waits (the owner is
/// always this thread → `Proceed` with no events) — threads=1 behavior is untouched.
pub(crate) fn wait_pending_decl(sess: &Session, label: &str, pkg: &str) -> PendingWait {
    let me = std::thread::current().id();
    let dkey = ResKey::Decl(label.to_string());
    let pkey = ResKey::Pkg(pkg.to_string());
    {
        // Silent pre-check: only engage (and emit events) when the package is genuinely
        // mid-flight on ANOTHER worker. A vanished package (FailRetry purge between the
        // pending peek and here) counts as a partial read — its retry may resolve this.
        let g = sess.loaded.lock().expect("loaded poisoned");
        match g.res.get(&pkey) {
            Some(PkgState::InFlight(owner)) if *owner != me => {}
            Some(PkgState::InFlight(_)) => return PendingWait::Proceed,
            Some(_) => return PendingWait::PkgTerminal,
            None => {
                drop(g);
                sess.note_partial_read();
                return PendingWait::PkgTerminal;
            }
        }
    }
    sched_event(sess, "enter", label);
    let (outcome, partial) = wait_pending_decl_locked(sess, &dkey, &pkey, me);
    if partial {
        sess.note_partial_read();
    }
    let point = match outcome {
        PendingWait::Published => "decl-published",
        PendingWait::PkgTerminal => "decl-terminal",
        PendingWait::Proceed => "decl-proceed",
    };
    sched_event(sess, point, label);
    outcome
}


pub(crate) fn wait_pending_decl_locked(
    sess: &Session,
    dkey: &ResKey,
    pkey: &ResKey,
    me: std::thread::ThreadId,
) -> (PendingWait, bool) {
    let mut g = sess.loaded.lock().expect("loaded poisoned");
    loop {
        let owner = match g.res.get(dkey) {
            Some(PkgState::Done) => return (PendingWait::Published, false),
            // A terminal failure on the declaration itself (native FailCached) — the
            // callers' memo paths serve the real error.
            Some(PkgState::Failed(_)) => return (PendingWait::PkgTerminal, false),
            Some(PkgState::InFlight(t)) if *t == me => return (PendingWait::Proceed, false),
            Some(PkgState::InFlight(t)) => *t,
            None => match g.res.get(pkey) {
                // (Re)create the proxy, owned by the package's owner — the walk and the
                // publish/sweep treat it exactly like a claimed resource.
                Some(PkgState::InFlight(owner)) if *owner != me => {
                    let owner = *owner;
                    g.res.insert(dkey.clone(), PkgState::InFlight(owner));
                    owner
                }
                Some(PkgState::InFlight(_)) => return (PendingWait::Proceed, false),
                Some(_) => return (PendingWait::PkgTerminal, false),
                // FailRetry removed the package mid-wait: the declaration died with a
                // partial-read failure somewhere — restart-eligible.
                None => return (PendingWait::PkgTerminal, true),
            },
        };
        let mut wake_breaker = false;
        if let Some(has_breaker) = walk_cycle_kind(&g, owner, me) {
            if !has_breaker {
                // All-Decl cycle: the publishers are all parked on each other — a true
                // cross-thread deadlock shape. Proceed-partial (restart-eligible).
                return (PendingWait::Proceed, true);
            }
            wake_breaker = true;
        }
        g.waiting.insert(me, dkey.clone());
        if wake_breaker {
            sess.loaded_cv.notify_all();
        }
        trace_load("decl-wait", dkey.name());
        let (g2, t) = sess
            .loaded_cv
            .wait_timeout(g, std::time::Duration::from_secs(20))
            .expect("loaded poisoned");
        g = g2;
        g.waiting.remove(&me);
        trace_load("decl-wake", dkey.name());
        if t.timed_out() {
            eprintln!(
                "razel: warning: declaration wait exceeded 20s; re-parking on `{}`",
                dkey.name()
            );
            trace_load("decl-timeout", dkey.name());
        }
    }
}


/// `record_target`'s wait-graph hook (F3): an InFlight `Decl(label)` entry — a waiter's
/// proxy or a native runner's claim — completes the moment the target's row is recorded
/// (the only cross-thread-visible completion that exists mid-eval; instances complete at
/// package freeze). No-op (one mutex probe) when nobody registered interest.
pub(crate) fn publish_decl(sess: &Session, label: &str) {
    let published = {
        let mut g = sess.loaded.lock().expect("loaded poisoned");
        match g.res.get_mut(&ResKey::Decl(label.to_string())) {
            Some(st @ PkgState::InFlight(_)) => {
                *st = PkgState::Done;
                sess.loaded_cv.notify_all();
                true
            }
            _ => false,
        }
    };
    if published {
        sched_event(sess, "decl-done", label);
    }
}


/// A loading observation hook (the S2 deterministic-interleaving seam): called with
/// `(point, key)` at load-coordination and diagnostic events. Points: `"enter"` (before the graph lock —
/// the ONLY point where a test may block, e.g. on a barrier, to script an interleaving),
/// and post-lock outcomes: `"own"`, `"ready"`, `"reentry"`, `"cycle-proceed"`,
/// `"takeover-timeout"`, `"finish-ok"`, `"finish-err"`. Diagnostic points include
/// `"provider-reanalyze"`. Production runs carry `None` — one Option read per event.
#[derive(Clone)]
pub struct SchedHook(pub std::sync::Arc<dyn Fn(&str, &str) + Send + Sync>);


impl std::fmt::Debug for SchedHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SchedHook(..)")
    }
}


#[cfg(test)]
mod sync_assertions {
    /// P2 (worker-pool plan): the Session must be shareable across workers.
    #[test]
    fn session_is_send_and_sync() {
        fn assert_sync<T: Send + Sync>() {}
        assert_sync::<super::Session>();
    }
}


