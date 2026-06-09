//! A3 prerequisite: `ctx.actions.run` honors a `mnemonic` (Bazel-faithful). razel previously
//! hardcoded `mnemonic = executable`, which would make cc actions un-pairable with the golden
//! (keyed on `"CppCompile"`). razel's `cc:defs.bzl` rules pass `mnemonic = "CppCompile"`/`"CppArchive"`.

use razel_loading::analyze_starlark;

#[test]
fn actions_run_honors_mnemonic() {
    let src = r#"
def _impl(ctx):
    out = ctx.actions.declare_file(ctx.attr.name + ".o")
    ctx.actions.run(executable = "cc_wrapper.sh", outputs = [out], inputs = [], arguments = [], mnemonic = "CppCompile")
    return [DefaultInfo(files = [out])]
r = rule(implementation = _impl, attrs = {})
r(name = "x")
"#;
    let t = analyze_starlark("BUILD", src).unwrap();
    assert_eq!(t[0].actions[0].mnemonic, "CppCompile"); // mnemonic param honored
    assert_eq!(t[0].actions[0].argv[0], "cc_wrapper.sh"); // executable still argv[0]
}
