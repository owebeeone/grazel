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

/// T-001: depset elements reach map_each as LIVE values (File has .path), not strings.
#[test]
fn depset_elements_keep_their_type_for_map_each() {
    let src = r#"
def _mapper(f):
    return ["p-" + f.path]

def _impl(ctx):
    d = depset(ctx.files.srcs)
    args = ctx.actions.args()
    args.add_all(d, map_each = _mapper)
    ctx.actions.run(executable = "tool", outputs = [], inputs = [], arguments = [args])

r = rule(implementation = _impl, attrs = {})
r(name = "t", srcs = ["a.c", "b.c"])
"#;
    let targets = razel_loading::analyze_starlark("BUILD", src).unwrap();
    let argv = &targets[0].actions[0].argv;
    assert_eq!(argv, &["tool", "p-a.c", "p-b.c"], "File elements kept .path through the depset");
}

/// FROZEN depsets behave like depsets (round 30 — the TF `depset has no attribute 'count'`
/// class, 70 pkgs): a module-level depset freezes with its .bzl; the live-only downcasts made
/// `add_all(map_each=)` pass the WHOLE depset to the mapper (protobuf's proto_common.bzl:196
/// shape), `.to_list()` return `[]`, and `depset(transitive=[frozen])` skip members — all
/// silently wrong.
#[test]
fn frozen_depsets_behave_like_depsets() {
    let root = std::env::temp_dir().join(format!("razel-frozen-depset-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("app")).unwrap();
    std::fs::write(
        root.join("app/defs.bzl"),
        "PATHS = depset([\"x/y\", \"z\"])\n",
    )
    .unwrap();
    std::fs::write(
        root.join("app/BUILD"),
        r#"load("//app:defs.bzl", "PATHS")

def _mapper(p):
    return ["s-%d" % p.count("/")]

def _impl(ctx):
    args = ctx.actions.args()
    args.add_all(PATHS, map_each = _mapper)
    merged = depset(["m"], transitive = [PATHS])
    args.add_all([str(len(PATHS.to_list())), str(len(merged.to_list()))])
    ctx.actions.run(executable = "tool", outputs = [], inputs = [], arguments = [args])

r = rule(implementation = _impl, attrs = {})
r(name = "t")
"#,
    )
    .unwrap();
    let res = razel_loading::analyze_workspace_with(
        &root,
        "//app:t",
        razel_loading::GlobalFlags::default(),
    );
    let _ = std::fs::remove_dir_all(&root);
    let targets = res.unwrap();
    let t = targets.iter().find(|t| t.name.ends_with(":t")).unwrap();
    assert_eq!(
        t.actions[0].argv,
        ["tool", "s-1", "s-0", "2", "3"],
        "map_each per element; to_list real; transitive merged"
    );
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
