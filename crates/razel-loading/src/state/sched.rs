//! `state::sched` — split from `state.rs` (facade in `mod.rs`).

use super::*;

#[derive(Clone, Debug)]
pub(crate) enum PkgState {
    InFlight(std::thread::ThreadId),
    Done,
    /// Round 29: a DECLARE-phase failure — Bazel's "package in error". The load ran once;
    /// every later consumer reads this cached error (loud, no re-eval). Analysis-phase
    /// failures clear instead (retryable — the round-24 unpoisoning semantics).
    Failed(String),
}


/// A wait-graph resource, TYPED (round 33): the former string keys discriminated package vs
/// `.bzl` by `:`-in-key — unsound once target labels (which carry `:`) join the graph for
/// demand futures. The variant drives cycle resolution; the name is the hook/trace rendering
/// (unchanged strings: pkg name, bzl path, declaration label).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ResKey {
    Pkg(String),
    Bzl(String),
    /// A per-DECLARATION demand future (RazelDemandFutures.md): completes at `record_target`;
    /// the owning package's finish sweeps stragglers. Cycle rule: a declaration waiter never
    /// waits into a cycle (the publisher is blocked on the waiter) — it proceeds-partial.
    Decl(String),
}


impl ResKey {
    pub(crate) fn name(&self) -> &str {
        match self {
            ResKey::Pkg(s) | ResKey::Bzl(s) | ResKey::Decl(s) => s,
        }
    }
}


/// The single-flight WAIT GRAPH (P4a): packages AND `.bzl` modules in one keyed map (typed —
/// [`ResKey`]), plus the waits-for edges that make cross-thread demand cycles DETECTABLE.
/// Package↔bzl cycles are real (worker A's package loads a module owned by worker B whose
/// eval demand-loads A's package), so the resource kinds must share one graph and one lock.
#[derive(Default)]
pub(crate) struct WaitGraph {
    pub(crate) res: std::collections::HashMap<ResKey, PkgState>,
    /// worker → the resource key it is blocked on (one edge per parked worker).
    pub(crate) waiting: std::collections::HashMap<std::thread::ThreadId, ResKey>,
    /// CONCURRENT evaluations per key (cycle/timeout takeovers duplicate a load while the
    /// original owner is still evaluating). Failure cleanup must respect survivors: a failed
    /// finisher may only purge when it is the LAST live eval and nobody succeeded.
    pub(crate) live: std::collections::HashMap<ResKey, usize>,
}


/// What acquiring a resource grants the caller.
pub(crate) enum Acquire {
    /// This caller evaluates (and MUST call [`finish_resource`]). Granted for: first claim,
    /// detected-cycle takeover of a `.bzl` module, and timeout takeover.
    Own,
    /// Another worker finished it (Done) — read the relevant cache/state.
    Ready,
    /// THIS thread is already mid-load on it (re-entry on the caller's own stack).
    Reentry,
    /// A cross-thread PACKAGE demand cycle: proceed against the owner's partial state — the
    /// exact cross-thread analogue of sequential re-entry (`a→b→a` no-ops and `b` reads `a`'s
    /// mid-eval declarations). Duplicating instead (the old takeover) bred divergent results
    /// and stale-owner windows.
    CycleProceed,
    /// The resource is a CACHED package-in-error (declare-phase failure) — the caller
    /// surfaces this error without re-evaluating.
    Failed(String),
}


/// How an owned load ended — drives the wait-graph's terminal state (see [`PkgState`]).
pub(crate) enum FinishOutcome {
    Ok,
    /// Analysis-phase failure: clear for retry (round-24 semantics).
    FailRetry,
    /// Declare-phase / pre-eval failure: cache as package-in-error.
    FailCached(String),
}


