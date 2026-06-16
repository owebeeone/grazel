//! `state::session` — split from `state.rs` (facade in `mod.rs`).

use starlark::any::ProvidesStaticType;
use starlark::eval::Evaluator;
use std::collections::BTreeMap;
use std::path::PathBuf;
use super::*;

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
    pub(crate) eval_stacks: SyncCell<std::collections::HashMap<std::thread::ThreadId, EvalStack>>,
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
    /// Round 29: per-DECLARATION error memo for FAILED native analyses. Native bodies are
    /// FnOnce — the first demander consumes the slot; if its run fails, later consumers would
    /// see only "not analyzed" (wrong reason). The memo serves them the real error.
    pub(crate) native_errors: SyncCell<std::collections::HashMap<String, String>>,
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
    /// crate-universe plan P0.5: the loading-phase graph — canonical label → `LoadedTarget` (raw
    /// attrs, UNRESOLVED selects, typed edges). Populated at capture (record time); edges resolved
    /// by `loaded::finalize_edges` post-load. `razel query` reads this; the build path ignores it.
    pub(crate) loaded_targets: SyncCell<BTreeMap<String, crate::loaded::LoadedTarget>>,
    /// Session-wide `.bzl` module cache (canonical label → frozen module). ONE evaluation per
    /// `.bzl` per Session — provider identities (`dep[MyInfo]` ptr-eq) hold across packages,
    /// and TF's macro layer evaluates once, not per-package.
    pub(crate) bzl_cache:
        SyncCell<std::collections::HashMap<String, starlark::environment::FrozenModule>>,
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
    /// P3.4: canonical names of targets whose `target_compatible_with` is unsatisfied on the
    /// configured platform. The rule records an actionless target + flags it here; the driver
    /// (P3.4b) reads this to SKIP it in wildcard/transitive and LOUD-ERROR when it's named or a
    /// dep of a compatible target. (Bazel's `IncompatiblePlatformProvider`, as a session set.)
    pub(crate) incompatible_targets: SyncCell<std::collections::BTreeSet<String>>,
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
        std::collections::HashMap<
            (std::path::PathBuf, String, String),
            std::sync::Arc<Vec<String>>,
        >,
    >,
    /// Stable ids for aspect values. Cache keys cannot use `Display` (`<aspect>`) and the
    /// Starlark pointer accessor is crate-private, so aspects get a session-local identity.
    pub(crate) aspect_ids: std::sync::atomic::AtomicU64,
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
    /// Fetch R1: repo specs recorded by the `repository_rule` recorder during a WORKSPACE
    /// eval ([`crate::fetch`]). Empty outside extraction.
    pub(crate) repo_specs: SyncCell<Vec<crate::fetch::RepoSpec>>,
}


/// A deferred native-rule analysis body (E0c): the rule fn's work, run by the demand-driven pass.
/// MUST capture only plain data (no starlark `Value`s — they would be invisible to the GC).
pub(crate) type NativeAnalyzeFn =
    Box<dyn for<'v, 'a, 'e> FnOnce(&mut Evaluator<'v, 'a, 'e>) -> anyhow::Result<()> + Send + Sync>;


/// Coerce a closure to [`NativeAnalyzeFn`] (pins the higher-ranked lifetimes for inference).
pub(crate) fn native_decl<F>(f: F) -> NativeAnalyzeFn
where
    F: for<'v, 'a, 'e> FnOnce(&mut Evaluator<'v, 'a, 'e>) -> anyhow::Result<()>
        + Send
        + Sync
        + 'static,
{
    Box::new(f)
}


impl Session {
    pub(crate) fn new(workspace: Option<PathBuf>, global: GlobalFlags) -> Self {
        Session {
            workspace,
            global,
            ..Default::default()
        }
    }

    /// THIS worker's eval stack, mutable (write-locks the map — keep `f` tiny, never recurse
    /// into an eval under it; the [R1] discipline).
    fn with_stack<R>(&self, f: impl FnOnce(&mut EvalStack) -> R) -> R {
        let mut m = self.eval_stacks.borrow_mut();
        f(m.entry(std::thread::current().id()).or_default())
    }

    /// Read-only view (read lock — the hot path: `canon_label`/`qualify` per label).
    fn read_stack<R>(&self, f: impl FnOnce(&EvalStack) -> R) -> Option<R> {
        self.eval_stacks
            .borrow()
            .get(&std::thread::current().id())
            .map(f)
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
        self.read_stack(|s| s.analyzing.contains(label))
            .unwrap_or(false)
    }

    /// Install/replace THIS worker's in-flight target.
    pub(crate) fn set_current_target(&self, t: Option<AnalyzedTarget>) {
        self.with_stack(|s| s.current = t);
    }

    /// Commit: take the in-flight target out (post-impl record).
    pub(crate) fn take_current_target(&self) -> Option<AnalyzedTarget> {
        self.with_stack(|s| s.current.take())
    }

    /// F4: this worker's cross-thread partial-read count (see [`EvalStack::partial_reads`]).
    pub(crate) fn partial_reads(&self) -> usize {
        self.read_stack(|s| s.partial_reads).unwrap_or(0)
    }

    pub(crate) fn note_partial_read(&self) {
        self.with_stack(|s| s.partial_reads += 1);
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


pub(crate) fn with_current<F: FnOnce(&mut AnalyzedTarget)>(sess: &Session, f: F) {
    sess.with_stack(|s| {
        if let Some(c) = s.current.as_mut() {
            f(c);
        }
    });
}


