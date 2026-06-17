//! P5.4 (§9.5) dev driver: DOGFOOD — `razel build //crates/razel-cli:razel` with NO Bazel.
//!
//! IGNORED by default (the self-hosting capstone). Analyzes + builds razel's OWN full graph (~20
//! workspace crates + ~229 `@crates`) from the repo root, materializing `.razel-crates`. The
//! milestone-5 WS gate — run explicitly:
//!   cargo test -p razel-build --test dogfood_selfhost -- --ignored --nocapture

use razel_build::build_workspace_with;
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

/// Build PRODUCTS razel writes into a materialized `.razel-crates` repo (rlibs/rmetas/dylibs/`.d`,
/// `_bs*` build-script outputs, the `cargo_toml_env_vars` env-file) — one level, directly under each
/// repo dir. Listing them lets the build assert it left the SOURCE repos pristine (B2: outputs go to
/// the `razel-out` tree, never in-place) and lets the re-run cleaner drop stale ones.
fn crate_build_products(root: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let Ok(repos) = std::fs::read_dir(root.join(".razel-crates")) else { return found };
    for repo in repos.flatten().filter(|e| e.path().is_dir()) {
        let Ok(files) = std::fs::read_dir(repo.path()) else { continue };
        for f in files.flatten() {
            let n = f.file_name().to_string_lossy().into_owned();
            let product = n.ends_with(".rlib")
                || n.ends_with(".rmeta")
                || n.ends_with(".dylib")
                || n.ends_with(".d")
                || n.starts_with("_bs")
                || n == "cargo_toml_env_vars";
            if product {
                found.push(f.path());
            }
        }
    }
    found
}

/// Re-run hygiene: drop stale build PRODUCTS from prior (pre-B2 / in-place) builds, keeping the
/// materialized SOURCES — so a fresh build's `.razel-crates`-cleanliness assertion is meaningful.
fn clean_crate_build_products(root: &Path) {
    for p in crate_build_products(root) {
        let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
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

/// Stage 2 — the CAPSTONE: BUILD the razel binary with no Bazel into the OUTPUT TREE, then RUN it
/// (§9.5 WS gate: "the binary builds and runs"). Under `bin_tree_layout` (what the CLI sets) the
/// binary lands in `razel-out/<config>/bin/crates/razel-cli/razel` (physically under the `.razel-exec`
/// forest), and the SOURCE tree — both the workspace and `.razel-crates` — stays pristine (P6.B2: the
/// output-tree-separation proof on the full razel graph). We exec `razel help` from the output tree.
#[test]
#[ignore = "P5.4 dogfood: builds + runs //crates/razel-cli:razel (the self-hosting capstone)"]
fn probe_razel_cli_builds_and_runs() {
    let root = repo_root();
    // SAFETY: single-threaded test setup; points wrapper-routed actions at the just-built binary.
    unsafe { std::env::set_var("RAZEL_PROCESS_WRAPPER", wrapper_path()) };
    clean_crate_build_products(&root); // drop stale products from prior (pre-B2) in-place builds
    let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();
    // P6.B2: build into the OUTPUT TREE (`bin_tree_layout`, what the CLI sets) — outputs land in
    // razel-out, never in-place; the source tree stays clean (the Q1 pollution is fixed at scale).
    let flags = GlobalFlags { bin_tree_layout: true, ..Default::default() };
    let report = build_workspace_with(&root, TOP, &cache, flags)
        .unwrap_or_else(|e| panic!("razel-cli build failed: {e}"));
    eprintln!("razel built: {} actions executed, {} outputs", report.executed, report.produced.len());

    // The binary is in the OUTPUT TREE, never in-place.
    let bin_rel = report
        .default_outputs
        .iter()
        .find(|p| p.ends_with("crates/razel-cli/razel"))
        .unwrap_or_else(|| panic!("razel binary in default_outputs, got {:?}", report.default_outputs));
    assert!(bin_rel.contains("razel-out/"), "binary in the output tree, got `{bin_rel}`");
    assert!(
        !root.join("crates/razel-cli/razel").exists(),
        "source tree must stay clean — no in-place binary"
    );
    // The build wrote NO products into the `.razel-crates` SOURCE repos (the Q1 pollution, fixed).
    let products = crate_build_products(&root);
    assert!(
        products.is_empty(),
        "build must not pollute .razel-crates, found {} product(s): {:?}",
        products.len(),
        products
    );

    // RUN the self-hosted binary (built by razel, not cargo) from the output tree.
    let bin = root.join(".razel-exec").join(bin_rel);
    let size = std::fs::metadata(&bin).map(|m| m.len()).unwrap_or(0);
    let out = std::process::Command::new(&bin).arg("help").output().expect("exec self-hosted razel");
    assert!(out.status.success(), "self-hosted razel `help` failed: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("razel") && text.contains("build"),
        "self-hosted razel help output looks wrong:\n{text}"
    );
    eprintln!("SELF-HOST OK (output tree): built ({size} bytes) + ran razel; source tree clean");
}
