//! B2 dev-driver (RazelRustParityPlan): does razel analyze the REAL `@crates//:blake3` closure?
//!
//! IGNORED by default — it analyzes razel's OWN dogfood `@crates` lock from the repo root (and
//! materializes `.razel-crates` there), so it is a fast DRIVER for the B2 gap-closing roll, not a
//! committed gate (B3's `crate_blake3` parity golden is the gate). Faster feedback than the CLI
//! rebuild: only razel-loading recompiles. Run:
//!   cargo test -p razel-loading --test blake3_closure -- --ignored --nocapture

use razel_loading::{GlobalFlags, analyze_workspace_with};
use std::path::Path;

/// RazelRustParityPlan **B3** — blake3 EXTERNAL-closure ANALYSIS parity. Same posture as Phase A's
/// `rust_graph_parity`, applied to the real `@crates//:blake3`: canonicalize the wrapper prefix +
/// strip the documented toolchain/link deviations, diff blake3's OWN Rustc actions against the
/// captured Bazel golden. The OMIT list extends Phase A's with **`ExtractCargoTomlEnvVars`** (+ razel's
/// `FileWrite` twin): razel's `cargo_toml_env_vars` bakes `CARGO_PKG_*` at analysis (a `FileWrite`)
/// where Bazel runs an extraction action — intra-target env plumbing consumed only by the same crate's
/// rustc, exactly the documented `CargoBuildScriptRun` (b) deviation (the meaningful surface — the
/// `CARGO_PKG_*` reaching rustc — is execution-parity, B4).
const B3_OMIT: &[&str] = &[
    "CargoBuildScriptRun",
    "ExtractCargoTomlEnvVars", // bazel's mnemonic for cargo_toml_env_vars
    "FileWrite",               // razel's cargo_toml_env_vars twin (analysis-baked CARGO_PKG_*)
    "TranslateBuildInfo",
    "ExecutableSymlink",
    "RepoMappingManifest",
    "RunfilesTree",
    "SourceSymlinkManifest",
    "Symlink",
    "SymlinkTree",
    "CopyFile",
];

fn b3_rustc_argv(argv: &[String]) -> Vec<String> {
    let mut v = razel_parity::strip_rust_deviation_flags(&razel_parity::canonicalize_rust_argv(argv));
    // `-Ldependency` search paths are ORDER-INSENSITIVE to rustc (RR-sanctioned parity relaxation):
    // compare them as a SET by sorting the `-Ldependency=` tokens in place (non-`-L` tokens keep their
    // position). Applied to BOTH sides, so razel's transitive fold matches Bazel's traversal regardless
    // of order — only the SET of dep dirs must agree.
    let mut sorted: Vec<String> =
        v.iter().filter(|t| t.starts_with("-Ldependency=")).cloned().collect();
    sorted.sort();
    let mut it = sorted.into_iter();
    for t in v.iter_mut() {
        if t.starts_with("-Ldependency=") {
            *t = it.next().expect("same count");
        }
    }
    v
}

/// Drop the build-script flag-file inputs (the (b) JSONL-vs-split deviation) — same as Phase A.
fn b3_rustc_inputs(inputs: &[String]) -> Vec<String> {
    const BS: &[&str] =
        &[".out", ".flags", ".linkflags", ".linksearchpaths", ".env", ".depenv", ".cargo_runfiles"];
    let mut v: Vec<String> =
        inputs.iter().filter(|i| !BS.iter().any(|s| i.ends_with(s))).cloned().collect();
    v.sort();
    v
}

#[test]
#[ignore = "B2 dev driver: analyzes razel's own @crates closure from the repo root"]
fn probe_blake3_closure_analyzes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root");
    match analyze_workspace_with(&root, "@crates//:blake3", GlobalFlags::default()) {
        Ok(targets) => {
            eprintln!("OK: @crates//:blake3 closure analyzed — {} targets", targets.len());
            for t in targets.iter().take(60) {
                eprintln!("  {}", t.name);
            }
        }
        Err(e) => panic!("@crates//:blake3 closure did NOT analyze:\n{e}"),
    }
}

