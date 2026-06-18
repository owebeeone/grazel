//! End-to-end CLI tests: drive the built `razel` binary as a real process and
//! assert on its behaviour (and that `--cbor` output is valid taut-wire).

use std::process::Command;

/// A single-package BUILD that compiles one real object via the rule engine.
const BUILD: &str = r#"
def _impl(ctx):
    out = ctx.attr.name + ".o"
    ctx.actions.run(
        executable = "/usr/bin/cc",
        outputs = [out],
        inputs = [ctx.attr.src],
        arguments = ["-c", ctx.attr.src, "-o", out],
    )
    return [DefaultInfo(files = [out])]

cc_obj = rule(implementation = _impl, attrs = {"src": 1})
cc_obj(name = "widget", src = "widget.c")
"#;

fn razel() -> Command {
    Command::new(env!("CARGO_BIN_EXE_razel"))
}

fn unhex(s: &str) -> Vec<u8> {
    let s = s.trim();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn version_prints_text() {
    let out = razel().arg("version").output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("razel "), "got: {s}");
    assert!(s.contains("protocol"), "got: {s}");
}

#[test]
fn version_cbor_decodes_back_through_the_wire() {
    // The CLI's --cbor output must be exactly the bytes a client decodes.
    let out = razel().args(["version", "--cbor"]).output().unwrap();
    assert!(out.status.success());
    let bytes = unhex(&String::from_utf8_lossy(&out.stdout));
    let v = razel_wire::VersionInfo::from_cbor(&razel_wire::decode(&bytes));
    assert_eq!(v.protocol, 1);
    assert!(!v.version.is_empty());
}

#[test]
fn build_compiles_a_real_object_end_to_end() {
    if !std::path::Path::new("/usr/bin/cc").exists() {
        return; // skip where no cc
    }
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("BUILD"), BUILD).unwrap();
    std::fs::write(ws.path().join("widget.c"), "int answer(void){return 42;}").unwrap();

    // Bare name → single-package build of the workspace's BUILD (the rule() dialect
    // doesn't package-qualify paths, so it's single-package; a `//pkg:name` label
    // routes to the multi-package loader — see tests/cli_build.rs).
    let out = razel()
        .args(["build", "widget", "-C"])
        .arg(ws.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stderr); // build summary → stderr (Bazel)
    assert!(s.contains("widget.o"), "stderr: {s}");
    assert!(ws.path().join("widget.o").exists(), "object not produced");
}

/// `razel clean` removes razel's output tree, cache, AND the Bazel-style convenience
/// symlinks (like `bazel clean` wipes `bazel-out` + the `bazel-*` links) — but never a
/// source file. Synthesizes a build's artifacts directly (no toolchain dependency).
#[test]
fn clean_removes_output_tree_cache_and_convenience_symlinks() {
    let ws = tempfile::tempdir().unwrap();
    let p = ws.path();
    std::fs::create_dir_all(p.join("razel-out/cfg/bin")).unwrap();
    std::fs::create_dir_all(p.join(".razel-cache")).unwrap();
    std::fs::write(p.join("BUILD"), "# a real source — must survive clean\n").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("razel-out/cfg/bin", p.join("razel-bin")).unwrap();
        std::os::unix::fs::symlink("razel-out/cfg/testlogs", p.join("razel-testlogs")).unwrap();
    }

    let out = razel().args(["clean", "-C"]).arg(p).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(!p.join("razel-out").exists(), "razel-out not removed");
    assert!(!p.join(".razel-cache").exists(), ".razel-cache not removed");
    assert!(p.join("BUILD").exists(), "clean must NOT touch source files");
    #[cfg(unix)]
    {
        // The convenience symlinks themselves are unlinked (symlink_metadata = not found).
        assert!(std::fs::symlink_metadata(p.join("razel-bin")).is_err(), "razel-bin link kept");
        assert!(std::fs::symlink_metadata(p.join("razel-testlogs")).is_err(), "razel-testlogs kept");
    }
}

