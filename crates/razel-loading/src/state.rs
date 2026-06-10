//! Per-analysis state + core types + the host-cc tool layer (C0 decomposition of rules.rs).
//! The foundation every other loader module imports. AD2: state is a fresh `Session` per
//! analyze_*, threaded explicitly — no ambient globals.

use razel_dds::{FieldId, FieldValue, ProviderTypeId, Scalar};
use starlark::any::ProvidesStaticType;
use starlark::eval::Evaluator;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;

/// One action registered by a rule impl (`ctx.actions.run`/`write`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnalyzedAction {
    pub mnemonic: String,
    /// Full command: `[executable, args…]` — what the executor spawns.
    pub argv: Vec<String>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

/// The captured analysis of one target: a rule impl ran and produced these.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnalyzedTarget {
    pub name: String,
    /// Resolved dependency target names (from the `deps` attr).
    pub deps: Vec<String>,
    pub actions: Vec<AnalyzedAction>,
    /// `DefaultInfo(files=…)`.
    pub default_info: Vec<String>,
    /// The target's OWN providers (C2d) — the razel-dds value algebra IS the storage: `CcInfo.hdrs`/
    /// `.cflags` are `Set`, `JavaInfo.compile_jars`/`.runtime_jars` are `OrderedDepset`, `.neverlink`
    /// is `Scalar`. The TRANSITIVE closure a dependent sees is a `DdsRead` fold over [`crate::dds`].
    /// (Replaces the former five flat fields — one representation, no hand-reflection.)
    pub providers: BTreeMap<(ProviderTypeId, FieldId), FieldValue>,
}

fn scalar_str(s: &Scalar) -> Option<String> {
    if let Scalar::Str(x) = s { Some(x.clone()) } else { None }
}

impl AnalyzedTarget {
    /// A provider field's string elements (`Set` or `OrderedDepset`), empty if absent. Generic — the
    /// caller names the provider/field (language modules + tests); `state` stays language-free (C3a.5b).
    pub fn field_strs(&self, ty: &str, field: &str) -> Vec<String> {
        match self.providers.get(&(ProviderTypeId::new(ty), FieldId::new(field))) {
            Some(FieldValue::Set(s)) => s.iter().filter_map(scalar_str).collect(),
            Some(FieldValue::OrderedDepset(v)) => v.iter().filter_map(scalar_str).collect(),
            _ => Vec::new(),
        }
    }
    /// A `Scalar(Bool)` provider field (e.g. java `neverlink`), false if absent. Generic.
    pub fn scalar_bool(&self, ty: &str, field: &str) -> bool {
        matches!(
            self.providers.get(&(ProviderTypeId::new(ty), FieldId::new(field))),
            Some(FieldValue::Scalar(Scalar::Bool(true)))
        )
    }
    /// Set a provider field (the capture write — `razel_build.info` + the native rules).
    pub fn set_provider(&mut self, ty: &str, field: &str, value: FieldValue) {
        self.providers.insert((ProviderTypeId::new(ty), FieldId::new(field)), value);
    }
    /// Set a `Set`-valued provider field from strings — the common native-rule capture. Generic: the
    /// rule names its own provider (allowed); `state` stays language-free.
    pub(crate) fn set_set(&mut self, ty: &str, field: &str, values: Vec<String>) {
        self.set_provider(ty, field, FieldValue::Set(values.into_iter().map(Scalar::Str).collect()));
    }
}

#[derive(Default)]
pub(crate) struct AnalysisState {
    pub(crate) targets: Vec<AnalyzedTarget>,
}

/// P4a: ONE WORKER's eval-stack state — the package/repo context, the in-flight target, the
/// mid-analysis guard set. These mirror a single thread's nested-eval call stack and must
/// never be shared: round 23 proved that Session-wide copies corrupt every concurrent eval
/// (labels canonicalize against another worker's package). Keyed by `ThreadId` on the shared
/// Session (AD2: Session-owned, not a process global; dies with the Session).
#[derive(Default)]
pub(crate) struct EvalStack {
    /// The package this worker is currently evaluating (`None` ⇒ single-package mode).
    pub(crate) current_pkg: Option<String>,
    /// The (repo, pkg) of the module this worker is loading/evaluating (lexical binding).
    pub(crate) bzl_repo: Vec<Option<(String, String)>>,
    /// The in-flight `AnalyzedTarget` being built by this worker's current rule analysis.
    pub(crate) current: Option<AnalyzedTarget>,
    /// Targets mid-analysis on THIS worker (cycle detection; cross-worker duplicate analysis
    /// is allowed — results overwrite by label, the established idempotent re-analysis).
    pub(crate) analyzing: HashSet<String>,
}

