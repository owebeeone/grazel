//! Layer 0 (L2 path): custom-provider instances flow ACROSS packages — the L2a debt. Each
//! package's heap dies with its eval, so after a package's analysis completes, its module
//! freezes and the captured instances harvest into the Session (heap-independent); `dep[P]`
//! falls back to the harvest. rules_cc's FunctionInfo delegate and rules_rust's dep[CrateInfo]
//! are exactly this shape. Test-first (AGENTS.md).

use razel_loading::{GlobalFlags, analyze_workspace_with};

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
