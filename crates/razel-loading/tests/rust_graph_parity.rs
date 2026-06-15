//! RazelRustParityPlan **A1** — the rust analysis-parity gate (the DRIVER). Mirrors `graph_parity.rs`
//! (cc) / `java_graph_parity.rs`: render razel's declared action GRAPH for a local rust corpus case
//! and diff it (as a set) against the captured Bazel golden.
//!
//! **Lands RED on purpose** (the gate LEADS): razel's rustc argv is a lean original (`19321b7`),
//! structurally far from rules_rust's, and *this diff is the work-list* Phase A (A2–A6) shrinks to
//! green. A documented carve-out alongside the 2 cc/java sentinels until A6 — so the expected-red set
//! is 3 until the rust argv is faithful. Root cause this gate fixes: rust parity was never gated
//! (cc/java had a parity test; rust had the golden since `496374a` but no test consuming it).

use razel_loading::{analyze_workspace_with, GlobalFlags};
use std::path::Path;

/// Allowlisted + logged deviations (`Report::omitted`), never silently dropped (plan §2):
/// - the Bazel infra actions razel does not model;
/// - **`CargoBuildScriptRun`** — DOCUMENTED DEVIATION (RR's call, plan §6.1): razel's build-script
///   run emits ONE structured JSONL flags-file (`.out`) + `OUT_DIR`; Bazel splits it into
///   `.flags`/`.linkflags`/`.linksearchpaths`/`.env`/`.depenv` (+ runfiles). That format is
///   intra-target internal plumbing — consumed only by the same crate's rustc — so razel keeps its
///   §6.1 design; the parity-meaningful surface (the crate's rustc argv + the rlib) is matched.
/// What remains to MATCH: the 2 Rustc actions (the build-script bin compile + the crate compile).
const OMIT: &[&str] = &[
    "CargoBuildScriptRun",
    "ExecutableSymlink",
    "RepoMappingManifest",
    "RunfilesTree",
    "SourceSymlinkManifest",
    "Symlink",
    "SymlinkTree",
];

/// Canonicalize a Rustc argv for comparison: strip the process-wrapper prefix → bare rustc args
/// (A1), then drop the toolchain/link deviation flags (documented; razel uses the system rustc + no
/// cc-toolchain rust links). Applied to BOTH razel + golden.
fn rustc_argv(argv: &[String]) -> Vec<String> {
    razel_parity::strip_rust_deviation_flags(&razel_parity::canonicalize_rust_argv(argv))
}

/// The build-script run's flags-file outputs, consumed by the crate's rustc as inputs. razel emits
/// ONE JSONL (`.out`); Bazel splits them (`.flags`/`.linkflags`/`.linksearchpaths`/`.env`/`.depenv`/
/// `.cargo_runfiles`) — the documented (b) deviation (the CargoBuildScriptRun output format). The
/// crate's CONSUMPTION inherits it, so drop these from the input comparison (the `OUT_DIR` tree
/// (`.out_dir`) is NOT dropped — it matches). Applied to BOTH sides' Rustc inputs.
fn rustc_inputs(inputs: &[String]) -> Vec<String> {
    const BS_FLAG_SUFFIXES: &[&str] = &[
        ".out", ".flags", ".linkflags", ".linksearchpaths", ".env", ".depenv", ".cargo_runfiles",
    ];
    let mut v: Vec<String> = inputs
        .iter()
        .filter(|i| !BS_FLAG_SUFFIXES.iter().any(|s| i.ends_with(s)))
        .cloned()
        .collect();
    v.sort();
    v
}

#[test]
fn rust_build_script_graph_matches_the_golden() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../parity");
    // Parity posture (A2): drive razel as Bazel does — outputs in the `bazel-out/<cfg>/bin/` tree
    // (so the action keys can pair with the golden, which is `bazel aquery`'s bazel-out paths).
    let flags = GlobalFlags { bazel_build_compat: true, ..Default::default() };
    // Analyze the crate; its build-script dep (`:build_script_build`) is pulled in via the §4.3 edge,
    // so the action set is `withbs`'s rustc (wrapper-routed) + the build script's compile + run.
    let targets = analyze_workspace_with(&root, "//corpus/rust/build_script:withbs", flags)
        .expect("razel analyzes the build_script corpus case");

    // Render → normalize (cfg/repo/hash/sdk tokens) → canonicalize the Rustc argv (strip the
    // process-wrapper prefix so the comparison is the rustc invocation itself, A1).
    let n = |s: &str| razel_parity::normalize(s).trim_end().to_string();
    let razel: Vec<razel_parity::Action> = targets
        .iter()
        .flat_map(|t| t.actions.iter())
        .map(|a| {
            let rustc = a.mnemonic == "Rustc";
            let raw_inputs: Vec<String> = a.inputs.iter().map(|s| n(s)).collect();
            let mut inputs = if rustc { rustc_inputs(&raw_inputs) } else { raw_inputs };
            inputs.sort();
            let mut outputs: Vec<String> = a.outputs.iter().map(|s| n(s)).collect();
            outputs.sort();
            let argv: Vec<String> = a.argv.iter().map(|s| n(s)).collect();
            let argv = if rustc { rustc_argv(&argv) } else { argv };
            razel_parity::Action { mnemonic: a.mnemonic.clone(), argv, inputs, outputs }
        })
        .collect();

    // The golden, with the SAME Rustc canonicalization on its side.
    let golden: Vec<razel_parity::Action> = razel_parity::parse_golden(include_str!(
        "../../../parity/corpus/rust/build_script/golden.txt"
    ))
    .into_iter()
    .map(|a| {
        let rustc = a.mnemonic == "Rustc";
        razel_parity::Action {
            argv: if rustc { rustc_argv(&a.argv) } else { a.argv.clone() },
            inputs: if rustc { rustc_inputs(&a.inputs) } else { a.inputs.clone() },
            mnemonic: a.mnemonic,
            outputs: a.outputs,
        }
    })
    .collect();

    let report = razel_parity::diff(&razel, &golden, OMIT);
    assert!(
        report.is_match(),
        "RazelRustParityPlan: razel's rust graph must match the Bazel golden (documented deviations \
         only). RED until A6 — the diff below is the work-list:\n{report:#?}"
    );
}