/// The wait-graph trace (S3): `RAZEL_TRACE_LOAD=1` prints every coordination event — the
/// instrument that caught the harvest-index and InFlight-leak bugs, made permanent. Checked
/// per event (loads are low-frequency; no cached static — AD2).
pub(crate) fn trace_load(point: &str, key: &str) {
    if std::env::var_os("RAZEL_TRACE_LOAD").is_some() {
        eprintln!(
            "razel-trace: {:?} {point} `{key}`",
            std::thread::current().id()
        );
    }
}


/// Emit a loading diagnostic event to the trace + the S2 hook.
pub(crate) fn load_event(sess: &Session, point: &str, key: &str) {
    trace_load(point, key);
    if let Some(h) = &sess.global.sched_hook {
        (h.0)(point, key);
    }
}


/// Emit a wait-graph event. MUST be called WITHOUT the graph lock held (hooks may block at
/// "enter" by design — that is how tests script schedules).
pub(crate) fn sched_event(sess: &Session, point: &str, key: &str) {
    load_event(sess, point, key);
}


/// Single-flight acquire with DEADLOCK-FREE waiting: before parking, walk the waits-for chain
/// from the owner; if it reaches a resource THIS thread owns, the wait would deadlock — take
/// the load over NOW instead (duplicate eval is waste, not wrong: results overwrite by label,
/// the bzl cache by key). Package-level demand cycles are LEGAL in Bazel's model, and at TF
/// scale they are dense — the previous 20s-timeout-only takeover degenerated into an
/// hours-long livelock (every cycle edge cost a 20s sleep). The timeout stays as a backstop
/// for waits the graph cannot see (it should never fire in practice — it prints loudly).
pub(crate) fn acquire_resource(sess: &Session, key: &ResKey) -> Acquire {
    sched_event(sess, "enter", key.name());
    let (out, timed_out) = acquire_resource_locked(sess, key);
    let point = match out {
        Acquire::Own if timed_out => "takeover-timeout",
        Acquire::Own => "own",
        Acquire::Ready => "ready",
        Acquire::Reentry => "reentry",
        Acquire::CycleProceed => "cycle-proceed",
        Acquire::Failed(_) => "failed-cached",
    };
    // F4: every CycleProceed is a CROSS-thread partial read (same-thread re-entry takes
    // the Reentry arm) — count it so a failed entry load can restart.
    if matches!(out, Acquire::CycleProceed) {
        sess.note_partial_read();
    }
    sched_event(sess, point, key.name());
    out
}


pub(crate) fn acquire_resource_locked(sess: &Session, key: &ResKey) -> (Acquire, bool) {
    let me = std::thread::current().id();
    let mut g = sess.loaded.lock().expect("loaded poisoned");
    loop {
        match g.res.get(key) {
            Some(PkgState::Done) => return (Acquire::Ready, false),
            Some(PkgState::Failed(e)) => return (Acquire::Failed(e.clone()), false),
            Some(PkgState::InFlight(tid)) if *tid == me => return (Acquire::Reentry, false),
            Some(PkgState::InFlight(owner)) => {
                // Cycle check: owner → (what owner waits on) → its owner → … → me?
                let mut wake_breaker = false;
                if let Some(has_breaker) = walk_cycle_kind(&g, *owner, me) {
                    match key {
                        // Packages: sequential re-entry semantics — proceed against the
                        // owner's partial state, no duplicate eval.
                        ResKey::Pkg(_) => return (Acquire::CycleProceed, false),
                        // Declarations: an all-Decl cycle is a true dependency cycle —
                        // proceed-partial (callers report). A cycle with a Pkg/Bzl wait
                        // edge has a BREAKER: that waiter cycle-proceeds (or takes over)
                        // on its next wake — park THROUGH the cycle and wake it (it
                        // parked before this edge existed).
                        ResKey::Decl(_) if !has_breaker => {
                            return (Acquire::CycleProceed, false);
                        }
                        ResKey::Decl(_) => wake_breaker = true,
                        // `.bzl` modules: the module VALUE is required, so take the eval
                        // over (duplicate; converges — the worst case is an illegal load
                        // cycle, which the eval reports loudly).
                        ResKey::Bzl(_) => {
                            g.res.insert(key.clone(), PkgState::InFlight(me));
                            *g.live.entry(key.clone()).or_default() += 1;
                            return (Acquire::Own, false);
                        }
                    }
                }
                g.waiting.insert(me, key.clone());
                if wake_breaker {
                    sess.loaded_cv.notify_all();
                }
                trace_load("wait", key.name());
                let (g2, t) = sess
                    .loaded_cv
                    .wait_timeout(g, std::time::Duration::from_secs(20))
                    .expect("loaded poisoned");
                g = g2;
                g.waiting.remove(&me);
                trace_load("wake", key.name());
                if t.timed_out() {
                    // A declaration wait cannot take over (the body is non-local); its
                    // publisher is leak-proof (record publish / package-finish sweep), so
                    // re-park loudly instead of duplicating.
                    if matches!(key, ResKey::Decl(_)) {
                        eprintln!(
                            "razel: warning: declaration wait exceeded 20s; re-parking on `{}`",
                            key.name()
                        );
                        trace_load("decl-timeout", key.name());
                        continue;
                    }
                    eprintln!(
                        "razel: warning: load wait timed out (unseen cycle?); duplicating `{}`",
                        key.name()
                    );
                    g.res.insert(key.clone(), PkgState::InFlight(me));
                    *g.live.entry(key.clone()).or_default() += 1;
                    return (Acquire::Own, true);
                }
            }
            None => {
                g.res.insert(key.clone(), PkgState::InFlight(me));
                *g.live.entry(key.clone()).or_default() += 1;
                return (Acquire::Own, false);
            }
        }
    }
}


