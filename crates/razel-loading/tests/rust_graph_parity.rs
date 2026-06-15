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

/// Bazel infra actions razel does not model — allowlisted + logged (`Report::omitted`), never
/// silently dropped (plan §2). What remains to match: the 2 Rustc + the 1 CargoBuildScriptRun.
const OMIT: &[&str] = &[
    "ExecutableSymlink",
    "RepoMappingManifest",
    "RunfilesTree",
    "SourceSymlinkManifest",
    "Symlink",
    "SymlinkTree",
];

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
            let mut inputs: Vec<String> = a.inputs.iter().map(|s| n(s)).collect();
            inputs.sort();
            let mut outputs: Vec<String> = a.outputs.iter().map(|s| n(s)).collect();
            outputs.sort();
            let argv: Vec<String> = a.argv.iter().map(|s| n(s)).collect();
            let argv = if a.mnemonic == "Rustc" {
                razel_parity::canonicalize_rust_argv(&argv)
            } else {
                argv
            };
            razel_parity::Action { mnemonic: a.mnemonic.clone(), argv, inputs, outputs }
        })
        .collect();

    // The golden, with the SAME Rustc canonicalization on its side.
    let golden: Vec<razel_parity::Action> = razel_parity::parse_golden(include_str!(
        "../../../parity/corpus/rust/build_script/golden.txt"
    ))
    .into_iter()
    .map(|a| razel_parity::Action {
        argv: if a.mnemonic == "Rustc" {
            razel_parity::canonicalize_rust_argv(&a.argv)
        } else {
            a.argv
        },
        ..a
    })
    .collect();

    let report = razel_parity::diff(&razel, &golden, OMIT);
    assert!(
        report.is_match(),
        "RazelRustParityPlan: razel's rust graph must match the Bazel golden (documented deviations \
         only). RED until A6 — the diff below is the work-list:\n{report:#?}"
    );
}