/// RG 0011 (2): a BARE-name build routes through the canonical package-file resolver
/// (`BUILD.bazel` over `BUILD`), not a separate ad-hoc probe that predated it.
#[test]
fn bare_build_finds_build_bazel() {
    if !std::path::Path::new("/usr/bin/cc").exists() {
        return; // skip where no cc
    }
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("BUILD.bazel"), BUILD).unwrap();
    std::fs::write(ws.path().join("widget.c"), "int answer(void){return 42;}").unwrap();

    let out = razel()
        .args(["build", "widget", "-C"])
        .arg(ws.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "bare-name build must find BUILD.bazel; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        ws.path().join("widget.o").exists(),
        "object not produced from BUILD.bazel"
    );
}

#[test]
fn build_result_cbor_carries_outputs() {
    if !std::path::Path::new("/usr/bin/cc").exists() {
        return;
    }
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("BUILD"), BUILD).unwrap();
    std::fs::write(ws.path().join("widget.c"), "int answer(void){return 42;}").unwrap();

    let out = razel()
        .args(["build", "widget", "--cbor", "-C"])
        .arg(ws.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let bytes = unhex(&String::from_utf8_lossy(&out.stdout));
    let r = razel_wire::BuildResult::from_cbor(&razel_wire::decode(&bytes));
    assert_eq!(r.status, razel_wire::BuildStatus::Built);
    assert_eq!(r.outputs.len(), 1);
    assert_eq!(r.outputs[0].path, "widget.o");
    assert!(!r.outputs[0].digest.is_empty(), "output digest populated");
}

#[test]
fn second_build_is_cached_with_zero_recomputes() {
    if !std::path::Path::new("/usr/bin/cc").exists() {
        return;
    }
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("BUILD"), BUILD).unwrap();
    std::fs::write(ws.path().join("widget.c"), "int answer(void){return 42;}").unwrap();

    let run = || {
        let out = razel()
            .args(["build", "widget", "--cbor", "-C"])
            .arg(ws.path())
            .output()
            .unwrap();
        assert!(out.status.success());
        razel_wire::BuildResult::from_cbor(&razel_wire::decode(&unhex(&String::from_utf8_lossy(
            &out.stdout,
        ))))
    };

    let first = run();
    assert_eq!(first.status, razel_wire::BuildStatus::Built);
    assert_eq!(first.recomputes, 1, "cold build recomputes the one action");

    let second = run();
    assert_eq!(second.status, razel_wire::BuildStatus::Cached);
    assert_eq!(second.recomputes, 0, "warm rebuild recomputes nothing");
}

const BUILD_WITH_TEST: &str = r#"
def _impl(ctx):
    out = ctx.attr.name + ".o"
    ctx.actions.run(executable = "cc", outputs = [out], inputs = [ctx.attr.src], arguments = [])
    return [DefaultInfo(files = [out])]
thing = rule(implementation = _impl, attrs = {"src": 1})
thing(name = "widget", src = "widget.c")
thing(name = "widget_test", src = "widget.c")
"#;

#[test]
fn affected_query_local_returns_impacted_targets() {
    // Analysis-only — no toolchain needed.
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("BUILD"), BUILD_WITH_TEST).unwrap();

    let out = razel()
        .args(["affected", "widget.c", "--cbor", "-C"])
        .arg(ws.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let impact = razel_wire::ImpactSet::from_cbor(&razel_wire::decode(&unhex(
        &String::from_utf8_lossy(&out.stdout),
    )));
    let labels =
        |v: &[razel_wire::TargetRef]| v.iter().map(|t| t.label.clone()).collect::<Vec<_>>();
    assert_eq!(labels(&impact.targets), vec!["//:widget"]);
    assert_eq!(labels(&impact.tests), vec!["//:widget_test"]);
}