/// Per-analysis state, threaded explicitly — the precursor of the DDS (RazelV2Contracts §0)
/// and razel's answer to AD2 (no ambient state). Built fresh per `analyze_*` call (so there is
/// no `reset` to forget), stashed in `eval.extra`, and read by builtins via [`session`];
/// non-builtin helpers take `&Session`. Interior mutability (`RefCell`) on the fields mutated
/// during eval; `workspace`/`global` are set once at construction.
///
/// Re-entrant under nested package loads (`resolve_dep` → `load_package` → nested
/// `eval_build_src`): every borrow is kept short and **never held across an `eval_*` call**
/// (the [R1] discipline — a held `results`/`state` borrow across a nested eval would
/// double-borrow-panic). Multiple `Session`s coexist → multi-instance analysis (F24).
/// P1 (eval worker-pool plan): a `RefCell`-shaped wrapper over `RwLock` — same
/// `borrow()`/`borrow_mut()` API, so the ~27 Session fields convert without touching call
/// sites. RefCell's discipline ([R1]: never hold a borrow across a nested eval) becomes lock
/// discipline; a violation deadlocks where RefCell panicked — caught by the suite/sentinels.
#[derive(Default)]
pub(crate) struct SyncCell<T>(std::sync::RwLock<T>);

impl<T> SyncCell<T> {
    pub(crate) fn borrow(&self) -> std::sync::RwLockReadGuard<'_, T> {
        self.0.read().expect("SyncCell poisoned")
    }
    pub(crate) fn borrow_mut(&self) -> std::sync::RwLockWriteGuard<'_, T> {
        self.0.write().expect("SyncCell poisoned")
    }
}

#[derive(Default, ProvidesStaticType)]
pub(crate) struct Session {
    pub(crate) state: SyncCell<AnalysisState>,
    /// Analyzed targets by **canonical label** → providers, so a dependent's `deps` reads
    /// them (cross-target/-package provider flow). Bare name in single-package mode,
    /// `//pkg:name` in a workspace. This map is the embryonic DDS fact store.
    pub(crate) results: SyncCell<BTreeMap<String, AnalyzedTarget>>,
    /// Toolchain configs declared via `define_config` (host-config selection, D7).
    pub(crate) configs: SyncCell<Vec<String>>,
    /// P4a: per-worker eval-stack state (see [`EvalStack`]). Access via the `current_pkg()`/
    /// `set_current_pkg()`/`bzl_repo_*`/`analyzing_*`/`*_current_target` accessors only.
    pub(crate) eval_stacks:
        SyncCell<std::collections::HashMap<std::thread::ThreadId, EvalStack>>,
    /// The single-flight WAIT GRAPH: per-package + per-`.bzl` load state AND the waits-for
    /// edges (P3 + P4a — see [`acquire_resource`]). `InFlight(thread)` lets a demanding
    /// worker WAIT for the owner; detected cycles take over instead of deadlocking.
    /// Paired Mutex+Condvar (not SyncCell): waiting needs the condvar.
    pub(crate) loaded: std::sync::Mutex<WaitGraph>,
    pub(crate) loaded_cv: std::sync::Condvar,
    /// Workspace root (multi-package mode); `None` ⇒ single-package. Set once.
    pub(crate) workspace: Option<PathBuf>,
    /// CLI flags riding every cc action (`--copt`/`--linkopt`/`-c`). Set once.
    pub(crate) global: GlobalFlags,
    /// The resolved native (host) cc compiler, walked from `PATH` **once per Session** (AD2: not a
    /// process global — F13). `None` until first use; a different analysis (different PATH/toolchain)
    /// re-resolves because it's a fresh `Session`.
    pub(crate) resolved_cc: SyncCell<Option<String>>,
    /// E0 phase split: declared-but-not-yet-analyzed targets (canonical label → index into the
    /// current package's declaration store). Registered at record time (BUILD eval), consumed by the
    /// demand-driven analysis pass — this is what makes forward references resolve. Entries belong to
    /// the package currently being driven; a nested `load_package` drains its own before returning.
    pub(crate) pending: SyncCell<BTreeMap<String, usize>>,
    /// E0c: deferred native-rule analysis bodies, indexed by the declaration store's
    /// `DeclBody::Native` slots. Off-heap (the closures capture only plain unpacked attrs — no
    /// `Value`s — so they need no GC tracing and can live on the Session).
    pub(crate) native_decls: SyncCell<Vec<Option<NativeAnalyzeFn>>>,
    /// Undriven NATIVE decls of completed packages: label → `native_decls` index. Native
    /// bodies capture only plain data, so they run on demand in any later eval (the
    /// cross-package twin of `deferred_decls`).
    pub(crate) deferred_natives: SyncCell<std::collections::HashMap<String, usize>>,
    /// Output-FILE labels → (producing target, qualified output path). Registered at DECLARE
    /// time (genrule outs are static), so file labels naming generated outputs resolve.
    pub(crate) output_index: SyncCell<std::collections::HashMap<String, (String, String)>>,
    /// `alias()` targets (canonical name → canonical actual) — conditions resolve through them.
    pub(crate) aliases: SyncCell<BTreeMap<String, String>>,
    /// Declared `config_setting` specs by canonical label — what `select()` matches (razelV3).
    pub(crate) config_specs: SyncCell<BTreeMap<String, ConfigSpec>>,
    /// Session-wide `.bzl` module cache (canonical label → frozen module). ONE evaluation per
    /// `.bzl` per Session — provider identities (`dep[MyInfo]` ptr-eq) hold across packages,
    /// and TF's macro layer evaluates once, not per-package.
    pub(crate) bzl_cache: SyncCell<std::collections::HashMap<String, starlark::environment::FrozenModule>>,
    /// Harvested UNDRIVEN Starlark declarations (one frozen dict per dependency-loaded
    /// package) — analyzed on demand cross-package ([`crate::dialect`] `analyze_deferred`).
    pub(crate) deferred_decls: SyncCell<Vec<starlark::values::OwnedFrozenValue>>,
    /// label → index into `deferred_decls` (the harvest owner) — O(1) demand lookups; the
    /// linear all-packages scan was quadratic on tree sweeps.
    pub(crate) deferred_index: SyncCell<std::collections::HashMap<String, usize>>,
    /// label → index into `cross_captured` (same fix for provider-instance lookups).
    pub(crate) cross_index: SyncCell<std::collections::HashMap<String, usize>>,
    /// Once-per-session warning keys (tree sweeps turned per-call warnings into log storms).
    pub(crate) warned: SyncCell<std::collections::BTreeSet<String>>,
    /// Per-target transitive-fold memo (label → folded fields). The DDS fold was the tree-sweep
    /// hotspot: every dep edge re-walked its transitive closure; diamonds made it quadratic.
    pub(crate) fold_cache: SyncCell<std::collections::HashMap<String, Vec<(String, Vec<String>)>>>,
    /// Host tool discovery memo (rustc path, cc path, sysroot): these were probed via
    /// PATH walks + an `xcrun` SPAWN on EVERY ctx construction — two-thirds of sweep CPU
    /// was kernel time. Environment-derived, constant per session.
    pub(crate) host_tools: SyncCell<Option<(String, String, String)>>,
    /// Filesystem caches (the sweep profile was 60% `stat`/`getdirentries`): file-existence
    /// memo for the file-label fallbacks + per-directory RECURSIVE walk memo for glob()
    /// (it re-walked whole package trees per call).
    pub(crate) exists_cache: SyncCell<std::collections::HashMap<std::path::PathBuf, bool>>,
    pub(crate) walk_cache:
        SyncCell<std::collections::HashMap<std::path::PathBuf, std::sync::Arc<Vec<String>>>>,
    /// glob() RESULT memo, keyed (package dir, include, exclude) — after the walk cache,
    /// pattern-matching huge trees per call was the top CPU frame (TF macros repeat globs).
    pub(crate) glob_cache: SyncCell<
        std::collections::HashMap<(std::path::PathBuf, String, String), std::sync::Arc<Vec<String>>>,
    >,
    /// Pre-parsed BUILD ASTs (key = the eval name, `{pkg}/BUILD`): read+parse is pure and
    /// parallelizes across files; the sequential eval consumes them (load+parse / execute split).
    pub(crate) ast_cache: SyncCell<std::collections::HashMap<String, starlark::syntax::AstModule>>,
    /// Layer 0: harvested provider instances from COMPLETED packages (one frozen dict per
    /// package: canonical label → [(constructor, instance)]). OwnedFrozenValues keep their
    /// heaps alive; `dep[P]` falls back here for cross-package instances.
    pub(crate) cross_captured: SyncCell<Vec<starlark::values::OwnedFrozenValue>>,
    /// E0d: the Session's live fact store — the DDS IS the store. `None` until first use (lazy
    /// schema registration); access via [`crate::dds::session_dds`]. Targets assert incrementally
    /// at `record_target`; folds read this directly (no per-dep rebuild — O(n), not O(n²)).
    pub(crate) dds: SyncCell<Option<razel_dds::Dds>>,
}

