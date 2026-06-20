//! D4 — loading REAL vendored external rulesets from `third-party/`. razel resolves `@repo//pkg:file`
//! to a real file (with the `_`/`-` repo-dir convention) and evaluates it, so a BUILD can `load()`
//! upstream Starlark — the foundation for running real rules_rust. Test-first (AGENTS.md).

use razel_loading::{CrateLock, GlobalFlags, analyze_workspace_with};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

/// L2: loads INSIDE an external repo are repo-relative — `//pkg:f.bzl` and `:f.bzl` written in a
/// `@my_repo` module resolve against `my_repo`'s root, not the workspace (Bazel label semantics;
/// rules_cc's internals load `//cc/private:…` this way).
#[test]
fn external_repo_internal_loads_are_repo_relative() {
    let base = std::env::temp_dir().join(format!("razel-d4-rel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let repo = base.join("my-repo");
    std::fs::create_dir_all(repo.join("lib")).unwrap();
    std::fs::create_dir_all(repo.join("other")).unwrap();
    // lib/defs.bzl loads a repo-relative //other:helper.bzl AND a package-relative :sibling.bzl.
    std::fs::write(repo.join("other/helper.bzl"), "helper_part = \"hel\"\n").unwrap();
    std::fs::write(repo.join("lib/sibling.bzl"), "sibling_part = \"per\"\n").unwrap();
    std::fs::write(
        repo.join("lib/defs.bzl"),
        "load(\"//other:helper.bzl\", \"helper_part\")\n\
         load(\":sibling.bzl\", \"sibling_part\")\n\
         word = helper_part + sibling_part\n",
    )
    .unwrap();
    let root = base.join("ws");
    std::fs::create_dir_all(root.join("app")).unwrap();
    std::fs::write(
        root.join("app/BUILD"),
        "load(\"@my_repo//lib:defs.bzl\", \"word\")\nfilegroup(name = word, srcs = [])\n",
    )
    .unwrap();
    let flags = GlobalFlags { external_base: Some(base.clone()), ..Default::default() };
    let res = analyze_workspace_with(&root, "//app:helper", flags);
    let _ = std::fs::remove_dir_all(&base);
    let targets = res.expect("repo-relative loads inside an external repo resolve");
    assert!(
        targets.iter().any(|t| t.name.ends_with("helper")),
        "word assembled from repo-relative + package-relative loads: {:?}",
        targets.iter().map(|t| &t.name).collect::<Vec<_>>()
    );
}

/// Regression gate for the `@crates` clean-checkout bug (fix `e4d87c2`): on a FRESH checkout the
/// workspace BUILD's very first `load("@crates//:defs.bzl", …)` must MATERIALIZE the root `@crates`
/// repo on demand, not die with "unsupported load path". The root repo is INLINE (`root_contents`),
/// so this is network-free. The bug hid because the only catching test (`dogfood_selfhost.rs`) is
/// `#[ignore]` — this is the committed gate so it can't silently regress.
#[test]
fn clean_checkout_first_crates_load_materializes_the_root() {
    let base = std::env::temp_dir().join(format!("razel-crates-seed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("ws");
    std::fs::create_dir_all(&root).unwrap();
    // The workspace's first load is the @crates root defs.bzl (the crate_universe macro layer).
    std::fs::write(
        root.join("BUILD"),
        "load(\"@crates//:defs.bzl\", \"GROUP\")\nfilegroup(name = GROUP, srcs = [])\n",
    )
    .unwrap();
    // A synthetic lock whose ROOT @crates repo provides defs.bzl inline (no fetch, no network).
    let lock = CrateLock {
        version: 1,
        root_contents: BTreeMap::from([
            ("BUILD.bazel".to_string(), "exports_files([])\n".to_string()),
            ("defs.bzl".to_string(), "GROUP = \"crates_ok\"\n".to_string()),
        ]),
        crates: BTreeMap::new(),
        recorded_inputs: vec![],
        canonical_prefix: "rules_rust++crate+".to_string(),
    };
    // CLEAN checkout: nothing materialized yet — the loader must self-seed the root from the lock.
    let fetched = root.join(".razel-crates");
    assert!(!fetched.exists(), "precondition: nothing materialized");
    let flags = GlobalFlags {
        crate_lock: Some(Arc::new(lock)),
        fetched_external_base: Some(fetched.clone()),
        ..Default::default()
    };
    let res = analyze_workspace_with(&root, "//:crates_ok", flags);
    let materialized = fetched.join("rules_rust++crate+crates/defs.bzl").exists();
    let _ = std::fs::remove_dir_all(&base);
    let targets =
        res.expect("first @crates//:defs.bzl load self-materializes the root (no 'unsupported load path')");
    assert!(
        targets.iter().any(|t| t.name.ends_with("crates_ok")),
        "the root defs.bzl's GROUP drove the target name: {:?}",
        targets.iter().map(|t| &t.name).collect::<Vec<_>>()
    );
    assert!(materialized, "the root @crates repo was materialized on demand");
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

/// D4.3: real bazel_skylib `common_settings.bzl` loads — it needs `provider()`, the `config.*` /
/// `platform_common` builtin stubs, and `rule()` absorbing extra kwargs (`build_setting`, `doc`).
#[test]
fn loads_real_bazel_skylib_common_settings() {
    let tp = third_party();
    assert!(
        tp.join("bazel-skylib/rules/common_settings.bzl").exists(),
        "vendored bazel_skylib common_settings expected at {}",
        tp.display()
    );
    let root = std::env::temp_dir().join(format!("razel-d4-cs-{}", std::process::id()));
    let pkg = root.join("app");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("BUILD"),
        "load(\"@bazel_skylib//rules:common_settings.bzl\", \"BuildSettingInfo\")\n\
         filegroup(name = \"x\", srcs = [])\n",
    )
    .unwrap();
    let flags = GlobalFlags { external_base: Some(tp), ..Default::default() };
    let res = analyze_workspace_with(&root, "//app:x", flags);
    let _ = std::fs::remove_dir_all(&root);
    let targets = res.expect("real common_settings.bzl loads");
    assert!(targets.iter().any(|t| t.name.ends_with("x")));
}
