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

/// Round 32 (protobuf_python's `dep[PyInfo]`, 72 pkgs): a Starlark provider read off a
/// NATIVE-rule dep (the DDS field channel — no captured instances) synthesizes an absorbing
/// instance shaped by the provider's DECLARED fields; members work in depset(transitive=)
/// (absorbed ⇒ skipped). Starlark-rule deps with captured providers keep the LOUD error.
#[test]
fn registered_provider_reads_off_native_deps_absorb() {
    let src = r#"
PyInfo2 = provider(fields = ["transitive_sources", "imports"])

def _use(ctx):
    info = ctx.attr.deps[0][PyInfo2]
    merged = depset(transitive = [info.imports])
    ctx.actions.run(executable = "tool", outputs = [], inputs = [],
                    arguments = [str(len(merged.to_list()))])

use = rule(implementation = _use, attrs = {"deps": attr.label_list()})
filegroup(name = "rt", srcs = [])
use(name = "t", deps = [":rt"])
"#;
    let targets = razel_loading::analyze_starlark("BUILD", src).unwrap();
    let t = targets.iter().find(|t| t.name == "t").unwrap();
    assert_eq!(t.actions[0].argv, ["tool", "0"], "declared-field synthesis + absorb-in-depset");
}

/// The guard: a dep whose rule RETURNED providers still errors loudly on a wrong index.
#[test]
fn missing_provider_on_starlark_dep_still_errors() {
    let src = r#"
AInfo = provider(fields = ["x"])
BInfo = provider(fields = ["y"])

def _lib(ctx):
    return [AInfo(x = 1)]

def _use(ctx):
    info = ctx.attr.deps[0][BInfo]
    ctx.actions.run(executable = "tool", outputs = [], inputs = [], arguments = [])

lib = rule(implementation = _lib, attrs = {})
use = rule(implementation = _use, attrs = {"deps": attr.label_list()})
lib(name = "l")
use(name = "t", deps = [":l"])
"#;
    let err = razel_loading::analyze_starlark("BUILD", src).unwrap_err();
    assert!(err.contains("does not provide"), "loud error preserved: {err}");
}
