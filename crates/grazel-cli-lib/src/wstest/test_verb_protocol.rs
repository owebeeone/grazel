//! `grazel ws test` — the workstream's acceptance instrument (GrazelWorkstream §1).
//!
//! Stages exercise the REAL binary end-to-end (CLI → scope resolution → socket →
//! daemon → wire → back); a stage that could pass against a mock is wrongly
//! written. The same ladder runs as the shipped verb and in-process under
//! `cargo test` (grazel-cli's integration test) — only `StageCtx.grazel_bin`
//! differs (`current_exe` vs `CARGO_BIN_EXE_grazel`).

use std::process::Command;


use super::*;

/// `grazel test` = razel's test verb verbatim (GrazelVerbs.md): bazel's exit
/// protocol (0 / 3), PASSED/FAILED lines, test.log under testlogs — and zero
/// daemon involvement (the §1b shared cache makes routing moot).
pub(crate) fn test_verb_protocol(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(ws.join("t")).map_err(|e| e.to_string())?;
    std::fs::write(
        ws.join("t/BUILD.bazel"),
        "load(\"@aspect_rules_js//js:defs.bzl\", \"js_test\")\n\
         js_test(name = \"ok\", entry_point = \"ok.js\")\n\
         js_test(name = \"bad\", entry_point = \"bad.js\")\n",
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(ws.join("t/ok.js"), "console.log('fine'); process.exit(0);\n")
        .map_err(|e| e.to_string())?;
    std::fs::write(ws.join("t/bad.js"), "console.error('kaboom'); process.exit(1);\n")
        .map_err(|e| e.to_string())?;
    let ws_abs = ws.canonicalize().map_err(|e| e.to_string())?.display().to_string();
    let grazel_test = |target: &str| -> Result<(Option<i32>, String), String> {
        let out = Command::new(&ctx.grazel_bin)
            .current_dir(&ws)
            .env("GRAZEL_HOME", &home)
            .env_remove("GRAZEL_SCOPE")
            .args(["test", target, "-C", &ws_abs])
            .output()
            .map_err(|e| e.to_string())?;
        // razel's test summary (PASSED/FAILED) is on STDERR (Bazel-style); capture both.
        Ok((
            out.status.code(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        ))
    };
    let (code, stdout) = grazel_test("//t:ok")?;
    if code != Some(0) || !stdout.contains("PASSED") {
        return Err(format!("passing test: exit {code:?}, output: {stdout}"));
    }
    let log = ws.join("razel-testlogs/t/ok/test.log"); // Bazel testlogs layout (via the symlink)
    if !std::fs::read_to_string(&log).map_err(|e| e.to_string())?.contains("fine") {
        return Err("test.log missing the test's stdout".into());
    }
    let (code, stdout) = grazel_test("//t:bad")?;
    if code != Some(3) || !stdout.contains("FAILED") {
        return Err(format!("failing test: exit {code:?} (want bazel's 3), stdout: {stdout}"));
    }
    if home.join(".uds").exists() {
        return Err("test verb started a daemon".into());
    }
    Ok(())
}