#[test]
#[ignore = "P4.2 dev driver: analyzes serde_derive (a rust_proc_macro crate) from the repo root"]
fn probe_serde_derive_analyzes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root");
    let label = "@@rules_rust++crate+crates__serde_derive-1.0.228//:serde_derive";
    match analyze_workspace_with(&root, label, GlobalFlags { bazel_build_compat: true, ..Default::default() }) {
        Ok(targets) => {
            eprintln!("OK: serde_derive closure analyzed — {} targets", targets.len());
            for t in targets.iter().filter(|t| t.name.contains("serde_derive")) {
                for a in &t.actions {
                    eprintln!("  {} [{}] -> {:?}", t.name, a.mnemonic, a.outputs);
                }
            }
        }
        Err(e) => panic!("serde_derive closure did NOT analyze:\n{e}"),
    }
}

#[test]
#[ignore = "P4.6 dev driver: full-@crates scale — analyzes starlark's LARGE closure from the root"]
fn probe_starlark_closure_analyzes() {
    // P4.6 (§9.4): the resolved graph builds end-to-end at scale. `starlark` is razel's heaviest
    // direct dep — its closure spans dozens of crates (proc-macros, build scripts, per-cfg selects),
    // so analyzing it on the pure `fetch_crate` RepoFetch path exercises the full `@crates` machinery
    // far beyond blake3/getrandom/serde_derive. A clean analysis (no unresolved attr/select/dep) is
    // the scale signal; the captured goldens remain the aq-parity gate.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root");
    match analyze_workspace_with(&root, "@crates//:starlark", GlobalFlags::default()) {
        Ok(targets) => {
            eprintln!("OK: @crates//:starlark closure analyzed — {} targets", targets.len());
            assert!(targets.len() > 30, "starlark pulls a large closure, got {}", targets.len());
        }
        Err(e) => panic!("@crates//:starlark closure did NOT analyze:\n{e}"),
    }
}

#[test]
#[ignore = "P4.4 dev driver: analyzes getrandom (richer per-cfg `select()` deps) from the repo root"]
fn probe_getrandom_analyzes() {
    // getrandom 0.4.2's `deps = select({<triple>: [libc|wasi|windows…], default: []})` is the richer
    // P4.3/P4.4 per-cfg case: on a darwin host the libc arm must resolve (extern'd into getrandom's
    // own rustc), and the wasm/windows arms must be selected away (their repos are never fetched).
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root");
    let label = "@@rules_rust++crate+crates__getrandom-0.4.2//:getrandom";
    match analyze_workspace_with(&root, label, GlobalFlags { bazel_build_compat: true, ..Default::default() }) {
        Ok(targets) => {
            eprintln!("OK: getrandom closure analyzed — {} targets", targets.len());
            for t in targets.iter().filter(|t| t.name.contains("getrandom-0.4.2")) {
                for a in &t.actions {
                    eprintln!("  {} [{}] -> {:?}", t.name, a.mnemonic, a.outputs);
                    for (i, tok) in a.argv.iter().enumerate() {
                        if tok.contains("--extern") || tok.contains("libc") || tok.contains("wasi") {
                            eprintln!("    argv[{i}] {tok}");
                        }
                    }
                }
            }
        }
        Err(e) => panic!("getrandom closure did NOT analyze:\n{e}"),
    }
}

