//! A1 (RazelStarlarkBoundaryPlan §10): the `razel_cc` builtin namespace, called from a `rule()`
//! impl. `razel_cc.command_line(action, variables)` wraps `Constrain` over the macOS cc config; a
//! rule impl feeds its result to `ctx.actions.run`, and the declared action's argv must equal the
//! golden's `CppCompile` command line. Proves the engine is reachable + correct *through Starlark*.

use razel_loading::analyze_starlark;

const SRC: &str = r#"
def _impl(ctx):
    cl = razel_cc.command_line("c++-compile", {
        "source_file": "corpus/cc/transitive/util.cc",
        "output_file": "bazel-out/<cfg>/bin/corpus/cc/transitive/_objs/util/util.o",
        "dependency_file": "bazel-out/<cfg>/bin/corpus/cc/transitive/_objs/util/util.d",
        "minimum_os_version": "<sdk>",
        "quote_include_paths": [".", "bazel-out/<cfg>/bin"],
    })
    out = ctx.actions.declare_file(ctx.attr.name + ".o")
    ctx.actions.run(executable = cl[0], outputs = [out], inputs = [], arguments = cl[1:])
    return [DefaultInfo(files = [out])]

cc_compile = rule(implementation = _impl, attrs = {})
cc_compile(name = "util")
"#;

#[test]
fn razel_cc_command_line_builtin_reproduces_the_golden_compile_argv() {
    let targets = analyze_starlark("BUILD", SRC).unwrap();
    let action = &targets[0].actions[0];

    // The golden's util CppCompile argv (parsed via the A0 runner).
    let golden = razel_parity::parse_golden(include_str!(
        "../../../parity/corpus/cc/transitive/golden.txt"
    ));
    let util_compile = golden
        .iter()
        .find(|a| a.mnemonic == "CppCompile" && a.outputs.iter().any(|o| o.ends_with("util/util.o")))
        .expect("util CppCompile in golden");

    // The builtin, driven from a rule() impl, produced exactly Bazel's command line.
    assert_eq!(action.argv, util_compile.argv);
}
