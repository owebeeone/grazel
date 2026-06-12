//! The cross-daemon output-base lock (§1b made concrete; GrazelScopes.md).
//!
//! One lock per workspace at `<workspace>/.razel-cache/workspace.lock` — the
//! single-writer rule across ALL daemons (razeld + any number of scope
//! grazelds; output bases are not keyed by distribution or scope). Content is
//! one JSON line `{"pid":N,"daemon":"grazeld","scope":"<name>"}` so the refusal
//! can NAME the holder. A dead holder's lock is reaped, not feared. The contract
//! is proposed to the razel lane (razeld mirrors it when daemon mode lands).

use std::path::{Path, PathBuf};

fn lock_path(root: &Path) -> PathBuf {
    root.join(".razel-cache").join("workspace.lock")
}

fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let rest = text.split(&format!("\"{key}\":")).nth(1)?;
    let rest = rest.trim_start();
    if let Some(stripped) = rest.strip_prefix('"') {
        stripped.split('"').next()
    } else {
        Some(rest.split([',', '}']).next()?.trim())
    }
}

/// Claim the workspace for this daemon. Loud on conflict with a LIVE holder;
/// a dead holder's lock is reaped and retaken.
pub fn acquire(root: &Path, scope: &str) -> Result<(), String> {
    let lock = lock_path(root);
    let dir = lock.parent().expect("lock has a parent");
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for _ in 0..2 {
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&lock) {
            Ok(mut f) => {
                use std::io::Write;
                writeln!(
                    f,
                    "{{\"pid\":{},\"daemon\":\"grazeld\",\"scope\":\"{scope}\"}}",
                    std::process::id()
                )
                .map_err(|e| e.to_string())?;
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let text = std::fs::read_to_string(&lock).unwrap_or_default();
                let holder_pid: Option<u32> = field(&text, "pid").and_then(|p| p.parse().ok());
                if let Some(pid) = holder_pid
                    && crate::dial::pid_alive(pid)
                {
                    return Err(format!(
                        "workspace {} is held by {} scope=\"{}\" (pid {pid})",
                        root.display(),
                        field(&text, "daemon").unwrap_or("?"),
                        field(&text, "scope").unwrap_or("?"),
                    ));
                }
                // Holder is dead (or the file is garbage): reap and retry.
                let _ = std::fs::remove_file(&lock);
            }
            Err(e) => return Err(format!("{}: {e}", lock.display())),
        }
    }
    Err(format!("could not acquire {}", lock.display()))
}

/// Release — only if WE hold it (a pid check, so a confused daemon can never
/// release another writer's claim).
pub fn release(root: &Path) {
    let lock = lock_path(root);
    let ours = std::fs::read_to_string(&lock)
        .ok()
        .and_then(|t| field(&t, "pid").and_then(|p| p.parse::<u32>().ok()))
        .is_some_and(|pid| pid == std::process::id());
    if ours {
        let _ = std::fs::remove_file(&lock);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_release_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        acquire(tmp.path(), "t").unwrap();
        let err = acquire(tmp.path(), "u").unwrap_err(); // we are alive: refused
        assert!(err.contains("held by") && err.contains("\"t\""), "{err}");
        release(tmp.path());
        acquire(tmp.path(), "u").unwrap();
    }

    #[test]
    fn dead_holder_is_reaped() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = lock_path(tmp.path());
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        std::fs::write(&lock, "{\"pid\":999999999,\"daemon\":\"grazeld\",\"scope\":\"ghost\"}\n")
            .unwrap();
        acquire(tmp.path(), "t").unwrap();
    }
}