/// A deferred native-rule analysis body (E0c): the rule fn's work, run by the demand-driven pass.
/// MUST capture only plain data (no starlark `Value`s — they would be invisible to the GC).
pub(crate) type NativeAnalyzeFn =
    Box<dyn for<'v, 'a, 'e> FnOnce(&mut Evaluator<'v, 'a, 'e>) -> anyhow::Result<()> + Send + Sync>;

/// Coerce a closure to [`NativeAnalyzeFn`] (pins the higher-ranked lifetimes for inference).
pub(crate) fn native_decl<F>(f: F) -> NativeAnalyzeFn
where
    F: for<'v, 'a, 'e> FnOnce(&mut Evaluator<'v, 'a, 'e>) -> anyhow::Result<()> + Send + Sync + 'static,
{
    Box::new(f)
}

#[derive(Clone, Debug)]
pub(crate) enum PkgState {
    InFlight(std::thread::ThreadId),
    Done,
}

/// The single-flight WAIT GRAPH (P4a): packages AND `.bzl` modules in one keyed map (keys are
/// disjoint — `.bzl` keys carry a `:`, package keys never do), plus the waits-for edges that
/// make cross-thread demand cycles DETECTABLE. Package↔bzl cycles are real (worker A's package
/// loads a module owned by worker B whose eval demand-loads A's package), so the two resource
/// kinds must share one graph and one lock.
#[derive(Default)]
pub(crate) struct WaitGraph {
    pub(crate) res: std::collections::HashMap<String, PkgState>,
    /// worker → the resource key it is blocked on (one edge per parked worker).
    pub(crate) waiting: std::collections::HashMap<std::thread::ThreadId, String>,
    /// CONCURRENT evaluations per key (cycle/timeout takeovers duplicate a load while the
    /// original owner is still evaluating). Failure cleanup must respect survivors: a failed
    /// finisher may only purge when it is the LAST live eval and nobody succeeded.
    pub(crate) live: std::collections::HashMap<String, usize>,
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
}

