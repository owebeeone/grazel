//! P6.B1 (output-tree separation): under `bin_tree_layout` (what the CLI sets), a rust build's
//! outputs land in the `razel-out/<config>/bin/…` OUTPUT TREE, never in-place in the source tree —
//! so `query` after `build` never sees products (the Q1 follow-on). The rust/cargo rules went
//! through `out_path`, which honored only `--bazel_build_compat`; B1 unifies it with `bin_prefix`
//! so `bin_tree_layout` redirects rust outputs too (cc/js/py already did via `qualify_output`).

use razel_build::{GlobalFlags, build_workspace_with, config_segment};
use razel_exec::Cache;
use std::path::Path;
use std::process::Command;

/// rustc available? Mirror the cc / rust_rules tests' toolchain guard.
fn have_rustc() -> bool {
    Path::new("/usr/bin/rustc").exists()
        || Command::new("rustc")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

/// Every rust compile routes through `razel-process-wrapper` (P5.4); build it (cargo, once) and
/// return its ABSOLUTE path for `RAZEL_PROCESS_WRAPPER`. (Single test in this binary, so the
/// `set_var` below is safe — mirrors `rust_rules.rs`.)
fn wrapper_path() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test exe");
    let dir = exe.parent().and_then(|p| p.parent()).expect("target/<profile>");
    let wrapper = dir.join("razel-process-wrapper");
    if !wrapper.exists() {
        let ok = Command::new(env!("CARGO"))
            .args(["build", "-p", "razel-process-wrapper"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "failed to `cargo build -p razel-process-wrapper`");
    }
    wrapper
}

#[test]
fn rust_outputs_land_in_the_output_tree_not_in_place_under_bin_tree_layout() {
    if !have_rustc() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let w = |rel: &str, body: &str| {
        let p = root.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    w(
        "app/BUILD",
        "load(\"@rules_rust//rust:defs.bzl\", \"rust_binary\")\nrust_binary(name = \"app\", srcs = [\"app.rs\"])\n",
    );
    w("app/app.rs", "fn main() { println!(\"hi from the output tree\"); }\n");

    // SAFETY: single test in this binary; point wrapper-routed compiles at the just-built wrapper.
    unsafe { std::env::set_var("RAZEL_PROCESS_WRAPPER", wrapper_path()) };
    let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

    // `bin_tree_layout` = what the CLI sets: outputs go to the `razel-out` tree, source stays clean.
    let flags = GlobalFlags { bin_tree_layout: true, ..Default::default() };
    let report = build_workspace_with(root.path(), "//app:app", &cache, flags).unwrap();

    // The binary lands in the OUTPUT TREE (`razel-out/<config>/bin/app/app`), not in-place.
    let out_rel = format!("razel-out/{}/bin/app/app", config_segment(""));
    assert!(
        report.produced.iter().any(|p| p == &out_rel),
        "binary should be in the output tree `{out_rel}`, got {:?}",
        report.produced
    );
    assert!(root.path().join(&out_rel).is_file(), "binary physically present in razel-out");
    // The source tree stays pristine — no in-place `app/app` (the pollution Q1 surfaced).
    assert!(
        !root.path().join("app/app").exists(),
        "source tree must stay clean — no in-place output"
    );

    // …and the output-tree binary actually runs.
    let out = Command::new(root.path().join(&out_rel)).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("hi from the output tree"));
}
