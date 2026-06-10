//! Source-file labels (L2): in Bazel, a `label_list` entry that names no declared target resolves
//! to a SOURCE FILE in the package (file existence decides). Real rules declare `srcs` as
//! `attr.label_list(allow_files=…)` — `srcs = ["lib.rs"]` must resolve to the file, not error.

use razel_loading::{GlobalFlags, analyze_workspace_with};

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