/// Single-flight acquire with DEADLOCK-FREE waiting: before parking, walk the waits-for chain
/// from the owner; if it reaches a resource THIS thread owns, the wait would deadlock — take
/// the load over NOW instead (duplicate eval is waste, not wrong: results overwrite by label,
/// the bzl cache by key). Package-level demand cycles are LEGAL in Bazel's model, and at TF
/// scale they are dense — the previous 20s-timeout-only takeover degenerated into an
/// hours-long livelock (every cycle edge cost a 20s sleep). The timeout stays as a backstop
/// for waits the graph cannot see (it should never fire in practice — it prints loudly).
pub(crate) fn acquire_resource(sess: &Session, key: &str) -> Acquire {
    let me = std::thread::current().id();
    let mut g = sess.loaded.lock().expect("loaded poisoned");
    loop {
        match g.res.get(key) {
            Some(PkgState::Done) => return Acquire::Ready,
            Some(PkgState::InFlight(tid)) if *tid == me => return Acquire::Reentry,
            Some(PkgState::InFlight(owner)) => {
                // Cycle check: owner → (what owner waits on) → its owner → … → me?
                let mut cur = *owner;
                let mut cycle = false;
                for _ in 0..128 {
                    let Some(next_key) = g.waiting.get(&cur) else { break };
                    match g.res.get(next_key) {
                        Some(PkgState::InFlight(o2)) => {
                            if *o2 == me {
                                cycle = true;
                                break;
                            }
                            cur = *o2;
                        }
                        _ => break,
                    }
                }
                if cycle {
                    // Package keys (no `:`): sequential re-entry semantics — proceed against
                    // the owner's partial state, no duplicate eval. `.bzl` keys: the module
                    // VALUE is required, so take the eval over (duplicate; converges — the
                    // worst case is an illegal load cycle, which the eval reports loudly).
                    if !key.contains(':') {
                        return Acquire::CycleProceed;
                    }
                    g.res.insert(key.to_string(), PkgState::InFlight(me));
                    *g.live.entry(key.to_string()).or_default() += 1;
                    return Acquire::Own;
                }
                g.waiting.insert(me, key.to_string());
                let (g2, t) = sess
                    .loaded_cv
                    .wait_timeout(g, std::time::Duration::from_secs(20))
                    .expect("loaded poisoned");
                g = g2;
                g.waiting.remove(&me);
                if t.timed_out() {
                    eprintln!(
                        "razel: warning: load wait timed out (unseen cycle?); duplicating `{key}`"
                    );
                    g.res.insert(key.to_string(), PkgState::InFlight(me));
                    *g.live.entry(key.to_string()).or_default() += 1;
                    return Acquire::Own;
                }
            }
            None => {
                g.res.insert(key.to_string(), PkgState::InFlight(me));
                *g.live.entry(key.to_string()).or_default() += 1;
                return Acquire::Own;
            }
        }
    }
}

/// Finish an owned resource: Done on success (any cache insert MUST precede this — Ready
/// readers consult it), clear on failure (retryable); wake waiters. Returns whether the
/// caller may run FAILURE CLEANUP (purge): only the LAST live eval of the key may, and only
/// when no concurrent eval succeeded — a takeover duplicate failing mid-way must not clobber
/// the original owner's in-progress (or completed) results.
pub(crate) fn finish_resource(sess: &Session, key: &str, ok: bool) -> bool {
    let mut g = sess.loaded.lock().expect("loaded poisoned");
    let live = g.live.entry(key.to_string()).or_default();
    *live = live.saturating_sub(1);
    let last = *live == 0;
    let may_purge = if ok {
        g.res.insert(key.to_string(), PkgState::Done);
        false
    } else if matches!(g.res.get(key), Some(PkgState::Done)) {
        // A concurrent eval already succeeded — this failure is moot; keep Done.
        false
    } else if last {
        g.res.remove(key);
        true
    } else {
        // Survivors are still evaluating: leave THEIR InFlight claim in place (last-writer
        // state may carry our id — restore is impossible without per-owner slots; the
        // survivors' finish overwrites it) and do NOT purge.
        false
    };
    sess.loaded_cv.notify_all();
    may_purge
}

/// Begin loading `pkg`. True when THIS caller owns the load; false when already done or
/// same-thread re-entry. Cross-thread in-flight: waits (cycle-safe — see [`acquire_resource`]).
pub(crate) fn begin_pkg_load(sess: &Session, pkg: &str) -> bool {
    matches!(acquire_resource(sess, pkg), Acquire::Own)
}

