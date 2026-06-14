//! S5 (V3sh1): `razel test` — build → exec → bazel's test protocol: exit 0 all-pass /
//! exit 3 tests-failed (bazel's code), `test.log` written, PASSED/FAILED summary line
//! per target. E2E through the real bin. Test-first.

use std::process::Command;

fn write(path: &std::path::Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn ws(tag: &str) -> std::path::PathBuf {
    let w = std::env::temp_dir().join(format!("razel-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&w);
    w
}

#[test]
fn passing_sh_test_reports_passed_and_exits_zero() {
    let w = ws("pass");
    write(
        &w.join("t/BUILD"),
        "load(\"@rules_shell//shell:sh_test.bzl\", \"sh_test\")\n\
         sh_test(name = \"ok\", srcs = [\"ok.sh\"])\n",
    );
    write(&w.join("t/ok.sh"), "#!/bin/sh\necho all good\nexit 0\n");
    let out = Command::new(env!("CARGO_BIN_EXE_razel"))
        .args(["test", "//t:ok", "-C"])
        .arg(&w)
        .output()
        .expect("razel test");
    let stdout = String::from_utf8_lossy(&out.stderr); // razel's test summary → stderr (Bazel)
    assert_eq!(out.status.code(), Some(0), "stdout: {stdout}");
    assert!(stdout.contains("//t:ok") && stdout.contains("PASSED"), "{stdout}");
    // test.log captured (bazel's testlogs shape under the razel cache dir).
    let log = w.join(".razel-cache/testlogs/t/ok/test.log");
    assert!(log.is_file(), "test.log at {}", log.display());
    assert!(std::fs::read_to_string(&log).unwrap().contains("all good"));
    let _ = std::fs::remove_dir_all(&w);
}

#[test]
fn failing_test_reports_failed_and_exits_three() {
    let w = ws("fail");
    write(
        &w.join("t/BUILD"),
        "load(\"@rules_shell//shell:sh_test.bzl\", \"sh_test\")\n\
         sh_test(name = \"bad\", srcs = [\"bad.sh\"])\n",
    );
    write(&w.join("t/bad.sh"), "#!/bin/sh\necho boom >&2\nexit 1\n");
    let out = Command::new(env!("CARGO_BIN_EXE_razel"))
        .args(["test", "//t:bad", "-C"])
        .arg(&w)
        .output()
        .expect("razel test");
    let stdout = String::from_utf8_lossy(&out.stderr); // razel's test summary → stderr (Bazel)
    // Bazel's contract: exit 3 = build succeeded, tests failed.
    assert_eq!(out.status.code(), Some(3), "stdout: {stdout}");
    assert!(stdout.contains("//t:bad") && stdout.contains("FAILED"), "{stdout}");
    let log = w.join(".razel-cache/testlogs/t/bad/test.log");
    assert!(std::fs::read_to_string(&log).unwrap().contains("boom"), "stderr captured");
    let _ = std::fs::remove_dir_all(&w);
}

#[test]
fn js_test_rides_the_aspect_surface() {
    let w = ws("js");
    write(
        &w.join("t/BUILD.razel"),
        "load(\"@aspect_rules_js//js:defs.bzl\", \"js_test\")\n\
         js_test(name = \"jt\", entry_point = \"t.js\")\n",
    );
    write(&w.join("t/t.js"), "if (1 + 1 !== 2) process.exit(1); console.log('math holds');\n");
    let out = Command::new(env!("CARGO_BIN_EXE_razel"))
        .args(["test", "//t:jt", "-C"])
        .arg(&w)
        .output()
        .expect("razel test");
    let stdout = String::from_utf8_lossy(&out.stderr); // razel's test summary → stderr (Bazel)
    assert_eq!(out.status.code(), Some(0), "stdout: {stdout}\nstderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("PASSED"), "{stdout}");
    let _ = std::fs::remove_dir_all(&w);
}

/// A genuine ANALYSIS/build failure exits 1, never 3. (Note: a js_test with a
/// missing entry_point is NOT a build failure under launcher-from-source — the
/// launcher builds, node fails at runtime, exit 3 — so the fixture here uses an
/// analysis error: sh_test with no srcs.)
#[test]
fn build_failure_is_not_a_test_failure_exit() {
    let w = ws("bld");
    write(
        &w.join("t/BUILD"),
        "load(\"@rules_shell//shell:sh_test.bzl\", \"sh_test\")\n\
         sh_test(name = \"jt\")\n",
    );
    let out = Command::new(env!("CARGO_BIN_EXE_razel"))
        .args(["test", "//t:jt", "-C"])
        .arg(&w)
        .output()
        .expect("razel test");
    assert_eq!(out.status.code(), Some(1), "build failure = exit 1, not 3");
    let _ = std::fs::remove_dir_all(&w);
}

/// `razel test <a> <b> -j N` (Bazel multi-target): both run (one passes, one fails), the
/// summary aggregates, and a failing test exits 3 (build OK). Exercises the -j test pool.
#[test]
fn multi_target_test_with_jobs_aggregates_and_exits_three() {
    let w = ws("multi");
    write(
        &w.join("t/BUILD"),
        "load(\"@rules_shell//shell:sh_test.bzl\", \"sh_test\")\n\
         sh_test(name = \"ok\", srcs = [\"ok.sh\"])\n\
         sh_test(name = \"bad\", srcs = [\"bad.sh\"])\n",
    );
    write(&w.join("t/ok.sh"), "#!/bin/sh\necho good\nexit 0\n");
    write(&w.join("t/bad.sh"), "#!/bin/sh\necho boom\nexit 1\n");
    let out = Command::new(env!("CARGO_BIN_EXE_razel"))
        .args(["test", "//t:ok", "//t:bad", "-j", "2", "-C"])
        .arg(&w)
        .output()
        .expect("razel test");
    let stdout = String::from_utf8_lossy(&out.stderr); // razel's test summary → stderr (Bazel)
    assert_eq!(out.status.code(), Some(3), "one test failed → exit 3; stdout: {stdout}");
    assert!(stdout.contains("//t:ok") && stdout.contains("PASSED"), "{stdout}");
    assert!(stdout.contains("//t:bad") && stdout.contains("FAILED"), "{stdout}");
    assert!(
        stdout.contains("Executed 2 out of 2 tests: 1 passing, 1 failing"),
        "summary: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&w);
}
