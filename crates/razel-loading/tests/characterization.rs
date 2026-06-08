//! Phase 1.1 — characterization snapshots of the LIVE analysis path
//! (`analyze_bazel`/`analyze_starlark` → `AnalyzedTarget`/`AnalyzedAction`).
//!
//! These pin the *current* declared output (full argv/inputs/outputs) so the Phase 1.4/1.5
//! Session migration (the 7 thread-locals → a passed `Analysis` handle) cannot silently
//! regress it. This is a regression guard against razel's OWN output — NOT Bazel parity
//! (that is the separate golden harness, `RazelParityHarness.md`; razel's lowering here
//! deliberately differs from Bazel's feature-config command line).

use razel_loading::{analyze_bazel, analyze_starlark};

const CC_BUILD: &str = r#"
load("@rules_cc//cc:defs.bzl", "cc_library", "cc_binary")
cc_library(name = "greet", srcs = ["greet.cc"], hdrs = ["greet.h"])
cc_binary(name = "app", srcs = ["main.cc"], deps = [":greet"])
"#;

#[test]
fn cc_library_lowering_is_stable() {
    let targets = analyze_bazel(CC_BUILD).unwrap();
    let greet = targets.iter().find(|t| t.name == "greet").unwrap();
    assert_eq!(greet.actions.len(), 2, "cc_library = compile + archive");

    let compile = &greet.actions[0];
    assert_eq!(compile.mnemonic, "CppCompile");
    assert_eq!(compile.argv, ["/usr/bin/c++", "-iquote", ".", "-c", "greet.cc", "-o", "greet.cc.o"]);
    assert_eq!(compile.inputs, ["greet.cc", "greet.h"]);
    assert_eq!(compile.outputs, ["greet.cc.o"]);

    let archive = &greet.actions[1];
    assert_eq!(archive.mnemonic, "CppArchive");
    assert_eq!(archive.argv, ["/usr/bin/ar", "rcs", "libgreet.a", "greet.cc.o"]);
    assert_eq!(archive.inputs, ["greet.cc.o"]);
    assert_eq!(archive.outputs, ["libgreet.a"]);

    assert_eq!(greet.default_info, ["libgreet.a"]);
}

#[test]
fn cc_binary_links_dep_and_sees_transitive_header() {
    let targets = analyze_bazel(CC_BUILD).unwrap();
    let app = targets.iter().find(|t| t.name == "app").unwrap();
    assert_eq!(app.deps, ["greet"]);
    assert_eq!(app.actions.len(), 2, "cc_binary = compile + link");

    let compile = &app.actions[0];
    assert_eq!(compile.mnemonic, "CppCompile");
    assert_eq!(compile.argv, ["/usr/bin/c++", "-iquote", ".", "-c", "main.cc", "-o", "main.cc.o"]);
    // The dep's public header propagates into the dependent's compile inputs.
    assert_eq!(compile.inputs, ["main.cc", "greet.h"]);

    let link = &app.actions[1];
    assert_eq!(link.mnemonic, "CppLink");
    assert_eq!(link.argv, ["/usr/bin/c++", "-o", "app", "main.cc.o", "libgreet.a"]);
    assert_eq!(link.inputs, ["main.cc.o", "libgreet.a"]); // dep's archive linked in
    assert_eq!(link.outputs, ["app"]);
    assert_eq!(app.default_info, ["app"]);
}

#[test]
fn legacy_macro_expands_to_native_rules() {
    // A plain Starlark `def` (legacy macro) instantiating native rules expands on the live path.
    let src = r#"
load("@rules_cc//cc:defs.bzl", "cc_library", "cc_binary")
def my_component(name):
    cc_library(name = name + "_lib", srcs = [name + ".cc"])
    cc_binary(name = name, srcs = ["main.cc"], deps = [":" + name + "_lib"])

my_component(name = "widget")
"#;
    let targets = analyze_bazel(src).unwrap();
    assert_eq!(targets.len(), 2);
    assert_eq!(targets[0].name, "widget_lib");
    let bin = targets.iter().find(|t| t.name == "widget").unwrap();
    assert_eq!(bin.deps, ["widget_lib"]);
}

#[test]
fn starlark_rule_impl_declared_action_is_stable() {
    // The user-`rule()` path (running a Starlark impl) — pins the declared action verbatim.
    let src = r#"
def _impl(ctx):
    out = ctx.actions.declare_file(ctx.attr.name + ".o")
    ctx.actions.run(executable = "cc", outputs = [out], inputs = [ctx.attr.src], arguments = ["-c", ctx.attr.src])
    return [DefaultInfo(files = [out])]

cc_thing = rule(implementation = _impl, attrs = {"src": 1})
cc_thing(name = "widget", src = "widget.c")
"#;
    let targets = analyze_starlark("BUILD", src).unwrap();
    assert_eq!(targets.len(), 1);
    let a = &targets[0].actions[0];
    assert_eq!(a.mnemonic, "cc");
    assert_eq!(a.argv, ["cc", "-c", "widget.c"]); // executable is argv[0]
    assert_eq!(a.inputs, ["widget.c"]);
    assert_eq!(a.outputs, ["widget.o"]);
    assert_eq!(targets[0].default_info, ["widget.o"]);
}
