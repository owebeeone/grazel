//! Aspect propagation semantics. These pin the loading-grade model closely enough for TF:
//! entry attrs apply aspects, `attr_aspects` controls recursive propagation, and each aspect
//! value has distinct provider results.

use razel_loading::analyze_starlark;

#[test]
fn aspect_without_attr_aspects_does_not_recurse_through_deps() {
    let src = r#"
SeenInfo = provider(fields = ["seen"])

def _aspect_impl(target, ctx):
    seen = []
    if hasattr(ctx.rule.attr, "deps"):
        for dep in ctx.rule.attr.deps:
            if SeenInfo in dep:
                seen.append(dep[SeenInfo].seen)
    if not seen:
        seen = [str(ctx.label)]
    return [SeenInfo(seen = ",".join(seen))]

no_recurse = aspect(
    implementation = _aspect_impl,
    attr_aspects = [],
)

def _node_impl(ctx):
    return []

def _use_impl(ctx):
    ctx.actions.run(
        executable = "tool",
        outputs = [],
        inputs = [],
        arguments = [ctx.attr.deps[0][SeenInfo].seen],
    )

node = rule(implementation = _node_impl, attrs = {"deps": attr.label_list()})
use = rule(implementation = _use_impl, attrs = {
    "deps": attr.label_list(aspects = [no_recurse]),
})

node(name = "leaf")
node(name = "mid", deps = [":leaf"])
use(name = "top", deps = [":mid"])
"#;

    let targets = analyze_starlark("BUILD", src).unwrap();
    let top = targets.iter().find(|t| t.name == "top").unwrap();
    assert_eq!(
        top.actions[0].argv,
        ["tool", "//:mid"],
        "the aspect applies to the direct dep only; it must not recurse into :leaf"
    );
}

#[test]
fn aspect_attr_aspects_controls_non_deps_attrs() {
    let src = r#"
SeenInfo = provider(fields = ["seen"])

def _aspect_impl(target, ctx):
    seen = []
    if hasattr(ctx.rule.attr, "data"):
        for dep in ctx.rule.attr.data:
            if SeenInfo in dep:
                seen.append(dep[SeenInfo].seen)
    if not seen:
        seen = [str(ctx.label)]
    return [SeenInfo(seen = ",".join(seen))]

on_data = aspect(
    implementation = _aspect_impl,
    attr_aspects = ["data"],
)

def _node_impl(ctx):
    return []

def _use_impl(ctx):
    ctx.actions.run(
        executable = "tool",
        outputs = [],
        inputs = [],
        arguments = [ctx.attr.deps[0][SeenInfo].seen],
    )

node = rule(implementation = _node_impl, attrs = {"data": attr.label_list()})
use = rule(implementation = _use_impl, attrs = {
    "deps": attr.label_list(aspects = [on_data]),
})

node(name = "leaf")
node(name = "mid", data = [":leaf"])
use(name = "top", deps = [":mid"])
"#;

    let targets = analyze_starlark("BUILD", src).unwrap();
    let top = targets.iter().find(|t| t.name == "top").unwrap();
    assert_eq!(
        top.actions[0].argv,
        ["tool", "//:leaf"],
        "attr_aspects=[\"data\"] must propagate through data, not only deps"
    );
}

#[test]
fn two_aspects_on_same_dep_do_not_share_cached_provider_results() {
    let src = r#"
OneInfo = provider(fields = ["v"])
TwoInfo = provider(fields = ["v"])

def _one_impl(target, ctx):
    return [OneInfo(v = "one")]

def _two_impl(target, ctx):
    return [TwoInfo(v = "two")]

one_aspect = aspect(implementation = _one_impl, attr_aspects = [])
two_aspect = aspect(implementation = _two_impl, attr_aspects = [])

def _leaf_impl(ctx):
    return []

def _use_impl(ctx):
    dep = ctx.attr.deps[0]
    ctx.actions.run(
        executable = "tool",
        outputs = [],
        inputs = [],
        arguments = [dep[OneInfo].v, dep[TwoInfo].v],
    )

leaf = rule(implementation = _leaf_impl, attrs = {})
use = rule(implementation = _use_impl, attrs = {
    "deps": attr.label_list(aspects = [one_aspect, two_aspect]),
})

leaf(name = "leaf")
use(name = "top", deps = [":leaf"])
"#;

    let targets = analyze_starlark("BUILD", src).unwrap();
    let top = targets.iter().find(|t| t.name == "top").unwrap();
    assert_eq!(top.actions[0].argv, ["tool", "one", "two"]);
}