/// Finish a load this caller owned (see [`finish_resource`]); a failure also purges the
/// package's partial results — but only when this was the LAST live eval of the package
/// (takeover duplicates must not clobber a surviving owner's rows).
pub(crate) fn finish_pkg_load(sess: &Session, pkg: &str, ok: bool) {
    if finish_resource(sess, pkg, ok) {
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
    match acquire_resource(sess, key) {
        Acquire::Own | Acquire::Reentry => BzlBegin::Own,
        // CycleProceed is unreachable for `:`-keys (acquire takes bzl cycles over).
        Acquire::Ready | Acquire::CycleProceed => BzlBegin::Ready,
    }
}

/// Finish an owned `.bzl` eval (see [`finish_resource`]).
pub(crate) fn finish_bzl_load(sess: &Session, key: &str, ok: bool) {
    finish_resource(sess, key, ok);
}

/// A failed package eval must not poison its RESULTS either (the loaded-set twin): targets
/// analyzed before the failure sit in `results` with their captured provider instances DEAD
/// (the harvest is skipped on error), so every later consumer would read providerless deps
/// (`have 0 pairs`) — the TF enable_registration_v2 class. Drop the package's partial
/// entries; a dep re-load re-declares and re-analyzes them cleanly (the same idempotent
/// re-analysis consumers already do for harvested decls).
fn purge_partial_package(sess: &Session, pkg: &str) {
    let in_pkg = |label: &str| pkg_of(label).is_some_and(|p| p == pkg);
    sess.results.borrow_mut().retain(|k, _| !in_pkg(k));
    sess.pending.borrow_mut().retain(|k, _| !in_pkg(k));
    sess.fold_cache.borrow_mut().retain(|k, _| !in_pkg(k));
}

impl Session {
    pub(crate) fn new(workspace: Option<PathBuf>, global: GlobalFlags) -> Self {
        Session { workspace, global, ..Default::default() }
    }

    /// THIS worker's eval stack, mutable (write-locks the map — keep `f` tiny, never recurse
    /// into an eval under it; the [R1] discipline).
    fn with_stack<R>(&self, f: impl FnOnce(&mut EvalStack) -> R) -> R {
        let mut m = self.eval_stacks.borrow_mut();
        f(m.entry(std::thread::current().id()).or_default())
    }

    /// Read-only view (read lock — the hot path: `canon_label`/`qualify` per label).
    fn read_stack<R>(&self, f: impl FnOnce(&EvalStack) -> R) -> Option<R> {
        self.eval_stacks.borrow().get(&std::thread::current().id()).map(f)
    }

    /// The package THIS worker is evaluating (`None` ⇒ single-package mode).
    pub(crate) fn current_pkg(&self) -> Option<String> {
        self.read_stack(|s| s.current_pkg.clone()).flatten()
    }

    /// Set this worker's current package; returns the previous value (save/restore pairs).
    pub(crate) fn set_current_pkg(&self, pkg: Option<String>) -> Option<String> {
        self.with_stack(|s| std::mem::replace(&mut s.current_pkg, pkg))
    }

    pub(crate) fn bzl_repo_push(&self, ctx: Option<(String, String)>) {
        self.with_stack(|s| s.bzl_repo.push(ctx));
    }

    pub(crate) fn bzl_repo_pop(&self) {
        self.with_stack(|s| {
            s.bzl_repo.pop();
        });
    }

    /// The innermost module context of THIS worker (`None` ⇒ no module on the stack).
    pub(crate) fn bzl_repo_last(&self) -> Option<Option<(String, String)>> {
        self.read_stack(|s| s.bzl_repo.last().cloned()).flatten()
    }

    /// Cycle-guard insert for THIS worker (true = newly inserted, proceed).
    pub(crate) fn analyzing_insert(&self, label: &str) -> bool {
        self.with_stack(|s| s.analyzing.insert(label.to_string()))
    }

    pub(crate) fn analyzing_remove(&self, label: &str) {
        self.with_stack(|s| {
            s.analyzing.remove(label);
        });
    }

    pub(crate) fn analyzing_contains(&self, label: &str) -> bool {
        self.read_stack(|s| s.analyzing.contains(label)).unwrap_or(false)
    }

    /// Install/replace THIS worker's in-flight target.
    pub(crate) fn set_current_target(&self, t: Option<AnalyzedTarget>) {
        self.with_stack(|s| s.current = t);
    }

    /// Commit: take the in-flight target out (post-impl record).
    pub(crate) fn take_current_target(&self) -> Option<AnalyzedTarget> {
        self.with_stack(|s| s.current.take())
    }

    /// The resolved native (host) cc compiler — walked from `PATH` once per Session (§7 ·iii), cached
    /// on the Session (AD2: not a process global — F13; the pure walk is `first_on_path`, unit-tested).
    /// Fallback: `CXX`.
    pub(crate) fn host_cc(&self) -> String {
        if let Some(cc) = self.resolved_cc.borrow().as_ref() {
            return cc.clone();
        }
        let path = std::env::var("PATH").unwrap_or_default();
        let dirs: Vec<&str> = path.split(':').collect();
        let cc = first_on_path(&["c++", "clang++", "g++", "cc"], &dirs, |p| p.is_file())
            .unwrap_or_else(|| CXX.to_string());
        eprintln!("razel: native cc toolchain → {cc} (id {})", tool_id(&cc));
        *self.resolved_cc.borrow_mut() = Some(cc.clone());
        cc
    }
    /// Take the accumulated targets out (consumes the in-flight `state.targets`).
    pub(crate) fn take_targets(&self) -> Vec<AnalyzedTarget> {
        std::mem::take(&mut self.state.borrow_mut().targets)
    }
}

/// The per-analysis [`Session`] stashed in `eval.extra` by the analysis entry points.
/// Panics only on a programming error: a builtin reached without an initialized analysis.
pub(crate) fn session<'a>(eval: &Evaluator<'_, 'a, '_>) -> &'a Session {
    eval.extra
        .expect("analysis not initialized: Session missing from eval.extra")
        .downcast_ref::<Session>()
        .expect("eval.extra is not a Session")
}

