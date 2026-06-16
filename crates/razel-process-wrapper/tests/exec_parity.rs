//! RazelRustParityPlan **A7** — local EXECUTION parity (xp). The analysis gate (`rust_graph_parity`,
//! A1–A6) proved razel DECLARES Bazel's action graph; this proves razel can RUN it: drive a full
//! `razel build` of the local build-script corpus case with the REAL toolchain (system rustc routed
//! through this wrapper) and diff the produced execution surface against Bazel's.
//!
//! The parity-meaningful surface (plan §6.1/§8): (1) the crate's **rlib is produced**, and (2) the
//! build script's `cargo::rustc-cfg=buildscript_ran` **reaches the crate's rustc** as `--cfg
//! buildscript_ran` — the same cfg Bazel's `build_script_build.flags` carries (`--cfg=buildscript_ran`,
//! captured in `golden.flags`). razel emits ONE §6.1 JSONL flags-file (`.out`); Bazel SPLITS it into
//! `.flags`/`.env`/`.linkflags`/… — the documented **(b)** format deviation — so the diff is at the
//! DIRECTIVE level: the wrapper's `apply_flags` maps razel's JSONL to the same rustc args Bazel's
//! split files produce (here just the one cfg; the build script emits no env/link directives, so
//! Bazel's sibling files are empty and razel's record set has no env/link kinds).

use std::path::{Path, PathBuf};

/// The wrapper binary built alongside this test — its absolute path. The run / wrapped-rustc actions
/// reference the wrapper by `RAZEL_PROCESS_WRAPPER` (a full path → resolves under the sandbox's
/// minimal PATH); without the override they would use the bare crate name, unresolvable at exec.
const WRAPPER: &str = env!("CARGO_BIN_EXE_razel-process-wrapper");

fn parity_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../parity")
}

/// The build-script case is self-contained (`build_script_build` + `withbs`, both local) → a temp
/// workspace of just `MODULE.bazel` + the case dir builds it WITHOUT polluting the source tree.
fn temp_workspace() -> tempfile::TempDir {
    let parity = parity_dir();
    let ws = tempfile::tempdir().expect("tempdir");
    let case = ws.path().join("corpus/rust/build_script");
    std::fs::create_dir_all(&case).unwrap();
    std::fs::copy(parity.join("MODULE.bazel"), ws.path().join("MODULE.bazel")).unwrap();
    for f in ["BUILD", "build.rs", "lib.rs"] {
        std::fs::copy(parity.join("corpus/rust/build_script").join(f), case.join(f)).unwrap();
    }
    ws
}

/// Normalize a Bazel `.flags` file to rustc-arg TOKENS, matching the wrapper's two-token form: a
/// leading `--flag=value` splits into `["--flag", "value"]` (rustc accepts both; razel emits the
/// split form). For `--cfg=buildscript_ran` → `["--cfg", "buildscript_ran"]`.
fn normalize_rustc_flags(raw: &str) -> Vec<String> {
    raw.split_whitespace()
        .flat_map(|tok| match tok.split_once('=') {
            Some((flag, val)) if flag.starts_with("--") => {
                vec![flag.to_string(), val.to_string()]
            }
            _ => vec![tok.to_string()],
        })
        .collect()
}

#[test]
fn build_script_executes_and_its_cfg_reaches_the_crate_rustc() {
    // SAFETY: a fresh single-threaded test process; set before the in-process build reads the var.
    // `process_wrapper()` reads RAZEL_PROCESS_WRAPPER, so this points every wrapper-routed action's
    // argv[0] at the just-built binary (an absolute path, resolvable under the sandbox PATH).
    unsafe {
        std::env::set_var("RAZEL_PROCESS_WRAPPER", WRAPPER);
    }

    let ws = temp_workspace();
    let cache = razel_exec::Cache::new(ws.path().join(".razel-cache")).expect("cache");
    let report = razel_build::build_workspace_with(
        ws.path(),
        "//corpus/rust/build_script:withbs",
        &cache,
        razel_build::GlobalFlags::default(),
    )
    .expect("razel builds the build-script case end-to-end");

    // Cold cache → all THREE actions execute: build-script bin compile, the run, the crate compile.
    assert_eq!(report.executed, 3, "cold build runs the bin compile + run + crate compile");

    // (1) the crate's rlib is produced and is a real `ar` archive (the `!<arch>` magic).
    let case = ws.path().join("corpus/rust/build_script");
    let rlib = std::fs::read_dir(&case)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            let n = p.file_name().unwrap().to_string_lossy();
            n.starts_with("libwithbs-") && n.ends_with(".rlib")
        })
        .expect("the withbs rlib was produced");
    let bytes = std::fs::read(&rlib).unwrap();
    assert!(bytes.starts_with(b"!<arch>\n"), "rlib is an ar archive: {}", rlib.display());

    // (2) the build script's directive reached the flags-file, and the wrapper maps it to the SAME
    // rustc args Bazel's split `.flags` carries. razel: ONE §6.1 JSONL `.out`; Bazel: `.flags`
    // (the (b) deviation) — diffed at the directive level via `apply_flags` vs `golden.flags`.
    let jsonl = std::fs::read_to_string(case.join("build_script_build.out"))
        .expect("the §6.1 flags-file was produced");
    let records = razel_process_wrapper::flags::read_flags_jsonl(&jsonl).expect("valid JSONL");
    let (rustc_args, env) = razel_process_wrapper::rustc::apply_flags(&records);

    let golden = std::fs::read_to_string(parity_dir().join("corpus/rust/build_script/golden.flags"))
        .expect("bazel .flags reference (golden.flags)");
    let expected = normalize_rustc_flags(&golden);
    assert_eq!(rustc_args, expected, "razel's flags-file maps to Bazel's `.flags` rustc args");
    assert_eq!(rustc_args, ["--cfg", "buildscript_ran"], "the build-script cfg reaches the crate");
    // The build script emits no env/link directives → Bazel's `.env`/`.linkflags` are empty, so
    // razel's record set yields no rustc-env (matching the empty Bazel siblings).
    assert!(env.is_empty(), "no rustc-env directives (Bazel's `.env` is empty too): {env:?}");
}
