//! RazelRustParityPlan **B4** — the EXTERNAL-closure EXECUTION gate (xp), Milestone-1's close.
//! `rust_graph_parity` + `blake3_analysis_matches_the_bazel_golden` prove razel DECLARES Bazel's
//! `@crates//:blake3` graph; this proves razel can RUN it: a full `razel build @crates//:blake3` in
//! the parity posture (`bazel_build_compat`, the (c) exec-root) with the system toolchain routed
//! through this wrapper, asserting the produced execution surface.
//!
//! IGNORED (network): it fetches razel's OWN dogfood `@crates` lock from the repo root and
//! materializes `.razel-crates`/`.razel-exec` there (gitignored). Run:
//!   cargo test -p razel-process-wrapper --test blake3_xp -- --ignored --nocapture
//!
//! The parity-meaningful surface (plan §6.1/§8):
//!   1. blake3's **rlib is produced** and is a real `ar` archive;
//!   2. its `build.rs` cc-compiled the **NEON SIMD** into the `OUT_DIR` tree (`libblake3_neon.a`) —
//!      the cc path razel drives via the wrapper (system `cc`; exact CFLAGS not gated);
//!   3. the build script's directives reach the crate's rustc as the SAME args Bazel's split
//!      `.flags`/`.linkflags`/`.linksearchpaths` carry — incl. the link-search rewritten to the
//!      EXEC-ROOT-RELATIVE `OUT_DIR` (not the ephemeral sandbox abs path), where the tree is staged.

use std::path::{Path, PathBuf};

/// The wrapper built alongside this test — its absolute path. Wrapper-routed actions reference it via
/// `RAZEL_PROCESS_WRAPPER` (a full path resolves under the sandbox's minimal PATH).
const WRAPPER: &str = env!("CARGO_BIN_EXE_razel-process-wrapper");

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root")
}

/// blake3's generated-output dir in the exec-root: `bazel-out/<cfg>/bin/external/<repo>/`. The
/// single `<cfg>` segment is whatever this host minted — glob it rather than recompute.
fn blake3_bin_dir(exec_root: &Path) -> PathBuf {
    let cfg = std::fs::read_dir(exec_root.join("bazel-out"))
        .expect("bazel-out exists")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .expect("a <config> dir under bazel-out");
    cfg.join("bin/external/rules_rust++crate+crates__blake3-1.8.2")
}

#[test]
#[ignore = "B4 execution gate: builds @crates//:blake3 from the repo root (fetches over network)"]
fn blake3_external_closure_builds_and_its_buildscript_reaches_rustc() {
    // SAFETY: a fresh single-threaded test process; set before the in-process build reads the var.
    // Points every wrapper-routed action's argv[0] at the just-built binary (absolute → resolvable).
    unsafe {
        std::env::set_var("RAZEL_PROCESS_WRAPPER", WRAPPER);
    }

    let root = repo_root();
    // A FRESH cache → a cold build (every closure action executes); the exec-root forest is rebuilt
    // each run (`prepare_exec_root`), so this is hermetic w.r.t. prior probes.
    let cache_dir = tempfile::tempdir().expect("tempdir");
    let cache = razel_exec::Cache::new(cache_dir.path()).expect("cache");
    // Parity posture: bazel-out rooting (the (c) exec-root) — outputs land in `.razel-exec/bazel-out`,
    // isolated from the `external/` source symlink.
    let flags = razel_build::GlobalFlags { bazel_build_compat: true, ..Default::default() };
    let report = razel_build::build_workspace_with(&root, "@crates//:blake3", &cache, flags)
        .expect("razel builds @crates//:blake3 end-to-end");

    assert!(report.executed > 0, "a cold build executes actions (closure was not fully cached)");
    let exec_root = root.join(".razel-exec");
    let bin_dir = blake3_bin_dir(&exec_root);

    // (1) blake3's rlib is produced (the build's DefaultInfo) and is a real `ar` archive.
    let rlib_rel = report
        .default_outputs
        .iter()
        .find(|p| p.ends_with(".rlib"))
        .expect("blake3's rlib is the build's default output");
    let rlib = exec_root.join(rlib_rel);
    let bytes = std::fs::read(&rlib).unwrap_or_else(|e| panic!("read {}: {e}", rlib.display()));
    assert!(bytes.starts_with(b"!<arch>\n"), "rlib is an ar archive: {}", rlib.display());

    // (2) the build script cc-compiled the NEON SIMD into the OUT_DIR tree (bundled into the rlib).
    let out_dir = bin_dir.join("_bs.out_dir");
    assert!(
        out_dir.join("libblake3_neon.a").is_file(),
        "the cc SIMD static lib was produced in OUT_DIR: {}",
        out_dir.display()
    );
    let has_obj = std::fs::read_dir(&out_dir)
        .expect("OUT_DIR tree")
        .filter_map(Result::ok)
        .any(|e| e.file_name().to_string_lossy().ends_with("blake3_neon.o"));
    assert!(has_obj, "a compiled blake3_neon object is in OUT_DIR: {}", out_dir.display());

    // (3) the build script's §6.1 directives map (via the wrapper's `apply_flags`) to the rustc args
    // Bazel's split `.flags`/`.linkflags`/`.linksearchpaths` carry for the crate compile.
    let jsonl = std::fs::read_to_string(bin_dir.join("_bs.out")).expect("the §6.1 flags-file");
    let records = razel_process_wrapper::flags::read_flags_jsonl(&jsonl).expect("valid JSONL");
    let (args, _env) = razel_process_wrapper::rustc::apply_flags(&records);

    let has = |pair: [&str; 2]| args.windows(2).any(|w| w == pair);
    assert!(has(["--cfg", "blake3_neon"]), "SIMD cfg reaches rustc: {args:?}");
    assert!(has(["-l", "static=blake3_neon"]), "the static SIMD lib is linked: {args:?}");
    // The link-search is the EXEC-ROOT-RELATIVE OUT_DIR (the abs→relative rewrite, B4): a relative
    // `bazel-out/…/_bs.out_dir` where the staged tree resolves — NOT the dead per-action sandbox path.
    let lsearch = args
        .windows(2)
        .find(|w| w[0] == "-L" && w[1].starts_with("native="))
        .map(|w| w[1].clone())
        .expect("a -L native= search path");
    let path = lsearch.trim_start_matches("native=");
    assert!(path.starts_with("bazel-out/"), "link-search is exec-relative: {path}");
    assert!(path.ends_with("_bs.out_dir"), "link-search points at the OUT_DIR tree: {path}");
    assert!(!path.contains(".razel-sandbox"), "link-search is NOT the ephemeral sandbox abs path: {path}");
}