#[test]
#[ignore = "B3 dev gate: blake3 external-closure analysis parity (fetches over network from the root)"]
fn blake3_analysis_matches_the_bazel_golden() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root");
    // Parity posture: bazel-out rooting so action keys pair with `bazel aquery`'s paths.
    let flags = GlobalFlags { bazel_build_compat: true, ..Default::default() };
    let targets = analyze_workspace_with(&root, "@crates//:blake3", flags)
        .unwrap_or_else(|e| panic!("razel analyzes @crates//:blake3: {e}"));

    let n = |s: &str| razel_parity::normalize(s).trim_end().to_string();
    let is_blake3 = |s: &str| s.contains("crates__blake3-1.8.2");

    // razel's blake3-OWN actions, normalized + Rustc-canonicalized.
    let razel: Vec<razel_parity::Action> = targets
        .iter()
        .filter(|t| is_blake3(&t.name))
        .flat_map(|t| t.actions.iter())
        .map(|a| {
            let rustc = a.mnemonic == "Rustc";
            let mut outputs: Vec<String> = a.outputs.iter().map(|s| n(s)).collect();
            outputs.sort();
            let argv: Vec<String> = a.argv.iter().map(|s| n(s)).collect();
            let mut inputs: Vec<String> = a.inputs.iter().map(|s| n(s)).collect();
            inputs.sort();
            razel_parity::Action {
                mnemonic: a.mnemonic.clone(),
                argv: if rustc { b3_rustc_argv(&argv) } else { argv },
                inputs: if rustc { b3_rustc_inputs(&inputs) } else { inputs },
                outputs,
            }
        })
        .collect();

    // The committed Bazel golden, blake3's OWN actions (it's already blake3-scoped + normalized).
    let golden: Vec<razel_parity::Action> = razel_parity::parse_golden(include_str!(
        "../../../parity/corpus/rust/crate_blake3/golden.txt"
    ))
    .into_iter()
    .map(|a| {
        let rustc = a.mnemonic == "Rustc";
        razel_parity::Action {
            argv: if rustc { b3_rustc_argv(&a.argv) } else { a.argv.clone() },
            inputs: if rustc { b3_rustc_inputs(&a.inputs) } else { a.inputs.clone() },
            mnemonic: a.mnemonic,
            outputs: a.outputs,
        }
    })
    .collect();

    if std::env::var("B3_DUMP").is_ok() {
        for (tag, set) in [("RAZEL", &razel), ("GOLDEN", &golden)] {
            for a in set.iter().filter(|a| a.mnemonic == "Rustc") {
                eprintln!("== {tag} Rustc {} ==", a.outputs.join(","));
                for (i, t) in a.argv.iter().enumerate() {
                    eprintln!("  [{i}] {t}");
                }
            }
        }
    }
    let report = razel_parity::diff(&razel, &golden, B3_OMIT);
    assert!(
        report.is_match(),
        "blake3 analysis must match the Bazel golden (documented deviations only):\n{report:#?}"
    );
}

#[test]
#[ignore = "P4.2 dev gate: serde_derive proc-macro analysis parity (fetches over network from the root)"]
fn serde_derive_analysis_matches_the_bazel_golden() {
    // serde_derive is a `rust_proc_macro` (§5.3, P4.1/P4.2) — its faithful Rustc action (the host
    // dylib compile) must match Bazel's, modulo the SAME documented deviations as blake3's rlib.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root");
    let flags = GlobalFlags { bazel_build_compat: true, ..Default::default() };
    let label = "@@rules_rust++crate+crates__serde_derive-1.0.228//:serde_derive";
    let targets = analyze_workspace_with(&root, label, flags)
        .unwrap_or_else(|e| panic!("razel analyzes serde_derive: {e}"));

    let n = |s: &str| razel_parity::normalize(s).trim_end().to_string();
    let is_sd = |s: &str| s.contains("crates__serde_derive-1.0.228");

    let razel: Vec<razel_parity::Action> = targets
        .iter()
        .filter(|t| is_sd(&t.name))
        .flat_map(|t| t.actions.iter())
        .map(|a| {
            let rustc = a.mnemonic == "Rustc";
            let mut outputs: Vec<String> = a.outputs.iter().map(|s| n(s)).collect();
            outputs.sort();
            let argv: Vec<String> = a.argv.iter().map(|s| n(s)).collect();
            let mut inputs: Vec<String> = a.inputs.iter().map(|s| n(s)).collect();
            inputs.sort();
            razel_parity::Action {
                mnemonic: a.mnemonic.clone(),
                argv: if rustc { b3_rustc_argv(&argv) } else { argv },
                inputs: if rustc { b3_rustc_inputs(&inputs) } else { inputs },
                outputs,
            }
        })
        .collect();

    let golden: Vec<razel_parity::Action> = razel_parity::parse_golden(include_str!(
        "../../../parity/corpus/rust/crate_serde_derive/golden.txt"
    ))
    .into_iter()
    .map(|a| {
        let rustc = a.mnemonic == "Rustc";
        razel_parity::Action {
            argv: if rustc { b3_rustc_argv(&a.argv) } else { a.argv.clone() },
            inputs: if rustc { b3_rustc_inputs(&a.inputs) } else { a.inputs.clone() },
            mnemonic: a.mnemonic,
            outputs: a.outputs,
        }
    })
    .collect();

    if std::env::var("B3_DUMP").is_ok() {
        for (tag, set) in [("RAZEL", &razel), ("GOLDEN", &golden)] {
            for a in set.iter().filter(|a| a.mnemonic == "Rustc") {
                eprintln!("== {tag} Rustc {} ==", a.outputs.join(","));
                for (i, t) in a.argv.iter().enumerate() {
                    eprintln!("  [{i}] {t}");
                }
            }
        }
    }
    let report = razel_parity::diff(&razel, &golden, B3_OMIT);
    assert!(
        report.is_match(),
        "serde_derive analysis must match the Bazel golden (documented deviations only):\n{report:#?}"
    );
}

