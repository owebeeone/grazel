//! P5.4 (§9.5) dev driver: DOGFOOD — `razel build //crates/razel-cli:razel` with NO Bazel.
//!
//! IGNORED by default (the self-hosting capstone). Analyzes + builds razel's OWN full graph (~20
//! workspace crates + ~229 `@crates`) from the repo root, materializing `.razel-crates`. The
//! milestone-5 WS gate — run explicitly:
//!   cargo test -p razel-build --test dogfood_selfhost -- --ignored --nocapture

use razel_build::build_workspace;
use razel_exec::Cache;
use razel_loading::{GlobalFlags, analyze_workspace_with};
use std::path::Path;

const TOP: &str = "//crates/razel-cli:razel";

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root")
}

/// Stage 1 — does the FULL razel graph ANALYZE? (loading + analysis, no execution.) Reports the
/// target count; surfaces graph-construction gaps before the execution tail.
#[test]
#[ignore = "P5.4 dogfood: analyzes razel's own //crates/razel-cli:razel graph from the root"]
fn probe_razel_cli_analyzes() {
    let root = repo_root();
    match analyze_workspace_with(&root, TOP, GlobalFlags::default()) {
        Ok(targets) => {
            eprintln!("razel-cli graph analyzed: {} targets", targets.len());
            assert!(
                targets.len() > 50,
                "expected the full razel+@crates graph, got {}",
                targets.len()
            );
        }
        Err(e) => panic!("razel-cli analysis failed: {e}"),
    }
}

/// Stage 2 — the capstone: BUILD the razel binary with no Bazel. Asserts the binary is produced.
#[test]
#[ignore = "P5.4 dogfood: builds //crates/razel-cli:razel (the self-hosting capstone)"]
fn probe_razel_cli_builds() {
    let root = repo_root();
    let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();
    match build_workspace(&root, TOP, &cache) {
        Ok(report) => {
            eprintln!(
                "razel built: {} actions executed, {} outputs",
                report.executed,
                report.produced.len()
            );
            assert!(
                report.default_outputs.iter().any(|p| p.contains("razel")),
                "expected the razel binary output, got {:?}",
                report.default_outputs
            );
        }
        Err(e) => panic!("razel-cli build failed: {e}"),
    }
}
