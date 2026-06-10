//! E0a — forward references (RazelV3Plan §2). Bazel loads a whole package before analyzing, so
//! BUILD targets may reference later-declared ones. razel's eager same-scope analysis made this an
//! error ("dep not analyzed yet"); the E0 phase split (record declarations, analyze demand-driven)
//! makes it work. Test-first: this fails by construction before E0b.

use razel_loading::analyze_starlark;

/// A target deps on a later-declared one — the canonical real-world BUILD shape.
#[test]
fn forward_reference_within_a_package_analyzes() {
    let src = r#"
def _impl(ctx):
    fs = []
    for d in ctx.attr.deps:
        fs = fs + d.files
    out = ctx.actions.declare_file(ctx.attr.name + ".o")
    ctx.actions.run(executable = "tool", outputs = [out], inputs = fs, arguments = fs)
    return [DefaultInfo(files = [out])]

r = rule(implementation = _impl, attrs = {})
r(name = "app", deps = [":lib"])  # forward reference — :lib is declared BELOW
r(name = "lib", deps = [])
"#;
    let targets = analyze_starlark("BUILD", src).unwrap();
    let app = targets.iter().find(|t| t.name.ends_with("app")).unwrap();
    assert!(
        app.actions[0].argv.contains(&"lib.o".to_string()),
        "app resolved the forward-referenced lib's DefaultInfo: {:?}",
        app.actions[0].argv
    );
}

/// E0c: NATIVE rules forward-reference too — a cc_library deps on a later-declared one
/// (the deferred-native path: record at eval, analyze on demand).
#[test]
fn native_rule_forward_reference_analyzes() {
    let src = r#"
load("@rules_cc//cc:cc_library.bzl", "cc_library")
cc_library(name = "app", srcs = ["app.c"], hdrs = ["app.h"], deps = [":lib"])
cc_library(name = "lib", srcs = [], hdrs = ["lib.h"])
"#;
    let targets = razel_loading::analyze_bazel(src).unwrap();
    let app = targets.iter().find(|t| t.name.ends_with("app")).unwrap();
    let compile = &app.actions[0];
    assert!(
        compile.inputs.contains(&"lib.h".to_string()),
        "app's compile saw the forward-referenced lib's headers: {:?}",
        compile.inputs
    );
}

/// A dependency cycle is a clear analysis error, not a hang or a silent skip.
#[test]
fn dependency_cycle_is_an_error() {
    let src = r#"
def _impl(ctx):
    for d in ctx.attr.deps:
        pass
    return [DefaultInfo(files = [])]

r = rule(implementation = _impl, attrs = {})
r(name = "a", deps = [":b"])
r(name = "b", deps = [":a"])
"#;
    let err = analyze_starlark("BUILD", src).unwrap_err();
    assert!(format!("{err}").contains("cycle"), "cycle must be reported: {err}");
}
