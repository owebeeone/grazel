//! `grazel ws test` — the workstream's acceptance instrument (GrazelWorkstream §1).
//!
//! Stages exercise the REAL binary end-to-end (CLI → scope resolution → socket →
//! daemon → wire → back); a stage that could pass against a mock is wrongly
//! written. The same ladder runs as the shipped verb and in-process under
//! `cargo test` (grazel-cli's integration test) — only `StageCtx.grazel_bin`
//! differs (`current_exe` vs `CARGO_BIN_EXE_grazel`).

use crate::paths::ScopePaths;
use std::path::Path;



/// Member files under `<state>/members/` — contents are `<kind> <root>` lines.
pub(crate) fn members_of(home: &Path, scope: &str) -> Result<Vec<String>, String> {
    let dir = ScopePaths::new(home, scope)?.state_dir.join("members");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(vec![]);
    };
    Ok(entries
        .filter_map(|e| std::fs::read_to_string(e.ok()?.path()).ok())
        .map(|s| s.trim().to_string())
        .collect())
}