/// Build-wide flags from the command line that ride every cc action: `copts` prepend
/// to every compile (so `-c opt` / `--copt`/`--cxxopt`/`--conlyopt`/`--define` take
/// effect), `linkopts` append to every link (`--linkopt`). Per-target attrs still apply.
#[derive(Debug, Clone, Default)]
pub struct GlobalFlags {
    pub copts: Vec<String>,
    pub linkopts: Vec<String>,
    /// Which cc toolchain to use (§7): Native (host compiler, executable — default) or AdoptBazel
    /// (Bazel's faithful declared graph, for the parity runner).
    pub cc_toolchain: CcToolchainMode,
    /// External-repo base (D4): where vendored `@repo` sources live (e.g. `third-party/`). An
    /// `@repo//pkg:file` load resolves to `<base>/<repo>/pkg/file`, with `_`→`-` name tolerance
    /// (canonical `@bazel_skylib` ↔ dir `bazel-skylib`). `None` ⇒ external loads not configured.
    pub external_base: Option<PathBuf>,
    /// `-c`/`--compilation_mode` as STRUCTURED configuration (`config_setting` matching reads
    /// this; the cc flag expansion into `copts` is separate, done by the CLI). Empty ⇒ Bazel's
    /// default `fastbuild`.
    pub compilation_mode: String,
    /// `--define k=v` pairs as structured configuration (`config_setting` `define_values` /
    /// `values = {"define": "k=v"}`).
    pub defines: Vec<(String, String)>,
}

impl GlobalFlags {
    /// The effective compilation mode (`fastbuild` when unset — Bazel's default).
    pub(crate) fn mode(&self) -> &str {
        if self.compilation_mode.is_empty() { "fastbuild" } else { &self.compilation_mode }
    }
}

/// Bazel's name for the host CPU (`--cpu` default): `darwin_arm64`/`darwin_x86_64`/`k8`/`aarch64`.
pub(crate) fn host_cpu() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "darwin_arm64",
        ("macos", _) => "darwin_x86_64",
        ("linux", "x86_64") => "k8",
        ("linux", "aarch64") => "aarch64",
        _ => "unknown",
    }
}

/// Does a `constraint_value` (package family, value name) describe the REAL host? `@platforms`'
/// os/cpu families match the host; foreign constraint families are conservative-false.
pub(crate) fn host_constraint_matches(pkg: &str, name: &str) -> bool {
    let fam = pkg.rsplit('/').next().unwrap_or(pkg).trim_start_matches("@platforms//");
    match fam {
        "os" => match std::env::consts::OS {
            "macos" => matches!(name, "osx" | "macos"),
            os => name == os,
        },
        "cpu" => match std::env::consts::ARCH {
            "aarch64" => matches!(name, "arm64" | "aarch64"),
            arch => name == arch,
        },
        _ => false,
    }
}

/// A `config_setting`'s declared constraints — what `select()` matches against the configuration.
/// `values` keys razel models: `compilation_mode`, `define` (`"k=v"`); an unmodeled key errors
/// loudly at match time (never a silent non-match).
#[derive(Debug, Clone, Default)]
pub(crate) struct ConfigSpec {
    pub(crate) values: BTreeMap<String, String>,
    pub(crate) define_values: BTreeMap<String, String>,
    /// A `config_setting_group` (skylib): (true ⇒ match_all, false ⇒ match_any) over members.
    pub(crate) group: Option<(bool, Vec<String>)>,
    /// `flag_values =` constraints reference build-setting values razel doesn't model yet —
    /// CONSERVATIVE: such a condition never matches (CPU-host posture; registered debt).
    pub(crate) unmodeled: bool,
    /// `constraint_values =` labels (`@platforms//os:x`), matched against the REAL host.
    pub(crate) constraint_values: Vec<String>,
}

