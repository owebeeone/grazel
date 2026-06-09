//! A0 graph-parity acceptance (RazelStarlarkBoundaryPlan §10): razel's declared cc graph vs the
//! captured golden, compared as a *set* via `razel_parity::diff`. As of A3/4·ii the parity runner
//! analyzes in **Adopt-Bazel** toolchain mode (§7) — razel's bundled `cc:defs.bzl` over the
//! `Constrain` engine + the path model — so it is **GREEN**: razel reproduces Bazel's `CppCompile` +
//! `CppArchive` graph for `cc/transitive` (the 2 `CppModuleMap` actions razel does not model are
//! allowlisted + logged). Native mode (what razel-build runs) is unaffected.

use razel_loading::{CcToolchainMode, GlobalFlags, analyze_workspace_with};
use std::path::Path;

#[test]
fn live_cc_graph_matches_the_golden_in_adopt_bazel_mode() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../parity");
    let flags = GlobalFlags { cc_toolchain: CcToolchainMode::AdoptBazel, ..Default::default() };
    let targets = analyze_workspace_with(&root, "//corpus/cc/transitive:util", flags).unwrap();

    // Render each action into comparable form: normalize tokens (real cfg → <cfg>, repo → <repo>);
    // sort inputs/outputs.
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
    assert!(report.is_match(), "live cc graph must match the golden:\n{report:#?}");
    assert_eq!(report.omitted.len(), 2, "the 2 CppModuleMap actions are allowlisted + logged");
}
