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
    /// E0 phase split: declared-but-not-yet-analyzed targets (canonical label → index into the
    /// current package's declaration store). Registered at record time (BUILD eval), consumed by the
    /// demand-driven analysis pass — this is what makes forward references resolve. Entries belong to
    /// the package currently being driven; a nested `load_package` drains its own before returning.
    pub(crate) pending: RefCell<BTreeMap<String, usize>>,
    /// Targets currently mid-analysis (cycle detection for the demand-driven pass).
    pub(crate) analyzing: RefCell<HashSet<String>>,
    /// E0c: deferred native-rule analysis bodies, indexed by the declaration store's
    /// `DeclBody::Native` slots. Off-heap (the closures capture only plain unpacked attrs — no
    /// `Value`s — so they need no GC tracing and can live on the Session).
    pub(crate) native_decls: RefCell<Vec<Option<NativeAnalyzeFn>>>,
    /// The (repo, pkg) of the module currently being loaded/evaluated (BzlLoader + BUILD-eval
    /// maintained; repo == "" ⇒ the main workspace). String labels written in module code —
    /// `Label()`, select keys, attr DEFAULTS — bind against it (Bazel's lexical binding).
    pub(crate) current_bzl_repo: RefCell<Vec<Option<(String, String)>>>,
    /// `alias()` targets (canonical name → canonical actual) — conditions resolve through them.
    pub(crate) aliases: RefCell<BTreeMap<String, String>>,
    /// Declared `config_setting` specs by canonical label — what `select()` matches (razelV3).
    pub(crate) config_specs: RefCell<BTreeMap<String, ConfigSpec>>,
    /// Session-wide `.bzl` module cache (canonical label → frozen module). ONE evaluation per
    /// `.bzl` per Session — provider identities (`dep[MyInfo]` ptr-eq) hold across packages,
    /// and TF's macro layer evaluates once, not per-package.
    pub(crate) bzl_cache: RefCell<std::collections::HashMap<String, starlark::environment::FrozenModule>>,
    /// Harvested UNDRIVEN Starlark declarations (one frozen dict per dependency-loaded
    /// package) — analyzed on demand cross-package ([`crate::dialect`] `analyze_deferred`).
    pub(crate) deferred_decls: RefCell<Vec<starlark::values::OwnedFrozenValue>>,
    /// Layer 0: harvested provider instances from COMPLETED packages (one frozen dict per
    /// package: canonical label → [(constructor, instance)]). OwnedFrozenValues keep their
    /// heaps alive; `dep[P]` falls back here for cross-package instances.
    pub(crate) cross_captured: RefCell<Vec<starlark::values::OwnedFrozenValue>>,
    /// E0d: the Session's live fact store — the DDS IS the store. `None` until first use (lazy
    /// schema registration); access via [`crate::dds::session_dds`]. Targets assert incrementally
    /// at `record_target`; folds read this directly (no per-dep rebuild — O(n), not O(n²)).
    pub(crate) dds: RefCell<Option<razel_dds::Dds>>,
}

/// A deferred native-rule analysis body (E0c): the rule fn's work, run by the demand-driven pass.
/// MUST capture only plain data (no starlark `Value`s — they would be invisible to the GC).
pub(crate) type NativeAnalyzeFn =
    Box<dyn for<'v, 'a, 'e> FnOnce(&mut Evaluator<'v, 'a, 'e>) -> anyhow::Result<()>>;

/// Coerce a closure to [`NativeAnalyzeFn`] (pins the higher-ranked lifetimes for inference).
pub(crate) fn native_decl<F>(f: F) -> NativeAnalyzeFn
where
    F: for<'v, 'a, 'e> FnOnce(&mut Evaluator<'v, 'a, 'e>) -> anyhow::Result<()> + 'static,
{
    Box::new(f)
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
        Ok(self.define_values.iter().all(|(k, v)| has_define(k, v)))
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
        return expand(s.to_string());
    }
    match &*sess.current_pkg.borrow() {
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
    match &*sess.current_pkg.borrow() {
        Some(pkg) => format!("{pkg}/{path}"),
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
