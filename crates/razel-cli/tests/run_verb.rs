//! S3b (V3sh1): `razel run` — build the target, exec its runnable output, propagate
//! args and exit status. E2E through the REAL bin (env!(CARGO_BIN_EXE_razel)): bin →
//! lib → engine → host node (the S3 non-hermetic posture). Test-first.

use std::process::Command;

fn write(path: &std::path::Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[test]
fn run_builds_and_executes_a_js_binary() {
    let ws = std::env::temp_dir().join(format!("razel-run-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    write(
        &ws.join("srv/BUILD.bazel"),
        "load(\"@aspect_rules_js//js:defs.bzl\", \"js_binary\")\n\
         js_binary(name = \"hello\", entry_point = \"server.js\")\n",
    );
    write(
        &ws.join("srv/server.js"),
        "console.log('hello from gryth', process.argv.slice(2).join(','));\n",
    );
    let out = Command::new(env!("CARGO_BIN_EXE_razel"))
        .args(["run", "//srv:hello", "-C"])
        .arg(&ws)
        .args(["--", "a", "b"])
        .output()
        .expect("razel run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "exit ok\nstdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("hello from gryth a,b"),
        "program ran with args\nstdout: {stdout}\nstderr: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn run_propagates_the_program_exit_code() {
    let ws = std::env::temp_dir().join(format!("razel-run-exit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    write(
        &ws.join("a/BUILD.bazel"),
        "load(\"@aspect_rules_js//js:defs.bzl\", \"js_binary\")\n\
         js_binary(name = \"x\", entry_point = \"x.js\")\n",
    );
    write(&ws.join("a/x.js"), "process.exit(3);\n");
    let out = Command::new(env!("CARGO_BIN_EXE_razel"))
        .args(["run", "//a:x", "-C"])
        .arg(&ws)
        .output()
        .expect("razel run");
    assert_eq!(out.status.code(), Some(3), "program exit code propagates");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn run_fails_loud_when_the_build_fails() {
    let ws = std::env::temp_dir().join(format!("razel-run-fail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    write(
        &ws.join("a/BUILD.bazel"),
        "load(\"@aspect_rules_js//js:defs.bzl\", \"js_binary\")\n\
         js_binary(name = \"x\", entry_point = \"missing.js\")\n",
    );
    let out = Command::new(env!("CARGO_BIN_EXE_razel"))
        .args(["run", "//a:x", "-C"])
        .arg(&ws)
        .output()
        .expect("razel run");
    assert!(!out.status.success(), "build failure must not exec");
    let _ = std::fs::remove_dir_all(&ws);
}
