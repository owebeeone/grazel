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

/// L2: `provider(init=…)` returns the Bazel 2-tuple `(Provider, raw_ctor)` — rules_cc's CcInfo
/// is defined exactly this way (`cc_info.bzl:272`). The main callable routes through `init`
/// (kwargs → field dict); the raw ctor bypasses it; both construct instances of the SAME
/// provider (a dependent's `dep[P]` finds raw-made instances).
#[test]
fn provider_init_returns_tuple_and_routes_construction() {
    let src = r#"
def _init(x = 0, y = 0):
    return {"sum": x + y}

PairInfo, _raw_pair = provider(fields = ["sum"], init = _init)

def _lib(ctx):
    return [_raw_pair(sum = 42)]

def _bin(ctx):
    made = PairInfo(x = 1, y = 2)
    got = ctx.attr.deps[0][PairInfo]
    ctx.actions.run(executable = "tool", outputs = [], inputs = [],
                    arguments = [str(made.sum), str(got.sum)])

lib = rule(implementation = _lib, attrs = {})
bin = rule(implementation = _bin, attrs = {})
lib(name = "l")
bin(name = "b", deps = [":l"])
"#;
    let targets = analyze_starlark("BUILD", src).unwrap();
    let b = targets.iter().find(|t| t.name.ends_with("b")).unwrap();
    assert_eq!(
        b.actions[0].argv[1..],
        ["3".to_string(), "42".to_string()],
        "init routed (1+2=3) and the raw-made instance was indexable as PairInfo: {:?}",
        b.actions[0].argv
    );
}

/// D4.4: the Bazel builtin global stubs all resolve (builtin providers, the namespace stubs,
/// transition/configuration_field) — what lets real upstream `.bzl` compile past their free vars.
#[test]
fn bazel_builtin_globals_resolve() {
    let src = r#"
def _impl(ctx):
    _refs = [RunEnvironmentInfo, OutputGroupInfo, configuration_field, transition,
             config, platform_common, config_common, cc_common, coverage_common, testing]
    ctx.actions.run(executable = "noop", outputs = [], inputs = [], arguments = [])

r = rule(implementation = _impl, attrs = {})
r(name = "t")
"#;
    let targets = analyze_starlark("BUILD", src).unwrap();
    assert!(
        targets.iter().any(|t| t.name.ends_with("t") && !t.actions.is_empty()),
        "all Bazel builtin globals resolved + the rule analyzed"
    );
}
