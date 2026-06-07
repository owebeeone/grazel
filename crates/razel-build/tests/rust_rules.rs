//! Integration test: build a real Bazel workspace whose targets `load()` rust rules
//! from `@rules_rust`, resolved to razel's native rust_binary/rust_library (one
//! `rustc` action each). Proves a cross-package crate dep: `//app:app` depends on
//! `//lib:greet`, links its rlib via `--extern greet=...`, and `use greet::greet()`
//! resolves at runtime.

use razel_build::build_workspace;
use razel_exec::Cache;
use std::path::Path;
use std::process::Command;

/// rustc available? Mirror the cc tests' toolchain guard.
fn have_rustc() -> bool {
    Path::new("/usr/bin/rustc").exists()
        || Command::new("rustc")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

#[test]
fn builds_and_runs_rust_binary_with_library_dep() {
    if !have_rustc() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let w = |rel: &str, body: &str| {
        let p = root.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };

    // lib package — a rust_library exporting an rlib.
    w(
        "lib/BUILD",
        r#"load("@rules_rust//rust:defs.bzl", "rust_library")
rust_library(name = "greet", srcs = ["greet.rs"])
"#,
    );
    w(
        "lib/greet.rs",
        "pub fn greet() -> &'static str { \"hi from rust\" }\n",
    );

    // app package — a rust_binary depending on //lib:greet.
    w(
        "app/BUILD",
        r#"load("@rules_rust//rust:defs.bzl", "rust_binary")
rust_binary(name = "app", srcs = ["app.rs"], deps = ["//lib:greet"])
"#,
    );
    w(
        "app/app.rs",
        "fn main() { println!(\"{}\", greet::greet()); }\n",
    );

    let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

    // Cross-package: //lib:greet is loaded on demand while analyzing app.
    let report = build_workspace(root.path(), "//app:app", &cache).unwrap();
    assert_eq!(report.executed, 2, "1 rlib compile + 1 binary compile");
    assert!(report.produced.contains(&"app/app".to_string()));
    assert!(report.produced.contains(&"lib/libgreet.rlib".to_string()));

    let out = Command::new(root.path().join("app/app")).output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("hi from rust"),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}