#[test]
#[ignore = "P4.4 dev gate: getrandom richer per-cfg `select()` deps analysis parity (network from the root)"]
fn getrandom_analysis_matches_the_bazel_golden() {
    // getrandom 0.4.2 is the richer §5.4 case (P4.3/P4.4): its `deps = select({<triple>: …})` must
    // resolve to the host (darwin/unix) arm — getrandom's OWN rlib Rustc action `--extern`s exactly
    // cfg_if+libc+rand_core, with the wasm/windows arms selected away (never fetched). Same documented
    // deviations as blake3's rlib (B3_OMIT + the toolchain/link strips).
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root");
    let flags = GlobalFlags { bazel_build_compat: true, ..Default::default() };
    let label = "@@rules_rust++crate+crates__getrandom-0.4.2//:getrandom";
    let targets = analyze_workspace_with(&root, label, flags)
        .unwrap_or_else(|e| panic!("razel analyzes getrandom: {e}"));

    let n = |s: &str| razel_parity::normalize(s).trim_end().to_string();
    let is_gr = |s: &str| s.contains("crates__getrandom-0.4.2");

    let razel: Vec<razel_parity::Action> = targets
        .iter()
        .filter(|t| is_gr(&t.name))
        .flat_map(|t| t.actions.iter())
        .map(|a| {
            let rustc = a.mnemonic == "Rustc";
            let mut outputs: Vec<String> = a.outputs.iter().map(|s| n(s)).collect();
            outputs.sort();
            let argv: Vec<String> = a.argv.iter().map(|s| n(s)).collect();
            let mut inputs: Vec<String> = a.inputs.iter().map(|s| n(s)).collect();
            inputs.sort();
            razel_parity::Action {
                mnemonic: a.mnemonic.clone(),
                argv: if rustc { b3_rustc_argv(&argv) } else { argv },
                inputs: if rustc { b3_rustc_inputs(&inputs) } else { inputs },
                outputs,
            }
        })
        .collect();

    let golden: Vec<razel_parity::Action> = razel_parity::parse_golden(include_str!(
        "../../../parity/corpus/rust/crate_getrandom/golden.txt"
    ))
    .into_iter()
    .map(|a| {
        let rustc = a.mnemonic == "Rustc";
        razel_parity::Action {
            argv: if rustc { b3_rustc_argv(&a.argv) } else { a.argv.clone() },
            inputs: if rustc { b3_rustc_inputs(&a.inputs) } else { a.inputs.clone() },
            mnemonic: a.mnemonic,
            outputs: a.outputs,
        }
    })
    .collect();

    if std::env::var("B3_DUMP").is_ok() {
        for (tag, set) in [("RAZEL", &razel), ("GOLDEN", &golden)] {
            for a in set.iter().filter(|a| a.mnemonic == "Rustc") {
                eprintln!("== {tag} Rustc {} ==", a.outputs.join(","));
                for (i, t) in a.argv.iter().enumerate() {
                    eprintln!("  [{i}] {t}");
                }
            }
        }
    }
    let report = razel_parity::diff(&razel, &golden, B3_OMIT);
    assert!(
        report.is_match(),
        "getrandom analysis must match the Bazel golden (documented deviations only):\n{report:#?}"
    );
}