/// Walk the waits-for chain from `start`: `Some(has_breaker)` when it reaches a resource
/// `me` owns (a cycle) — `has_breaker` = some wait edge in the chain is a Pkg/Bzl key,
/// i.e. a parked worker that resolves the cycle itself on its next wake (CycleProceed /
/// takeover). `None` = no cycle.
pub(crate) fn walk_cycle_kind(
    g: &WaitGraph,
    start: std::thread::ThreadId,
    me: std::thread::ThreadId,
) -> Option<bool> {
    let mut cur = start;
    let mut has_breaker = false;
    for _ in 0..128 {
        let Some(next_key) = g.waiting.get(&cur) else {
            return None;
        };
        if !matches!(next_key, ResKey::Decl(_)) {
            has_breaker = true;
        }
        match g.res.get(next_key) {
            Some(PkgState::InFlight(o2)) => {
                if *o2 == me {
                    return Some(has_breaker);
                }
                cur = *o2;
            }
            _ => return None,
        }
    }
    None
}


/// Finish an owned resource: Done on success (any cache insert MUST precede this — Ready
/// readers consult it), clear on failure (retryable); wake waiters. Returns whether the
/// caller may run FAILURE CLEANUP (purge): only the LAST live eval of the key may, and only
/// when no concurrent eval succeeded — a takeover duplicate failing mid-way must not clobber
/// the original owner's in-progress (or completed) results.
pub(crate) fn finish_resource(sess: &Session, key: &ResKey, outcome: FinishOutcome) -> bool {
    let ok = matches!(outcome, FinishOutcome::Ok);
    let may_purge = {
        let mut g = sess.loaded.lock().expect("loaded poisoned");
        let live = g.live.entry(key.clone()).or_default();
        *live = live.saturating_sub(1);
        let last = *live == 0;
        let may_purge = match outcome {
            FinishOutcome::Ok => {
                g.res.insert(key.clone(), PkgState::Done);
                false
            }
            // A concurrent eval already succeeded — a failure is moot; keep Done.
            _ if matches!(g.res.get(key), Some(PkgState::Done)) => false,
            // Survivors are still evaluating: leave THEIR InFlight claim in place and do
            // NOT purge (the last finisher reconciles).
            _ if !last => false,
            FinishOutcome::FailRetry => {
                g.res.remove(key);
                true
            }
            FinishOutcome::FailCached(e) => {
                // Package-in-error: cache the error; partial declares still purge.
                g.res.insert(key.clone(), PkgState::Failed(e));
                true
            }
        };
        // F3: a package's terminal state sweeps its declarations' outstanding proxy
        // futures — woken waiters re-resolve against the now-terminal package (harvest
        // visible, or the failure surfaces). Leak-proof: no proxy entry outlives its
        // package's InFlight window. Individually-finished Decl entries (native runs,
        // published records) are terminal facts and stay.
        if let ResKey::Pkg(p) = key {
            g.res.retain(|k, st| {
                !(matches!((k, st), (ResKey::Decl(l), PkgState::InFlight(_))
                    if pkg_of(l).as_deref() == Some(p.as_str())))
            });
        }
        sess.loaded_cv.notify_all();
        may_purge
    };
    sched_event(
        sess,
        if ok { "finish-ok" } else { "finish-err" },
        key.name(),
    );
    may_purge
}


