//! L2a — custom providers FLOW between targets (RazelV3Plan §3 L2): a rule impl returns provider
//! instances; a dependent indexes them off the dep (`dep[MyInfo]`) — Bazel's provider model, the
//! mechanism real rules_rust is built on (`dep[CrateInfo]`). Test-first (AGENTS.md).

use razel_loading::analyze_starlark;

/// Capture-from-return + `dep[P]`: lib returns `MyInfo(msg=…)`; bin reads `deps[0][MyInfo].msg`.
#[test]
fn provider_returned_by_dep_is_indexable_by_dependent() {
    let src = r#"
MyInfo = provider(fields = ["msg"])

def _lib(ctx):
    return [MyInfo(msg = "from-lib"), DefaultInfo(files = ["l.o"])]

def _bin(ctx):
    info = ctx.attr.deps[0][MyInfo]
    ctx.actions.run(executable = "tool", outputs = [], inputs = [], arguments = [info.msg])

lib = rule(implementation = _lib, attrs = {})
bin = rule(implementation = _bin, attrs = {})
lib(name = "l")
bin(name = "b", deps = [":l"])
"#;
    let targets = analyze_starlark("BUILD", src).unwrap();
    let b = targets.iter().find(|t| t.name.ends_with("b")).unwrap();
    assert!(
        b.actions[0].argv.contains(&"from-lib".to_string()),
        "dep[MyInfo].msg flowed to the dependent: {:?}",
        b.actions[0].argv
    );
}

/// Indexing a provider the dep did NOT return is a clear analysis error (Bazel errors too).
#[test]
fn indexing_an_unprovided_provider_errors() {
    let src = r#"
MyInfo = provider(fields = ["msg"])
OtherInfo = provider(fields = ["x"])

def _lib(ctx):
    return [MyInfo(msg = "m")]

def _bin(ctx):
    info = ctx.attr.deps[0][OtherInfo]

lib = rule(implementation = _lib, attrs = {})
bin = rule(implementation = _bin, attrs = {})
lib(name = "l")
bin(name = "b", deps = [":l"])
"#;
    let err = analyze_starlark("BUILD", src).unwrap_err();
    assert!(
        format!("{err}").contains("does not provide"),
        "missing provider must error clearly: {err}"
    );
}

/// The dep's plain fields still read as before (`d.files` — the existing dep-struct surface).
#[test]
fn dep_files_field_still_reads() {
    let src = r#"
def _lib(ctx):
    return [DefaultInfo(files = ["l.o"])]

def _bin(ctx):
    fs = ctx.attr.deps[0].files
    ctx.actions.run(executable = "tool", outputs = [], inputs = [], arguments = fs)

lib = rule(implementation = _lib, attrs = {})
bin = rule(implementation = _bin, attrs = {})
lib(name = "l")
bin(name = "b", deps = [":l"])
"#;
    let targets = analyze_starlark("BUILD", src).unwrap();
    let b = targets.iter().find(|t| t.name.ends_with("b")).unwrap();
    assert!(b.actions[0].argv.contains(&"l.o".to_string()));
}
