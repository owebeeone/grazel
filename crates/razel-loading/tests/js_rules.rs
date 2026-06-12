//! S3a (V3sh1): the razel-native js rulepack — `js_binary` (node entry + node_modules
//! dep) and `ts_project`-lite (one tsc action). Loaded from `@razel_js//js:defs.bzl`
//! in razel-native (E-mode) packages — gryth's grammar. Test-first.

use razel_loading::{GlobalFlags, analyze_workspace_with};

fn write(path: &std::path::Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn fixture(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("razel-js-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

/// `js_binary` in a BUILD.razel package: one action emitting the launcher (which
/// execs the SOURCE entry — node's walk-up finds the workspace node_modules);
/// DefaultInfo = the launcher.
#[test]
fn js_binary_analyzes_with_launcher() {
    let ws = fixture("bin");
    write(
        &ws.join("srv/BUILD.razel"),
        "load(\"@aspect_rules_js//js:defs.bzl\", \"js_binary\")\n\
         js_binary(name = \"hello\", entry_point = \"server.js\")\n",
    );
    write(&ws.join("srv/server.js"), "console.log('hi');\n");
    let targets = analyze_workspace_with(&ws, "//srv:hello", GlobalFlags::default()).unwrap();
    let t = targets.iter().find(|t| t.name == "//srv:hello").expect("target");
    assert_eq!(t.default_info, vec!["srv/hello".to_string()], "launcher is the output");
    let a = &t.actions[0];
    assert_eq!(a.mnemonic, "JsBinary");
    assert!(a.inputs.contains(&"srv/server.js".to_string()));
    assert!(a.outputs.contains(&"srv/hello".to_string()));
    let cmd = a.argv.join(" ");
    assert!(cmd.contains("server.js"), "launcher runs the entry: {cmd}");
    assert!(cmd.contains("dirname"), "entry resolved relative to the launcher: {cmd}");
    let _ = std::fs::remove_dir_all(&ws);
}

/// Only `entry_point` is required (aspect surface; deps ride node walk-up).
#[test]
fn js_binary_minimal() {
    let ws = fixture("nodeps");
    write(
        &ws.join("a/BUILD.razel"),
        "load(\"@aspect_rules_js//js:defs.bzl\", \"js_binary\")\n\
         js_binary(name = \"x\", entry_point = \"x.js\")\n",
    );
    write(&ws.join("a/x.js"), "");
    let targets = analyze_workspace_with(&ws, "//a:x", GlobalFlags::default()).unwrap();
    assert!(targets.iter().any(|t| t.name == "//a:x"));
    let _ = std::fs::remove_dir_all(&ws);
}

/// `ts_project`-lite: ONE tsc action; outputs = srcs with .ts → .js under the
/// project's out dir; DefaultInfo lists the .js files.
#[test]
fn ts_project_lite_single_tsc_action() {
    let ws = fixture("ts");
    write(
        &ws.join("lib/BUILD.razel"),
        "load(\"@aspect_rules_ts//ts:defs.bzl\", \"ts_project\")\n\
         ts_project(name = \"core\", srcs = [\"a.ts\", \"b.ts\"])\n",
    );
    write(&ws.join("lib/a.ts"), "export const A = 1;\n");
    write(&ws.join("lib/b.ts"), "export const B = 2;\n");
    let targets = analyze_workspace_with(&ws, "//lib:core", GlobalFlags::default()).unwrap();
    let t = targets.iter().find(|t| t.name == "//lib:core").expect("target");
    assert_eq!(t.actions.len(), 1, "ONE tsc action — ts_project-lite");
    assert_eq!(t.actions[0].mnemonic, "TsProject");
    assert!(t.actions[0].inputs.contains(&"lib/a.ts".to_string()));
    assert!(
        t.default_info.iter().any(|o| o.ends_with("a.js"))
            && t.default_info.iter().any(|o| o.ends_with("b.js")),
        "compiled outputs declared: {:?}",
        t.default_info
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// The rulepack is razel-native: loading it from a plain BUILD works too (design C
/// will shim it bazel-side later), and entry must be a real attr error when missing.
#[test]
fn js_binary_requires_entry() {
    let ws = fixture("noentry");
    write(
        &ws.join("a/BUILD.razel"),
        "load(\"@aspect_rules_js//js:defs.bzl\", \"js_binary\")\n\
         js_binary(name = \"x\")\n",
    );
    let err = analyze_workspace_with(&ws, "//a:x", GlobalFlags::default())
        .expect_err("entry is required");
    assert!(err.contains("entry_point"), "{err}");
    let _ = std::fs::remove_dir_all(&ws);
}

/// `js_library` (aspect surface): a grouping rule — no actions, DefaultInfo = srcs.
/// Gates the `frontend` example (21 packages load through it).
#[test]
fn js_library_groups_srcs() {
    let ws = fixture("jslib");
    write(
        &ws.join("lib/BUILD.razel"),
        "load(\"@aspect_rules_js//js:defs.bzl\", \"js_library\")\n\
         js_library(name = \"l\", srcs = [\"a.js\", \"b.js\"])\n",
    );
    write(&ws.join("lib/a.js"), "");
    write(&ws.join("lib/b.js"), "");
    let targets = analyze_workspace_with(&ws, "//lib:l", GlobalFlags::default()).unwrap();
    let t = targets.iter().find(|t| t.name == "//lib:l").expect("target");
    assert!(t.actions.is_empty(), "grouping rule: no actions");
    assert_eq!(t.default_info, vec!["lib/a.js".to_string(), "lib/b.js".to_string()]);
    let _ = std::fs::remove_dir_all(&ws);
}
