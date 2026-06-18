//! `state::flags` — split from `state.rs` (facade in `mod.rs`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use super::*;

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
    /// Fetch R4 (RazelFetchPlan §3): the FETCHED external root
    /// (`_razel_<user>/<md5(ws)>/external` — `xtask fetch`'s materializations), consulted
    /// AFTER `external_base`: hand-vendored overlays win until deliberately retired.
    /// Exact repo names (the materializer writes canonical dirs; no `_`→`-` tolerance).
    pub fetched_external_base: Option<PathBuf>,
    /// `-c`/`--compilation_mode` as STRUCTURED configuration (`config_setting` matching reads
    /// this; the cc flag expansion into `copts` is separate, done by the CLI). Empty ⇒ Bazel's
    /// default `fastbuild`.
    pub compilation_mode: String,
    /// `--define k=v` pairs as structured configuration (`config_setting` `define_values` /
    /// `values = {"define": "k=v"}`).
    pub defines: Vec<(String, String)>,
    /// S2 test seam (see [`SchedHook`]). `None` in production.
    pub sched_hook: Option<SchedHook>,
    /// `--strict_bazel` (V3sh1 §3d, the parity oracle switch): act as if this IS bazel —
    /// `*.razel` files are INVISIBLE. With `BUILD.razel` removed (2026-06-18) the only
    /// `.razel` left is `MODULE.razel`, and its sole reader (`find_workspace_root`) is off
    /// the live path — so this flag is a DORMANT hook today. Full rc/CLI wiring was for S6.
    pub strict_bazel: bool,
    /// `--jobs`/`-j` (S5x): max targets the executor runs CONCURRENTLY. `0`/`1` = serial
    /// (the default — no behaviour change); `N>1` enables the parallel action executor
    /// ([`crate`]-external: `razel-build`'s `execute_jobs`). Build-phase only; analysis ignores it.
    pub jobs: usize,
    /// `RAZEL_BAZEL_BUILD_COMPAT` / `--bazel_build_compat`: write build outputs to Bazel's
    /// `bazel-out/<config>/bin/` tree (+ `bazel-testlogs/`, convenience symlinks) instead of
    /// in-tree, so razel and Bazel share the same build directory. Default off (in-tree).
    /// Build-phase only.
    pub bazel_build_compat: bool,
    /// Materialize outputs under razel's own `razel-out/<config>/bin` tree (+ `razel-bin` /
    /// `razel-testlogs` convenience symlinks) rather than in-tree. The CLI sets this for every
    /// `build`/`test`/`run` so a user's source tree is never polluted; the bare library
    /// default is off (in-tree), which keeps analysis snapshots output-base-independent.
    /// Superseded by `bazel_build_compat` (which forces the real Bazel `bazel-out` names).
    /// Build-phase only.
    pub bin_tree_layout: bool,
    /// The parsed `MODULE.bazel.lock` `@crates` world (crate-universe P3.1e), when the build uses
    /// `@crates`. Its presence turns ON canonicalization of `@crates`/`@crates__*` labels to their
    /// `@@rules_rust++crate+…` identity in [`canon_label`] (§11.3); `None` for every non-`@crates`
    /// build (so the central label path is byte-identical there). `Arc` — shared, never mutated.
    pub crate_lock: Option<std::sync::Arc<crate::lock::CrateLock>>,
}


impl GlobalFlags {
    /// The effective compilation mode (`fastbuild` when unset — Bazel's default).
    pub(crate) fn mode(&self) -> &str {
        if self.compilation_mode.is_empty() {
            "fastbuild"
        } else {
            &self.compilation_mode
        }
    }

    /// Candidate DIRS for external repo `repo` (bare name — no `@`, no `//`), in
    /// precedence order: hand-vendored (with the `_`→`-` name tolerance) first, then the
    /// fetched root (exact name) — fetch R4. The ONE resolver every external consumer
    /// (loads, BUILDs, file fallbacks, glob) folds over.
    pub(crate) fn external_repo_dirs(&self, repo: &str) -> Vec<std::path::PathBuf> {
        let mut v = Vec::new();
        if let Some(base) = &self.external_base {
            v.push(base.join(repo));
            let dashed = repo.replace('_', "-");
            if dashed != repo {
                v.push(base.join(dashed));
            }
        }
        if let Some(f) = &self.fetched_external_base {
            v.push(f.join(repo));
        }
        v
    }

    /// First existing candidate dir for `repo` (see [`Self::external_repo_dirs`]).
    pub(crate) fn external_repo_dir(&self, repo: &str) -> Option<std::path::PathBuf> {
        self.external_repo_dirs(repo)
            .into_iter()
            .find(|p| p.exists())
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


/// The host's Rust target triple (`@rules_rust//rust/platform:<triple>` / rules_rust's
/// `target_triple`). CPU-host posture: the configured triple IS the host's.
pub(crate) fn host_triple() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", _) => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        _ => "x86_64-unknown-linux-gnu",
    }
}


/// Does a `constraint_value` (package family, value name) describe the REAL host? `@platforms`'
/// os/cpu families match the host; foreign constraint families are conservative-false.
pub(crate) fn host_constraint_matches(pkg: &str, name: &str) -> bool {
    let fam = pkg
        .rsplit('/')
        .next()
        .unwrap_or(pkg)
        .trim_start_matches("@platforms//");
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
    pub(crate) fn matches(&self, sess: &Session) -> Result<bool, String> {
        let flags = &sess.global;
        let has_define = |k: &str, v: &str| flags.defines.iter().any(|(dk, dv)| dk == k && dv == v);
        for (key, want) in &self.values {
            let ok = match key.as_str() {
                "compilation_mode" => flags.mode() == want,
                "define" => match want.split_once('=') {
                    Some((k, v)) => has_define(k, v),
                    None => {
                        return Err(format!("config_setting `define` value `{want}` is not k=v"));
                    }
                },
                "cpu" => want == host_cpu(),
                // Unmodeled host-config keys (crosstool_top, apple cpus, …): CONSERVATIVE — the
                // condition doesn't match on this host. Loud once per key (registered debt).
                other => {
                    let warn_key = format!("config_setting key {other}");
                    if sess.warned.borrow_mut().insert(warn_key) {
                        eprintln!(
                            "razel: warning: config_setting key `{other}` not modeled — treating \
                             condition as non-matching"
                        );
                    }
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
            .chain(
                self.define_values
                    .iter()
                    .map(|(k, v)| ("d".to_string(), k.clone(), v.clone())),
            )
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


