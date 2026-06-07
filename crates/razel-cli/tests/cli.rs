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

    let out = razel()
        .args(["build", "//x:widget", "-C"])
        .arg(ws.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("widget.o"), "stdout: {s}");
    assert!(ws.path().join("widget.o").exists(), "object not produced");
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
        String::from_utf8_lossy(&out.stderr).contains("FAILED"),
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
