//! D4.2 — the `provider()` builtin: define a provider, construct an instance, read its fields. This
//! is the grounded next gap toward real rules_rust (`common_settings.bzl:24` does
//! `BuildSettingInfo = provider(...)`). Capture-from-return + `dep[P]` indexing are later (D4.3+).
//! Test-first (AGENTS.md).

use razel_loading::analyze_starlark;

/// A rule defines a provider, constructs an instance, and reads a field of it locally in the impl.
#[test]
fn provider_defines_and_constructs() {
    let src = r#"
MyInfo = provider(fields = ["msg"])

def _impl(ctx):
    info = MyInfo(msg = "hello")
    ctx.actions.run(executable = "echo", outputs = [], inputs = [], arguments = [info.msg])

r = rule(implementation = _impl, attrs = {})
r(name = "t")
"#;
    let targets = analyze_starlark("BUILD", src).unwrap();
    let argv = &targets[0].actions[0].argv;
    assert!(argv.contains(&"hello".to_string()), "provider instance field read: {argv:?}");
}
