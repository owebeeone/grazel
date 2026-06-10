//! Layer 0 (L2 path): custom-provider instances flow ACROSS packages — the L2a debt. Each
//! package's heap dies with its eval, so after a package's analysis completes, its module
//! freezes and the captured instances harvest into the Session (heap-independent); `dep[P]`
//! falls back to the harvest. rules_cc's FunctionInfo delegate and rules_rust's dep[CrateInfo]
//! are exactly this shape. Test-first (AGENTS.md).

use razel_loading::{GlobalFlags, analyze_workspace_with, load_tree_report};

#[test]
fn provider_instance_flows_across_packages() {
    let root = std::env::temp_dir().join(format!("razel-xpkg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    // The provider DEFINITION lives in a shared .bzl (one frozen module → one identity).
    std::fs::create_dir_all(root.join("defs")).unwrap();
    std::fs::write(
        root.join("defs/info.bzl"),
        r#"MyInfo = provider(fields = ["msg"])

def _lib_impl(ctx):
    return [MyInfo(msg = "from-a")]

info_lib = rule(implementation = _lib_impl, attrs = {})
"#,
    )
    .unwrap();
    std::fs::write(root.join("defs/BUILD"), "").unwrap();
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::write(
        root.join("a/BUILD"),
        "load(\"//defs:info.bzl\", \"info_lib\")\ninfo_lib(name = \"t\")\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("b")).unwrap();
    std::fs::write(
        root.join("b/BUILD"),
        r#"load("//defs:info.bzl", "MyInfo")

def _bin_impl(ctx):
    info = ctx.attr.deps[0][MyInfo]
    ctx.actions.run(executable = "tool", outputs = [], inputs = [], arguments = [info.msg])

bin = rule(implementation = _bin_impl, attrs = {})
bin(name = "b", deps = ["//a:t"])
"#,
    )
    .unwrap();
    let res = analyze_workspace_with(&root, "//b:b", GlobalFlags::default());
    let _ = std::fs::remove_dir_all(&root);
    let targets = res.unwrap();
    let b = targets.iter().find(|t| t.name.ends_with(":b")).unwrap();
    assert!(
        b.actions[0].argv.contains(&"from-a".to_string()),
        "the cross-package instance was indexable by the shared provider identity: {:?}",
        b.actions[0].argv
    );
}

/// TF's `tf_gen_options_header` shape (the enable_registration_v2 class, 55 pkgs):
/// a skylib-style `bool_flag` (rule with `build_setting=`, impl returns
/// `BuildSettingInfo(value = ctx.build_setting_value)`) declared in package `a`,
/// read CROSS-PACKAGE through a `label_keyed_string_dict` attr's KEY targets:
/// `target[BuildSettingInfo].value` must be the flag's `build_setting_default`.
#[test]
fn build_setting_value_flows_through_label_keyed_dict_keys() {
    let root = std::env::temp_dir().join(format!("razel-bsi-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("defs")).unwrap();
    std::fs::write(
        root.join("defs/info.bzl"),
        r#"BuildSettingInfo = provider(fields = ["value"])

def _flag_impl(ctx):
    return [BuildSettingInfo(value = ctx.build_setting_value)]

bool_flag = rule(implementation = _flag_impl, build_setting = config.bool(flag = True))
"#,
    )
    .unwrap();
    std::fs::write(root.join("defs/BUILD"), "").unwrap();
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::write(
        root.join("a/BUILD"),
        "load(\"//defs:info.bzl\", \"bool_flag\")\nbool_flag(name = \"flag\", build_setting_default = False)\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("b")).unwrap();
    std::fs::write(
        root.join("b/BUILD"),
        r#"load("//defs:info.bzl", "BuildSettingInfo")

def _gen_impl(ctx):
    vals = []
    for target, identifier in ctx.attr.settings.items():
        vals.append("%s=%s" % (identifier, target[BuildSettingInfo].value))
    ctx.actions.run(executable = "tool", outputs = [], inputs = [], arguments = vals)

gen = rule(implementation = _gen_impl, attrs = {"settings": attr.label_keyed_string_dict()})
gen(name = "b", settings = {"//a:flag": "REGISTRATION_V2"})
"#,
    )
    .unwrap();
    let res = analyze_workspace_with(&root, "//b:b", GlobalFlags::default());
    let _ = std::fs::remove_dir_all(&root);
    let targets = res.unwrap();
    let b = targets.iter().find(|t| t.name.ends_with(":b")).unwrap();
    assert!(
        b.actions[0].argv.contains(&"REGISTRATION_V2=False".to_string()),
        "build_setting value read through a label_keyed_string_dict key: {:?}",
        b.actions[0].argv
    );
}

/// TF's actual 55-package sequence (the enable_registration_v2 class): the flag's package
/// FAILS its entry drive AFTER the flag analyzed (a later target errors), so the flag sits
/// in `results` with its captured instances dead (the harvest is skipped on error). The
/// failed load must not poison the package's partial results — a consumer's dep re-load
/// re-evaluates cleanly and the provider flows.
#[test]
fn failed_entry_drive_does_not_poison_providers_for_consumers() {
    let root = std::env::temp_dir().join(format!("razel-bsi-seq-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("defs")).unwrap();
    std::fs::write(
        root.join("defs/info.bzl"),
        r#"BuildSettingInfo = provider(fields = ["value"])

def _flag_impl(ctx):
    return [BuildSettingInfo(value = ctx.build_setting_value)]

bool_flag = rule(implementation = _flag_impl, build_setting = config.bool(flag = True))

def _broken_impl(ctx):
    fail("doc-target wall")

broken = rule(implementation = _broken_impl, attrs = {})
"#,
    )
    .unwrap();
    std::fs::write(root.join("defs/BUILD"), "").unwrap();
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::write(
        root.join("a/BUILD"),
        r#"load("//defs:info.bzl", "bool_flag", "broken")
bool_flag(name = "flag", build_setting_default = False)
broken(name = "wall")
"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("b")).unwrap();
    std::fs::write(
        root.join("b/BUILD"),
        r#"load("//defs:info.bzl", "BuildSettingInfo")

def _gen_impl(ctx):
    vals = []
    for target, identifier in ctx.attr.settings.items():
        vals.append("%s=%s" % (identifier, target[BuildSettingInfo].value))
    ctx.actions.run(executable = "tool", outputs = [], inputs = [], arguments = vals)

gen = rule(implementation = _gen_impl, attrs = {"settings": attr.label_keyed_string_dict()})
gen(name = "b", settings = {"//a:flag": "X"})
"#,
    )
    .unwrap();
    let report = load_tree_report(
        &root,
        GlobalFlags::default(),
        &["a".to_string(), "b".to_string()],
    );
    let _ = std::fs::remove_dir_all(&root);
    let a = report.iter().find(|(p, _)| p == "a").unwrap();
    assert!(a.1.is_err(), "package a's entry drive fails (the wall)");
    let b = report.iter().find(|(p, _)| p == "b").unwrap();
    assert!(
        b.1.is_ok(),
        "consumer must re-load the failed package cleanly and read the flag: {:?}",
        b.1
    );
}
