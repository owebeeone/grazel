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
