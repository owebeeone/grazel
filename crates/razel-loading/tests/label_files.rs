//! Source-file labels (L2): in Bazel, a `label_list` entry that names no declared target resolves
//! to a SOURCE FILE in the package (file existence decides). Real rules declare `srcs` as
//! `attr.label_list(allow_files=…)` — `srcs = ["lib.rs"]` must resolve to the file, not error.

use razel_loading::{GlobalFlags, analyze_workspace_with};

/// `Label.repo_name` (Bazel 7+ alias of `workspace_name` — flatbuffers' build_defs.bzl reads
/// it): repo without `@`, `""` for the main workspace. Round 30.
#[test]
fn label_repo_name_member() {
    let root = std::env::temp_dir().join(format!("razel-reponame-{}", std::process::id()));
    let pkg = root.join("app");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("BUILD"),
        r#"
def _impl(ctx):
    ext = Label("@some_repo//pkg:x")
    args = [ext.repo_name, ext.workspace_name, Label("//app:t").repo_name]
    ctx.actions.run(executable = "tool", outputs = [], inputs = [], arguments = args)

r = rule(implementation = _impl, attrs = {})
r(name = "t")
"#,
    )
    .unwrap();
    let res = analyze_workspace_with(&root, "//app:t", GlobalFlags::default());
    let _ = std::fs::remove_dir_all(&root);
    let targets = res.unwrap();
    let t = targets.iter().find(|t| t.name.ends_with(":t")).unwrap();
    assert_eq!(t.actions[0].argv[1..], ["some_repo", "some_repo", ""]);
}

/// `native.package_relative_label` (Bazel 7; flatbuffers/lite macros): a label string
/// resolves against the package BEING CONSTRUCTED (unlike `Label()`'s lexical binding);
/// an existing Label passes through unchanged. Round 31.
#[test]
fn package_relative_label_resolves_against_the_build_package() {
    let root = std::env::temp_dir().join(format!("razel-prl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("defs")).unwrap();
    // The macro lives in //defs but runs during //app's BUILD eval — the label must bind
    // to //app (package-relative), NOT to //defs (lexical).
    std::fs::write(
        root.join("defs/m.bzl"),
        r#"def mk(name):
    l = native.package_relative_label(":" + name)
    passthrough = native.package_relative_label(Label("@zzz//q:w"))
    native.filegroup(name = name + "-" + l.package + "-" + l.name + "-" + passthrough.repo_name)
"#,
    )
    .unwrap();
    std::fs::write(root.join("defs/BUILD"), "").unwrap();
    std::fs::create_dir_all(root.join("app")).unwrap();
    std::fs::write(
        root.join("app/BUILD"),
        "load(\"//defs:m.bzl\", \"mk\")\nmk(\"t\")\n",
    )
    .unwrap();
    let res = analyze_workspace_with(&root, "//app:t-app-t-zzz", GlobalFlags::default());
    let _ = std::fs::remove_dir_all(&root);
    let targets = res.unwrap();
    assert!(
        targets.iter().any(|t| t.name == "//app:t-app-t-zzz"),
        "package-relative binding + Label passthrough: {:?}",
        targets.iter().map(|t| &t.name).collect::<Vec<_>>()
    );
}

#[test]
fn label_list_entry_resolves_to_a_source_file() {
    let root = std::env::temp_dir().join(format!("razel-filelabel-{}", std::process::id()));
    let pkg = root.join("app");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(pkg.join("lib.rs"), "// src\n").unwrap();
    std::fs::write(
        pkg.join("BUILD"),
        r#"
def _impl(ctx):
    fs = []
    for s in ctx.attr.srcs:
        fs = fs + s.files
    ctx.actions.run(executable = "tool", outputs = [], inputs = fs, arguments = fs)

r = rule(implementation = _impl, attrs = {"srcs": attr.label_list()})
r(name = "t", srcs = ["lib.rs"])
"#,
    )
    .unwrap();
    let res = analyze_workspace_with(&root, "//app:t", GlobalFlags::default());
    let _ = std::fs::remove_dir_all(&root);
    let targets = res.unwrap();
    let t = targets.iter().find(|t| t.name.ends_with("t")).unwrap();
    assert!(
        t.actions[0].argv.contains(&"app/lib.rs".to_string()),
        "the file label resolved to the qualified source file: {:?}",
        t.actions[0].argv
    );
}

/// A label that matches neither a target nor a file stays a clear error.
#[test]
fn missing_target_and_file_still_errors() {
    let root = std::env::temp_dir().join(format!("razel-filelabel-miss-{}", std::process::id()));
    let pkg = root.join("app");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("BUILD"),
        r#"
def _impl(ctx):
    pass

r = rule(implementation = _impl, attrs = {"srcs": attr.label_list()})
r(name = "t", srcs = ["nope.rs"])
"#,
    )
    .unwrap();
    let res = analyze_workspace_with(&root, "//app:t", GlobalFlags::default());
    let _ = std::fs::remove_dir_all(&root);
    let err = res.unwrap_err();
    assert!(err.contains("nope.rs"), "missing file/target errors clearly: {err}");
}
