//! A0 graph-parity baseline (RazelStarlarkBoundaryPlan §10): razel's LIVE declared cc graph vs the
//! captured golden, compared as a *set* via `razel_parity::diff`. This is the acceptance test for
//! the cc-end-to-end phase, and it is **RED by design** today: the live `analyze_bazel` cc path
//! still emits the hardcoded `/usr/bin/c++` argv with razel paths (§6a), so the graph does not
//! match. Phase A4 (live path through the engine + the path model) flips the assertion to
//! `report.is_match()`. The `{CppModuleMap}` allowlist records the actions razel does not model —
//! logged, never silently missing (Review49's per-action-vs-per-graph fix).

use razel_loading::analyze_bazel;

#[test]
fn graph_parity_baseline_red_until_a4() {
    let corpus = r#"
load("@rules_cc//cc:defs.bzl", "cc_library")
cc_library(name = "base", srcs = ["base.cc"], hdrs = ["base.h"])
cc_library(name = "util", srcs = ["util.cc"], hdrs = ["util.h"], deps = [":base"])
"#;
    let targets = analyze_bazel(corpus).unwrap();

    // Render each live action into comparable form: normalize tokens (a no-op on razel's bare
    // paths today; maps bazel-out/<cfg> once A4 renders via the path model), sort inputs/outputs.
    let n = |s: &str| razel_parity::normalize(s).trim_end().to_string();
    let razel: Vec<razel_parity::Action> = targets
        .iter()
        .flat_map(|t| t.actions.iter())
        .map(|a| {
            let mut inputs: Vec<String> = a.inputs.iter().map(|s| n(s)).collect();
            inputs.sort();
            let mut outputs: Vec<String> = a.outputs.iter().map(|s| n(s)).collect();
            outputs.sort();
            razel_parity::Action {
                mnemonic: a.mnemonic.clone(),
                argv: a.argv.iter().map(|s| n(s)).collect(),
                inputs,
                outputs,
            }
        })
        .collect();

    let golden = razel_parity::parse_golden(include_str!(
        "../../../parity/corpus/cc/transitive/golden.txt"
    ));
    let report = razel_parity::diff(&razel, &golden, &["CppModuleMap"]);
    eprintln!("graph-parity baseline (cc/transitive):\n{report:#?}");

    // RED baseline — the gap Phase A1–A4 close (A4 turns this into `assert!(report.is_match())`).
    assert!(!report.is_match(), "expected RED baseline pre-A4");
    // The two CppModuleMap actions razel does not model are allowlisted + logged, not missing.
    assert_eq!(report.omitted.len(), 2, "the two CppModuleMap actions are recorded as omitted");
}