impl ConfigSpec {
    /// Does every constraint hold against the configuration?
    pub(crate) fn matches(&self, flags: &GlobalFlags) -> Result<bool, String> {
        let has_define =
            |k: &str, v: &str| flags.defines.iter().any(|(dk, dv)| dk == k && dv == v);
        for (key, want) in &self.values {
            let ok = match key.as_str() {
                "compilation_mode" => flags.mode() == want,
                "define" => match want.split_once('=') {
                    Some((k, v)) => has_define(k, v),
                    None => return Err(format!("config_setting `define` value `{want}` is not k=v")),
                },
                "cpu" => want == host_cpu(),
                // Unmodeled host-config keys (crosstool_top, apple cpus, …): CONSERVATIVE — the
                // condition doesn't match on this host. Loud once per key (registered debt).
                other => {
                    eprintln!(
                        "razel: warning: config_setting key `{other}` not modeled — treating \
                         condition as non-matching"
                    );
                    false
                }
            };
            if !ok {
                return Ok(false);
            }
        }
        if !self.define_values.iter().all(|(k, v)| has_define(k, v)) {
            return Ok(false);
        }
        // constraint_values: every listed @platforms constraint must hold on the REAL host.
        Ok(self.constraint_values.iter().all(|c| {
            c.strip_prefix("@platforms//")
                .and_then(|rest| rest.split_once(':'))
                .map(|(pkg, name)| host_constraint_matches(pkg, name))
                // Non-@platforms constraint families: conservative non-match.
                .unwrap_or(false)
        }))
    }

    /// All constraints as one comparable set (for the most-specialized-wins rule).
    pub(crate) fn constraints(&self) -> BTreeSet<(String, String, String)> {
        self.values
            .iter()
            .map(|(k, v)| ("v".to_string(), k.clone(), v.clone()))
            .chain(self.define_values.iter().map(|(k, v)| ("d".to_string(), k.clone(), v.clone())))
            .collect()
    }
}

/// The cc toolchain mode (RazelStarlarkBoundaryPlan §7) — the resolution to declared-vs-executable.
/// The parity context wants faithful (AdoptBazel); the build context wants runnable (Native).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CcToolchainMode {
    /// Resolve + run the host compiler — an executable graph (razel-build's path). **NOT Bazel-parity-
    /// tested** and never will be without toolchain materialization: this is razel's runnable lowering
    /// (host cc + simple flags), distinct from Bazel's declared graph. Only `AdoptBazel` is golden-
    /// tested; the characterization pins Native's *own* output, not Bazel parity (F18). Converging the
    /// two — running the declared graph as the executed graph — is Phase C/D (RazelGaps).
    #[default]
    Native,
    /// Bazel's faithful declared graph (`cc_wrapper.sh` + `bazel-out`) over razel's `cc:defs.bzl`;
    /// the graph-parity runner's path (declares + diffs, never executes). The ONLY golden-tested mode.
    AdoptBazel,
}

/// Canonicalize a target name/label against the current package. Single-package
/// mode keeps bare names; workspace mode produces `//pkg:name`.
pub(crate) fn canon_label(sess: &Session, s: &str) -> String {
    // Package shorthand: `//a/b` ≡ `//a/b:b` (same for `@repo//a/b`).
    let expand = |label: String| -> String {
        if let Some(rest) = label.rsplit("//").next()
            && !rest.contains(':')
            && !rest.is_empty()
        {
            let last = rest.rsplit('/').next().unwrap_or(rest);
            return format!("{label}:{last}");
        }
        label
    };
    // An `@repo//…` label is already canonical (external labels don't take the current package).
    if s.starts_with('@') {
        // Bare `@repo` shorthand ≡ `@repo//:repo`.
        if !s.contains("//") {
            let name = s.trim_start_matches('@');
            return format!("{s}//:{name}");
        }
        return expand(s.to_string());
    }
    match sess.current_pkg() {
        None => s.strip_prefix(':').unwrap_or(s).to_string(),
        // Inside an EXTERNAL package (`current_pkg == "@repo//pkg"`): labels resolve within that
        // repo — `//x:y` → `@repo//x:y`; `:n`/`n` → `@repo//pkg:n` (Bazel label semantics).
        Some(pkg) if pkg.starts_with('@') => {
            let repo = pkg.split("//").next().unwrap_or_default();
            if let Some(rest) = s.strip_prefix("//") {
                expand(format!("{repo}//{rest}"))
            } else if let Some(name) = s.strip_prefix(':') {
                format!("{pkg}:{name}")
            } else {
                format!("{pkg}:{s}")
            }
        }
        Some(pkg) => {
            if let Some(rest) = s.strip_prefix("//") {
                expand(format!("//{rest}"))
            } else if let Some(name) = s.strip_prefix(':') {
                format!("//{pkg}:{name}")
            } else {
                format!("//{pkg}:{s}")
            }
        }
    }
}

/// Package-qualify a source/output path (`x.cc` → `pkg/x.cc` in workspace mode).
pub(crate) fn qualify(sess: &Session, path: &str) -> String {
    match sess.current_pkg() {
        // External package: FILE paths take Bazel's exec-root form, `external/<repo>/<pkg>/…`
        // (`@repo//pkg` is a label, not a path).
        Some(pkg) => match pkg.strip_prefix('@') {
            Some(rest) => match rest.split_once("//") {
                Some((repo, sub)) if sub.is_empty() => format!("external/{repo}/{path}"),
                Some((repo, sub)) => format!("external/{repo}/{sub}/{path}"),
                None => format!("external/{rest}/{path}"),
            },
            None if pkg.is_empty() => path.to_string(),
            None => format!("{pkg}/{path}"),
        },
        None => path.to_string(),
    }
}

