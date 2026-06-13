//! Characterization of the remaining TF-load scaling shape: a target can be present in the
//! Session results map while its Starlark provider instances were captured in an entry package
//! that later failed before normal freeze/harvest. The loader must salvage those captures so
//! later consumers do not locally re-analyze the harvested declaration just to recover providers.

use razel_loading::{GlobalFlags, SchedHook, load_tree_report_with_threads};
use std::sync::{Arc, Mutex};

fn write_defs(root: &std::path::Path, failing: bool) {
    std::fs::create_dir_all(root.join("defs")).unwrap();
    std::fs::write(
        root.join("defs/info.bzl"),
        format!(
            r#"MyInfo = provider(fields = ["msg"])

def _lib_impl(ctx):
    return [MyInfo(msg = ctx.attr.msg)]

lib = rule(implementation = _lib_impl, attrs = {{"msg": attr.string()}})

def _use_impl(ctx):
    msg = ctx.attr.deps[0][MyInfo].msg
{body}

use = rule(implementation = _use_impl, attrs = {{"deps": attr.label_list()}})
"#,
            body = if failing {
                r#"    fail("after provider read: " + msg)"#
            } else {
                r#"    ctx.actions.run(executable = "tool", outputs = [], inputs = [], arguments = [msg])"#
            }
        ),
    )
    .unwrap();
    std::fs::write(root.join("defs/BUILD"), "").unwrap();
}

fn write_empty_provider_defs(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("defs")).unwrap();
    std::fs::write(
        root.join("defs/info.bzl"),
        r#"def _empty_impl(ctx):
    return []

empty = rule(implementation = _empty_impl, attrs = {})

def _use_impl(ctx):
    ctx.actions.run(executable = "tool", outputs = [], inputs = [], arguments = [str(len(ctx.attr.deps))])

use = rule(implementation = _use_impl, attrs = {"deps": attr.label_list()})
"#,
    )
    .unwrap();
    std::fs::write(root.join("defs/BUILD"), "").unwrap();
}

fn write_empty_shared_dep(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("dep")).unwrap();
    std::fs::write(
        root.join("dep/BUILD"),
        r#"load("//defs:info.bzl", "empty")
empty(name = "shared")
"#,
    )
    .unwrap();
}

fn write_shared_dep(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("dep")).unwrap();
    std::fs::write(
        root.join("dep/BUILD"),
        r#"load("//defs:info.bzl", "lib")
lib(name = "shared", msg = "shared")
"#,
    )
    .unwrap();
}

fn write_consumers(root: &std::path::Path, n: usize) -> Vec<String> {
    let mut packages = Vec::new();
    for i in 0..n {
        let pkg = format!("c{i}");
        packages.push(pkg.clone());
        std::fs::create_dir_all(root.join(&pkg)).unwrap();
        std::fs::write(
            root.join(&pkg).join("BUILD"),
            r#"load("//defs:info.bzl", "use")
use(name = "top", deps = ["//dep:shared"])
"#,
        )
        .unwrap();
    }
    packages
}

fn flags_with_events(events: Arc<Mutex<Vec<(String, String)>>>) -> GlobalFlags {
    let mut flags = GlobalFlags::default();
    flags.sched_hook = Some(SchedHook(Arc::new(move |point, key| {
        events
            .lock()
            .unwrap()
            .push((point.to_string(), key.to_string()));
    })));
    flags
}

fn count(events: &Mutex<Vec<(String, String)>>, point: &str, key: &str) -> usize {
    events
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, k)| p == point && k == key)
        .count()
}

#[test]
fn failed_entries_salvage_shared_starlark_dep_providers() {
    let root = std::env::temp_dir().join(format!("razel-demand-failed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    write_defs(&root, true);
    write_shared_dep(&root);
    let packages = write_consumers(&root, 4);
    let events = Arc::new(Mutex::new(Vec::new()));

    let (report, _) = load_tree_report_with_threads(
        &root,
        flags_with_events(events.clone()),
        &packages,
        Vec::new(),
        1,
    );
    let _ = std::fs::remove_dir_all(&root);

    for (pkg, r) in &report {
        assert!(
            r.is_err(),
            "fixture entries should fail after reading the provider: {pkg}"
        );
    }
    assert_eq!(
        count(&events, "provider-reanalyze", "//dep:shared"),
        0,
        "the first failed entry must still salvage //dep:shared's provider instance; otherwise \
         every later failed entry locally re-analyzes it"
    );
}

#[test]
fn successful_entry_harvest_prevents_reanalyzing_shared_starlark_dep() {
    let root = std::env::temp_dir().join(format!("razel-demand-success-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    write_defs(&root, false);
    write_shared_dep(&root);
    let packages = write_consumers(&root, 4);
    let events = Arc::new(Mutex::new(Vec::new()));

    let (report, _) = load_tree_report_with_threads(
        &root,
        flags_with_events(events.clone()),
        &packages,
        Vec::new(),
        1,
    );
    let _ = std::fs::remove_dir_all(&root);

    for (pkg, r) in &report {
        assert!(r.is_ok(), "fixture entries should succeed: {pkg}: {r:?}");
    }
    assert_eq!(
        count(&events, "provider-reanalyze", "//dep:shared"),
        0,
        "a successful first consumer freezes and harvests the shared dep's provider instance"
    );
}

#[test]
fn providerless_starlark_dep_is_known_empty_not_reanalyzed() {
    let root = std::env::temp_dir().join(format!("razel-demand-empty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    write_empty_provider_defs(&root);
    write_empty_shared_dep(&root);
    let packages = write_consumers(&root, 4);
    let events = Arc::new(Mutex::new(Vec::new()));

    let (report, _) = load_tree_report_with_threads(
        &root,
        flags_with_events(events.clone()),
        &packages,
        Vec::new(),
        1,
    );
    let _ = std::fs::remove_dir_all(&root);

    for (pkg, r) in &report {
        assert!(r.is_ok(), "fixture entries should succeed: {pkg}: {r:?}");
    }
    assert_eq!(
        count(&events, "provider-reanalyze", "//dep:shared"),
        0,
        "a Starlark target returning [] must be cached as known-empty providers, not repeatedly \
         re-analyzed by every consumer"
    );
}
