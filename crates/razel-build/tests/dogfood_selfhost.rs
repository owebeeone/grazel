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

/// The build routes every rustc/build-script action through `razel-process-wrapper`, referenced by
/// `RAZEL_PROCESS_WRAPPER` (an ABSOLUTE path — resolvable under the sandbox's minimal PATH; mirrors
/// `blake3_xp`). The test binary lives in `target/<profile>/deps/`, so the wrapper is its
/// grandparent's child; build it (cargo, no Bazel) if absent.
fn wrapper_path() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test exe");
    let dir = exe.parent().and_then(|p| p.parent()).expect("target/<profile>");
    let wrapper = dir.join("razel-process-wrapper");
    if !wrapper.exists() {
        let ok = std::process::Command::new(env!("CARGO"))
            .args(["build", "-p", "razel-process-wrapper"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "failed to `cargo build -p razel-process-wrapper`");
    }
    wrapper
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
    // SAFETY: single-threaded test setup; points wrapper-routed actions at the just-built binary.
    unsafe { std::env::set_var("RAZEL_PROCESS_WRAPPER", wrapper_path()) };
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