/// The package of a canonical label `//pkg:name`.
pub(crate) fn pkg_of(label: &str) -> Option<String> {
    // External: `@repo//pkg:name` → `@repo//pkg` (an external-package key for load_package).
    if let Some(rest) = label.strip_prefix('@') {
        let (repo, pkgname) = rest.split_once("//")?;
        let (pkg, _) = pkgname.split_once(':')?;
        return Some(format!("@{repo}//{pkg}"));
    }
    label
        .strip_prefix("//")?
        .split_once(':')
        .map(|(p, _)| p.to_string())
}

pub(crate) fn with_current<F: FnOnce(&mut AnalyzedTarget)>(sess: &Session, f: F) {
    sess.with_stack(|s| {
        if let Some(c) = s.current.as_mut() {
            f(c);
        }
    });
}

const CXX: &str = "/usr/bin/c++";
pub(crate) const AR: &str = "/usr/bin/ar";

/// The resolved native (host) cc compiler, by walking `PATH` (§7 ·iii — the Native toolchain). This
/// is what Bazel's `cc_configure` does (probe the host); razel does it at build time. Resolved +
/// logged **once**; `CXX` is the fallback when no candidate is on `PATH`.
// (the host-cc resolver now lives on `Session::host_cc` — AD2: per-Session, not a process-global
// OnceLock; F13. The pure PATH-walk is `first_on_path`, the identity is `tool_id`.)

/// First `<dir>/<candidate>` for which `exists` holds — PATH-walk, candidates in preference order.
/// Pure (dirs + probe injected) so it's testable without touching the environment.
fn first_on_path(
    candidates: &[&str],
    dirs: &[&str],
    exists: impl Fn(&std::path::Path) -> bool,
) -> Option<String> {
    for cand in candidates {
        for dir in dirs {
            let p = std::path::Path::new(dir).join(cand);
            if exists(&p) {
                return Some(p.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// A cheap stable identity for a resolved tool: `size@mtime` from one stat — the fast-path proxy.
/// (The content digest that actually keys actions is the follow-on, RazelGaps "toolchain-change
/// cache"; this is enough to log + later gate the re-hash.)
fn tool_id(path: &str) -> String {
    match std::fs::metadata(path) {
        Ok(m) => {
            let secs = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!("{}b@{secs}", m.len())
        }
        Err(_) => "absent".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_on_path_picks_first_existing_candidate_in_preference_order() {
        // §7 ·iii native cc resolution: candidate order wins within a dir; dirs scanned per candidate.
        let dirs = ["/nope", "/usr/bin", "/usr/local/bin"];
        // Only c++ and clang++ "exist", in different dirs — c++ wins (earlier candidate).
        let present = |p: &std::path::Path| {
            p == std::path::Path::new("/usr/bin/c++")
                || p == std::path::Path::new("/usr/local/bin/clang++")
        };
        assert_eq!(first_on_path(&["c++", "clang++"], &dirs, present).as_deref(), Some("/usr/bin/c++"));
        // Falls through candidates when the preferred one is absent anywhere.
        let only_clang = |p: &std::path::Path| p == std::path::Path::new("/usr/local/bin/clang++");
        assert_eq!(
            first_on_path(&["c++", "clang++"], &dirs, only_clang).as_deref(),
            Some("/usr/local/bin/clang++")
        );
        // None present → None (host_cc then falls back to CXX).
        assert_eq!(first_on_path(&["c++"], &dirs, |_| false), None);
    }
}


/// Cached file-existence check (the dep/file-label fallbacks stat per srcs entry).
pub(crate) fn path_is_file(sess: &Session, p: &std::path::Path) -> bool {
    if let Some(&hit) = sess.exists_cache.borrow().get(p) {
        return hit;
    }
    let v = p.is_file();
    sess.exists_cache.borrow_mut().insert(p.to_path_buf(), v);
    v
}

/// Cached RECURSIVE file listing under `dir` (paths relative to it) for glob().
pub(crate) fn walk_cached(sess: &Session, dir: &std::path::Path) -> std::sync::Arc<Vec<String>> {
    if let Some(hit) = sess.walk_cache.borrow().get(dir) {
        return hit.clone();
    }
    let mut files = Vec::new();
    crate::glob::walk_files(dir, dir, &mut files);
    let arc = std::sync::Arc::new(files);
    sess.walk_cache.borrow_mut().insert(dir.to_path_buf(), arc.clone());
    arc
}

/// The session's host tool triple (rustc, cc, sysroot) — discovered ONCE per session.
pub(crate) fn host_tools(sess: &Session) -> (String, String, String) {
    if let Some(t) = sess.host_tools.borrow().as_ref() {
        return t.clone();
    }
    let find = |name: &str| -> String {
        std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .map(|d| std::path::Path::new(d).join(name))
            .find(|p| p.is_file())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| name.to_string())
    };
    let sysroot = if std::env::consts::OS == "macos" {
        std::process::Command::new("xcrun")
            .args(["--show-sdk-path"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/".to_string())
    } else {
        "/".to_string()
    };
    let t = (find("rustc"), find("cc"), sysroot);
    *sess.host_tools.borrow_mut() = Some(t.clone());
    t
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
