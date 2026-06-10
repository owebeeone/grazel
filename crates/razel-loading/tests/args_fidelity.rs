//! Args fidelity (V2's D3, pulled onto the L2 path): `add(name, value)` two-positional,
//! `add_all(..., before_each= / format_each= / map_each=)` — the exact shapes rules_rust's
//! `rustc.bzl` uses (56 call sites). Previously these kwargs were silently DROPPED — the
//! silent-wrong class this lane exists to kill. Test-first (AGENTS.md).

use razel_loading::analyze_starlark;

#[test]
fn args_add_and_add_all_fidelity() {
    let src = r#"
def _mapper(x):
    if x == "skip":
        return None
    return ["m-" + x]

def _impl(ctx):
    args = ctx.actions.args()
    args.add("--flag", "v")
    args.add_all(["a", "b"], before_each = "--x")
    args.add_all(["c"], format_each = "--lib=%s")
    args.add_all(["one", "skip", "two"], map_each = _mapper)
    ctx.actions.run(executable = "tool", outputs = [], inputs = [], arguments = [args])

r = rule(implementation = _impl, attrs = {})
r(name = "t")
"#;
    let targets = analyze_starlark("BUILD", src).unwrap();
    let argv = &targets[0].actions[0].argv;
    let expect =
        ["tool", "--flag", "v", "--x", "a", "--x", "b", "--lib=c", "m-one", "m-two"];
    assert_eq!(argv, &expect, "Args expansion fidelity");
}

/// `ctx.actions.write(…, is_executable = True)` chmods the output (the launcher-script shape).
#[test]
fn write_is_executable_chmods() {
    let src = r#"
def _impl(ctx):
    ctx.actions.write(output = "run.sh", content = "echo hi", is_executable = True)

r = rule(implementation = _impl, attrs = {})
r(name = "t")
"#;
    let targets = razel_loading::analyze_starlark("BUILD", src).unwrap();
    let script = &targets[0].actions[0].argv[2];
    assert!(script.contains("chmod +x"), "is_executable adds the chmod: {script}");
}
