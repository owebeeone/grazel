//! D1 — the `rule()` attrs schema is now CONSULTED (was discarded). Beyond A2's attr-*kinds*: the
//! declared schema supplies defaults, enforces `mandatory`, and (D1b) coerces types. Test-first
//! (AGENTS.md): these pin the schema-driven behavior the real upstream rules need.

use razel_loading::analyze_starlark;

/// D1a: an omitted attr takes its declared `default` (so rules stop needing `getattr` fallbacks).
#[test]
fn rule_applies_declared_attr_default() {
    let src = r#"
def _impl(ctx):
    ctx.actions.run(executable = "tool", outputs = [], inputs = [], arguments = [ctx.attr.greeting])

greeter = rule(implementation = _impl, attrs = {"greeting": attr.string(default = "hi")})
greeter(name = "g")  # greeting omitted -> must default to "hi"
"#;
    let targets = analyze_starlark("BUILD", src).unwrap();
    let argv = &targets[0].actions[0].argv;
    assert!(argv.contains(&"hi".to_string()), "declared default must be applied: {argv:?}");
}

/// D1b: ANY `attr.label_list` (not just the hardcoded `deps`) resolves its labels to provider
/// structs — the schema kind drives resolution.
#[test]
fn rule_resolves_label_list_attr_to_providers() {
    let src = r#"
def _impl(ctx):
    fs = []
    for d in ctx.attr.libs:
        fs = fs + d.files
    ctx.actions.run(executable = "link", outputs = [], inputs = [], arguments = fs)

linker = rule(implementation = _impl, attrs = {"libs": attr.label_list()})
filegroup(name = "a", srcs = ["a.o"])
linker(name = "t", libs = [":a"])
"#;
    let targets = analyze_starlark("BUILD", src).unwrap();
    let t = targets.iter().find(|t| t.name.ends_with("t")).unwrap();
    assert!(t.actions[0].argv.contains(&"a.o".to_string()), "label_list resolved to dep files: {:?}", t.actions[0].argv);
}

/// D1c: a single `attr.label` resolves to ONE provider struct (not a list) — `rust.bzl` uses both.
#[test]
fn rule_resolves_single_label_attr_to_one_provider() {
    let src = r#"
def _impl(ctx):
    ctx.actions.run(executable = "use", outputs = [], inputs = [], arguments = ctx.attr.lib.files)

user = rule(implementation = _impl, attrs = {"lib": attr.label()})
filegroup(name = "a", srcs = ["a.o"])
user(name = "u", lib = ":a")
"#;
    let targets = analyze_starlark("BUILD", src).unwrap();
    let u = targets.iter().find(|t| t.name.ends_with("u")).unwrap();
    assert!(u.actions[0].argv.contains(&"a.o".to_string()), "single label resolved to its files: {:?}", u.actions[0].argv);
}

/// L2: an omitted attr with NO explicit default gets its Bazel TYPE default (`label_list`→[],
/// `string`→"", `int`→0, `bool`→False) — `ctx.attr.<name>` always exists (real rules iterate
/// `ctx.attr.deps` unconditionally).
#[test]
fn omitted_attrs_get_type_defaults() {
    let src = r#"
def _impl(ctx):
    args = [str(len(ctx.attr.deps)), ctx.attr.s + "end", str(ctx.attr.n), str(ctx.attr.b)]
    ctx.actions.run(executable = "tool", outputs = [], inputs = [], arguments = args)

r = rule(implementation = _impl, attrs = {
    "deps": attr.label_list(),
    "s": attr.string(),
    "n": attr.int(),
    "b": attr.bool(),
})
r(name = "t")
"#;
    let targets = analyze_starlark("BUILD", src).unwrap();
    let argv = &targets[0].actions[0].argv;
    assert_eq!(argv[1..], ["0", "end", "0", "False"], "type defaults applied: {argv:?}");
}

/// D1a: a `mandatory` attr that's omitted is a clear analysis error (not a silent `None`).
#[test]
fn rule_errors_on_missing_mandatory_attr() {
    let src = r#"
def _impl(ctx):
    ctx.actions.run(executable = "tool", outputs = [], inputs = [], arguments = [ctx.attr.src])

r = rule(implementation = _impl, attrs = {"src": attr.string(mandatory = True)})
r(name = "x")  # src omitted -> must error
"#;
    let err = analyze_starlark("BUILD", src).unwrap_err();
    assert!(format!("{err}").contains("mandatory"), "missing mandatory attr must error: {err}");
}
