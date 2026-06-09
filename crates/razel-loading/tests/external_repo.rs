//! D4 — loading REAL vendored external rulesets from `third-party/`. razel resolves `@repo//pkg:file`
//! to a real file (with the `_`/`-` repo-dir convention) and evaluates it, so a BUILD can `load()`
//! upstream Starlark — the foundation for running real rules_rust. Test-first (AGENTS.md).

use razel_loading::{GlobalFlags, analyze_workspace_with};
use std::path::{Path, PathBuf};

fn third_party() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../third-party")
}

/// D4.1 (mechanism, hermetic): a BUILD `load()`s a vendored external-repo `.bzl` that exports a
/// `struct` value, and the repo `@my_repo` resolves to the hyphenated dir `my-repo` (the `_`→`-`
/// convention). Proves real-file external loading independent of the real vendored repos.
#[test]
fn loads_external_repo_bzl_with_underscore_hyphen_mapping() {
    let base = std::env::temp_dir().join(format!("razel-d4-ext-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    // repo `@my_repo` lives in dir `my-repo` (hyphen) → exercises the `_`→`-` fallback.
    let repo_lib = base.join("my-repo/lib");
    std::fs::create_dir_all(&repo_lib).unwrap();
    // Mirror paths.bzl's shape: a module docstring + a def + a struct holding the def (called later).
    std::fs::write(
        repo_lib.join("defs.bzl"),
        "\"\"\"doc\"\"\"\ndef _leaf(p):\n    return p.split(\"/\")[-1]\nvals = struct(leaf = _leaf)\n",
    )
    .unwrap();
    let root = base.join("ws");
    let pkg = root.join("app");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("BUILD"),
        "load(\"@my_repo//lib:defs.bzl\", \"vals\")\nfilegroup(name = vals.leaf(\"x/leaf.o\"), srcs = [])\n",
    )
    .unwrap();
    let flags = GlobalFlags { external_base: Some(base.clone()), ..Default::default() };
    let res = analyze_workspace_with(&root, "//app:leaf.o", flags);
    let _ = std::fs::remove_dir_all(&base);
    let targets = res.expect("load external-repo .bzl exporting a struct");
    assert!(
        targets.iter().any(|t| t.name.ends_with("leaf.o")),
        "external repo's struct value drove the target name: {:?}",
        targets.iter().map(|t| &t.name).collect::<Vec<_>>()
    );
}

/// D4.1 (real): the same, against real vendored bazel_skylib `paths.bzl` (`third-party/bazel-skylib`).
#[test]
fn loads_real_bazel_skylib_paths() {
    let tp = third_party();
    assert!(
        tp.join("bazel-skylib/lib/paths.bzl").exists(),
        "vendored bazel_skylib expected at {}/bazel-skylib",
        tp.display()
    );
    let root = std::env::temp_dir().join(format!("razel-d4-sky-{}", std::process::id()));
    let pkg = root.join("app");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("BUILD"),
        "load(\"@bazel_skylib//lib:paths.bzl\", \"paths\")\n\
         filegroup(name = paths.basename(\"x/leaf.o\"), srcs = [])\n",
    )
    .unwrap();
    let flags = GlobalFlags { external_base: Some(tp), ..Default::default() };
    let res = analyze_workspace_with(&root, "//app:leaf.o", flags);
    let _ = std::fs::remove_dir_all(&root);
    let targets = res.expect("workspace analysis with a real skylib load");
    assert!(targets.iter().any(|t| t.name.ends_with("leaf.o")));
}