#[test]
fn affected_query_through_a_spawned_daemon() {
    use std::time::{Duration, Instant};
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("BUILD"), BUILD_WITH_TEST).unwrap();
    let socket = format!("/tmp/razel-cli-affected-{}.sock", std::process::id());

    let mut daemon = razel()
        .args(["daemon", "--socket", &socket, "-C"])
        .arg(ws.path())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !std::path::Path::new(&socket).exists() {
        assert!(Instant::now() < deadline, "daemon never bound the socket");
        std::thread::sleep(Duration::from_millis(10));
    }

    let out = razel()
        .args([
            "affected", "widget.c", "--daemon", "--socket", &socket, "-C",
        ])
        .arg(ws.path())
        .output()
        .unwrap();
    daemon.kill().ok();
    daemon.wait().ok();
    let _ = std::fs::remove_file(&socket);

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("//:widget"), "stdout: {s}");
    assert!(s.contains("//:widget_test"), "stdout: {s}");
}

#[test]
fn unknown_target_reports_failed_and_exits_nonzero() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("BUILD"), BUILD).unwrap();
    let out = razel()
        .args(["build", "nope", "-C"])
        .arg(ws.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("ERROR"), // Bazel: "ERROR: … build failed"
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn missing_build_file_errors_cleanly() {
    let ws = tempfile::tempdir().unwrap();
    let out = razel()
        .args(["build", "widget", "-C"])
        .arg(ws.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no BUILD"));
}

#[test]
fn build_through_a_spawned_daemon() {
    use std::time::{Duration, Instant};
    if !std::path::Path::new("/usr/bin/cc").exists() {
        return; // skip where no cc
    }
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("BUILD"), BUILD).unwrap();
    std::fs::write(ws.path().join("widget.c"), "int answer(void){return 42;}").unwrap();
    // Short socket path (macOS sun_path limit) — not inside the long tempdir.
    let socket = format!("/tmp/razel-cli-daemon-{}.sock", std::process::id());

    // Start the daemon as a real child process.
    let mut daemon = razel()
        .args(["daemon", "--socket", &socket, "-C"])
        .arg(ws.path())
        .spawn()
        .unwrap();

    // Wait for it to bind.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !std::path::Path::new(&socket).exists() {
        assert!(Instant::now() < deadline, "daemon never bound the socket");
        std::thread::sleep(Duration::from_millis(10));
    }

    // Build through the daemon.
    let out = razel()
        .args(["build", "//:widget", "--daemon", "--socket", &socket, "-C"])
        .arg(ws.path())
        .output()
        .unwrap();

    daemon.kill().ok();
    daemon.wait().ok();
    let _ = std::fs::remove_file(&socket);

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("widget.o"), // summary → stderr (Bazel)
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(ws.path().join("widget.o").exists());
}

/// RG 0010: a BARE invocation from inside the workspace (no -C; default ".") must
/// behave identically to absolute -C — relative workspaces broke COLD input staging
/// (sandbox actions resolve a relative exec_root from their own dir; a warm hit
/// masked it). Cold cache, current_dir = the workspace, no -C.
#[test]
fn bare_invocation_from_workspace_dir_builds_cold() {
    if !std::path::Path::new("/usr/bin/cc").exists() {
        return;
    }
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("BUILD"), BUILD).unwrap();
    std::fs::write(ws.path().join("widget.c"), "int answer(void){return 42;}").unwrap();
    // Resolve the binary to an ABSOLUTE path before changing cwd — CARGO_BIN_EXE_razel may
    // be a path relative to the initial cwd (it is under bazel's runfiles), and this test
    // runs from `ws`. (Cargo's value is already absolute, so canonicalize is a no-op there.)
    let razel_bin = std::fs::canonicalize(env!("CARGO_BIN_EXE_razel")).expect("razel bin");
    let out = std::process::Command::new(&razel_bin)
        .args(["build", "widget"])
        .current_dir(ws.path())
        .output()
        .expect("razel build");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout: {stdout}\nstderr: {stderr}");
    assert!(ws.path().join("widget.o").exists(), "object produced in the workspace");
}
