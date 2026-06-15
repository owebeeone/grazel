//! Shared default-deny env assembly for both wrapper subcommands (P3.8 `build-script`, P3.9
//! `rustc`). Lowest→highest precedence: the platform `baseline`, then `--env-file`s (later files
//! win, §6.2), then explicit `--env` entries. Each subcommand layers its own specifics on top
//! (build-script adds `OUT_DIR`; rustc adds the flags-file `rustc-env` records).

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

/// On Windows a fully `env_clear()`ed child often cannot even start (no `SystemRoot` → no DLL
/// load), so the wrapper seeds these system vars from its own env. Unix needs no baseline — it is
/// truly default-deny. The Cargo/rustc env on top is supplied by the caller (`--env`/`--env-file`).
#[cfg(windows)]
pub const WINDOWS_ENV_BASELINE: &[&str] = &[
    "SystemRoot",
    "ComSpec",
    "PATHEXT",
    "windir",
    "TEMP",
    "TMP",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "USERPROFILE",
];

#[cfg(windows)]
pub fn platform_baseline() -> BTreeMap<String, String> {
    WINDOWS_ENV_BASELINE
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect()
}
#[cfg(not(windows))]
pub fn platform_baseline() -> BTreeMap<String, String> {
    BTreeMap::new()
}

/// `baseline` < `env_files` (later files win, §6.2) < explicit `env`. Each subcommand layers its
/// own authoritative entries (e.g. `OUT_DIR`) on the result.
pub fn base_env(
    baseline: &BTreeMap<String, String>,
    env_files: &[PathBuf],
    env: &[(String, String)],
) -> io::Result<BTreeMap<String, String>> {
    let mut out = baseline.clone();
    for f in env_files {
        for line in std::fs::read_to_string(f)?.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                out.insert(k.trim().to_string(), v.to_string());
            }
        }
    }
    for (k, v) in env {
        out.insert(k.clone(), v.clone());
    }
    Ok(out)
}