/// Finish a load this caller owned (see [`finish_resource`]); failures also purge the
/// package's partial results — but only when this was the LAST live eval of the package
/// (takeover duplicates must not clobber a surviving owner's rows).
pub(crate) fn finish_pkg_load(sess: &Session, pkg: &str, outcome: FinishOutcome) {
    if finish_resource(sess, &ResKey::Pkg(pkg.to_string()), outcome) {
        purge_partial_package(sess, pkg);
    }
}


/// What [`begin_bzl_load`] grants the caller.
pub(crate) enum BzlBegin {
    /// This caller evaluates the module (and MUST call [`finish_bzl_load`]). Also granted on
    /// same-thread re-entry (a recursive load — the eval reports the cycle, as before).
    Own,
    /// Another worker finished it — the `bzl_cache` has the frozen module.
    Ready,
}


/// P4a single-flight for `.bzl` evaluation: ONE eval per module per Session even under the
/// pool. Concurrent double-eval would mint two PROVIDER IDENTITIES for the same `provider()`
/// (the cache's "identities hold across packages" contract breaks — `dep[MyInfo]` ptr-eq
/// fails with "does not provide ... (have 1 pairs)"). Shares the package wait graph (mixed
/// package↔bzl cycles resolve by takeover instead of deadlocking).
pub(crate) fn begin_bzl_load(sess: &Session, key: &str) -> BzlBegin {
    match acquire_resource(sess, &ResKey::Bzl(key.to_string())) {
        Acquire::Own | Acquire::Reentry => BzlBegin::Own,
        // CycleProceed/Failed are unreachable for Bzl keys (bzl cycles take over; bzl
        // failures clear for retry — only packages cache errors).
        Acquire::Ready | Acquire::CycleProceed | Acquire::Failed(_) => BzlBegin::Ready,
    }
}


/// Finish an owned `.bzl` eval (see [`finish_resource`]; bzl failures always retry).
pub(crate) fn finish_bzl_load(sess: &Session, key: &str, ok: bool) {
    finish_resource(
        sess,
        &ResKey::Bzl(key.to_string()),
        if ok {
            FinishOutcome::Ok
        } else {
            FinishOutcome::FailRetry
        },
    );
}


/// A failed package eval must not poison its RESULTS either (the loaded-set twin): targets
/// from the failed package may have partial rows in `results`, so every later consumer could
/// read stale facts. Drop the package's partial entries; a dep re-load re-declares and
/// re-analyzes them cleanly. Cross-package captures completed inside the failed eval are
/// salvaged before this purge, because their result rows belong to another package.
pub(crate) fn purge_partial_package(sess: &Session, pkg: &str) {
    let in_pkg = |label: &str| pkg_of(label).is_some_and(|p| p == pkg);
    sess.results.borrow_mut().retain(|k, _| !in_pkg(k));
    sess.pending.borrow_mut().retain(|k, _| !in_pkg(k));
    sess.fold_cache.borrow_mut().retain(|k, _| !in_pkg(k));
}


