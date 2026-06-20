//! `grazel ws test` — the workstream's acceptance instrument (GrazelWorkstream §1).
//!
//! Stages exercise the REAL binary end-to-end (CLI → scope resolution → socket →
//! daemon → wire → back); a stage that could pass against a mock is wrongly
//! written. The same ladder runs as the shipped verb and in-process under
//! `cargo test` (grazel-cli's integration test) — only `StageCtx.grazel_bin`
//! differs (`current_exe` vs `CARGO_BIN_EXE_grazel`).

use std::process::Command;


use super::*;

/// Any non-loopback bind is refused at startup — localhost-only by construction
/// until the iroh-era auth story exists.
pub(crate) fn http_localhost_only(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let out = Command::new(&ctx.grazel_bin)
        .current_dir(&ws)
        .env("GRAZEL_HOME", &home)
        .args(["daemon", "run", "--scope=open", "--http-bind=0.0.0.0:0"])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        stop_scope(ctx, &home, &ws, "open");
        return Err("non-local --http-bind was accepted".into());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stderr.contains("localhost") && !stderr.contains("127.0.0.1") {
        return Err(format!("refusal doesn't explain localhost-only: {stderr}"));
    }
    Ok(())
}

