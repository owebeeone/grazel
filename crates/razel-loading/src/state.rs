//! Per-analysis state + core types + the host-cc tool layer (C0 decomposition of rules.rs).
//! The foundation every other loader module imports. AD2: state is a fresh `Session` per
//! analyze_*, threaded explicitly — no ambient globals.

use starlark::any::ProvidesStaticType;
use starlark::eval::Evaluator;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
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
    /// Headers this target exports to dependents (cc_library `hdrs`, transitively).
    /// Bazel makes these explicit, so they double as the dependents' sandbox inputs.
    pub hdrs: Vec<String>,
    /// Compile flags this target exports to dependents — its `defines` (`-D…`) and
    /// `includes` (`-I…`), transitively. (Local `copts` are NOT exported.)
    pub cflags: Vec<String>,
    /// `JavaInfo(compile_jars=…)` — this target's OWN exported compile jar (java's header/ijar).
    /// **Ordered** (the dependents' classpath order is load-bearing): a dependent folds these
    /// preorder via `fold_compile_jars` (the OrderedDepset analog, B2/B3). Empty for non-java.
    pub compile_jars: Vec<String>,
    /// `JavaInfo(runtime_jars=…)` — this target's OWN runtime jar (the full jar). A SEPARATE ordered
    /// depset from `compile_jars` that does NOT cross-merge (B4): the runtime classpath folds via
    /// `fold_runtime_jars`, independent of the compile classpath.
    pub runtime_jars: Vec<String>,
    /// `JavaInfo(neverlink=True)` — compile-only: on dependents' COMPILE classpath but EXCLUDED from
    /// their runtime closure (`fold_runtime_jars` skips a neverlink node + its subtree). B4.
    pub neverlink: bool,
}

#[derive(Default)]
pub(crate) struct AnalysisState {
    pub(crate) targets: Vec<AnalyzedTarget>,
    pub(crate) current: Option<AnalyzedTarget>,
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
#[derive(Default, ProvidesStaticType)]
pub(crate) struct Session {
    pub(crate) state: RefCell<AnalysisState>,
    /// Analyzed targets by **canonical label** → providers, so a dependent's `deps` reads
    /// them (cross-target/-package provider flow). Bare name in single-package mode,
    /// `//pkg:name` in a workspace. This map is the embryonic DDS fact store.
    pub(crate) results: RefCell<BTreeMap<String, AnalyzedTarget>>,
    /// Toolchain configs declared via `define_config` (host-config selection, D7).
    pub(crate) configs: RefCell<Vec<String>>,
    /// The package currently being evaluated (`None` ⇒ single-package mode).
    pub(crate) current_pkg: RefCell<Option<String>>,
    /// Packages whose BUILD has been loaded (cycle/repeat guard).
    pub(crate) loaded: RefCell<HashSet<String>>,
    /// Workspace root (multi-package mode); `None` ⇒ single-package. Set once.
    pub(crate) workspace: Option<PathBuf>,
    /// CLI flags riding every cc action (`--copt`/`--linkopt`/`-c`). Set once.
    pub(crate) global: GlobalFlags,
    /// The resolved native (host) cc compiler, walked from `PATH` **once per Session** (AD2: not a
    /// process global — F13). `None` until first use; a different analysis (different PATH/toolchain)
    /// re-resolves because it's a fresh `Session`.
    pub(crate) resolved_cc: RefCell<Option<String>>,
}

impl Session {
    pub(crate) fn new(workspace: Option<PathBuf>, global: GlobalFlags) -> Self {
        Session { workspace, global, ..Default::default() }
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
    match &*sess.current_pkg.borrow() {
        None => s.strip_prefix(':').unwrap_or(s).to_string(),
        Some(pkg) => {
            if let Some(rest) = s.strip_prefix("//") {
                format!("//{rest}")
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
    match &*sess.current_pkg.borrow() {
        Some(pkg) => format!("{pkg}/{path}"),
        None => path.to_string(),
    }
}

/// The package of a canonical label `//pkg:name`.
pub(crate) fn pkg_of(label: &str) -> Option<String> {
    label
        .strip_prefix("//")?
        .split_once(':')
        .map(|(p, _)| p.to_string())
}

pub(crate) fn with_current<F: FnOnce(&mut AnalyzedTarget)>(sess: &Session, f: F) {
    if let Some(c) = sess.state.borrow_mut().current.as_mut() {
        f(c);
    }
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
