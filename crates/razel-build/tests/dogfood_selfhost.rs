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
    // ALWAYS (re)build so the wrapper reflects current source (`cargo test -p razel-build` does not
    // rebuild a sibling BINARY crate); cargo no-ops when unchanged.
    let ok = std::process::Command::new(env!("CARGO"))
        .args(["build", "-p", "razel-process-wrapper"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "failed to `cargo build -p razel-process-wrapper`");
    dir.join("razel-process-wrapper")
}

/// Re-run hygiene: razel writes external-crate build PRODUCTS into the materialized `.razel-crates`
/// tree (rlibs/rmetas/dylibs, `_bs.out`/`_bs.out_dir`). A prior partial run's leftovers collide with
/// a fresh build's exec-root staging (`File exists`), so clear the PRODUCTS (keep the materialized
/// SOURCES) — the dogfood is then deterministically re-runnable. (Action idempotency-on-rerun, so no
/// clean is needed at all, is a separate rung.) One level: products sit directly under each repo dir.
fn clean_crate_build_products(root: &Path) {
    let Ok(repos) = std::fs::read_dir(root.join(".razel-crates")) else { return };
    for repo in repos.flatten().filter(|e| e.path().is_dir()) {
        let Ok(files) = std::fs::read_dir(repo.path()) else { continue };
        for f in files.flatten() {
            let n = f.file_name().to_string_lossy().into_owned();
            let product = n.ends_with(".rlib")
                || n.ends_with(".rmeta")
                || n.ends_with(".dylib")
                || n.ends_with(".d")
                || n.starts_with("_bs.out");
            if product {
                let p = f.path();
                let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
            }
        }
    }
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

/// Stage 2 — the CAPSTONE: BUILD the razel binary with no Bazel, then RUN it (§9.5 WS gate:
/// "the binary builds and runs"). The link lands at `<root>/crates/razel-cli/razel` (through the
/// exec-root symlink); we exec `razel help` and assert it works, then remove the artifact.
#[test]
#[ignore = "P5.4 dogfood: builds + runs //crates/razel-cli:razel (the self-hosting capstone)"]
fn probe_razel_cli_builds_and_runs() {
    let root = repo_root();
    // SAFETY: single-threaded test setup; points wrapper-routed actions at the just-built binary.
    unsafe { std::env::set_var("RAZEL_PROCESS_WRAPPER", wrapper_path()) };
    clean_crate_build_products(&root); // re-run hygiene (below)
    let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();
    let report = build_workspace(&root, TOP, &cache).unwrap_or_else(|e| panic!("razel-cli build failed: {e}"));
    eprintln!("razel built: {} actions executed, {} outputs", report.executed, report.produced.len());
    assert!(
        report.default_outputs.iter().any(|p| p.contains("razel")),
        "expected the razel binary output, got {:?}",
        report.default_outputs
    );

    // RUN the self-hosted binary (built by razel, not cargo): it must execute + print its help.
    let bin = root.join("crates/razel-cli/razel");
    let size = std::fs::metadata(&bin).map(|m| m.len()).unwrap_or(0);
    let out = std::process::Command::new(&bin).arg("help").output().expect("exec self-hosted razel");
    let _ = std::fs::remove_file(&bin); // clean the artifact from the source tree
    assert!(out.status.success(), "self-hosted razel `help` failed: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("razel") && text.contains("build"),
        "self-hosted razel help output looks wrong:\n{text}"
    );
    eprintln!("SELF-HOST OK: built ({size} bytes) + ran razel");
}
