//! The §1b CROSS-DAEMON workspace writer lock — contract agreed with the grazel lane
//! (ws-razel/inbox/0005, RG's GR2 shape, accepted verbatim; THIS is the one
//! implementation — grazeld consumes it through the razel-daemon lib, the razel→grazel
//! arrow never reverses).
//!
//! One writer per workspace across ALL daemons (razeld, any scope grazeld, and
//! `razel`'s local in-process builds):
//! - **File:** `<workspace>/.razel-cache/workspace.lock` (the per-workspace dir razel
//!   already owns; when output bases move out-of-tree the lock moves WITH them —
//!   same contract, new home, both lanes switch together).
//! - **Acquire:** `create_new`; content is ONE JSON line
//!   `{"pid":N,"daemon":"razeld"|"grazeld"|"razel-local","scope":"<name>"}` (scope
//!   empty for the razel side). Holder alive → fail LOUD naming daemon+scope+pid;
//!   holder dead → reap and retake.
//! - **Release:** the holder removes the file (pid-checked); RAII via [`OutLock`]'s
//!   `Drop` so every exit path releases.

use std::io::Write;
use std::path::{Path, PathBuf};

/// A held workspace writer lock; releases on drop (pid-checked).
#[derive(Debug)]
pub struct OutLock {
    path: PathBuf,
}

/// Acquire the workspace writer lock, or fail loud naming the holder.
pub fn acquire(workspace: &Path, daemon: &str, scope: &str) -> Result<OutLock, String> {
    let dir = workspace.join(".razel-cache");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join("workspace.lock");
    for attempt in 0..2 {
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                let line = format!(
                    "{{\"pid\":{},\"daemon\":\"{daemon}\",\"scope\":\"{scope}\"}}\n",
                    std::process::id()
                );
                f.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
                return Ok(OutLock { path });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let held = std::fs::read_to_string(&path).unwrap_or_default();
                let holder_pid = field_u32(&held, "pid");
                if let Some(pid) = holder_pid {
                    if pid_alive(pid) {
                        let holder = field_str(&held, "daemon").unwrap_or_else(|| "?".into());
                        let scope = field_str(&held, "scope").filter(|s| !s.is_empty());
                        // grazeld holders get the remedy named (RG's shutdown verb).
                        let hint = if holder == "grazeld" {
                            format!(
                                "; or: grazel shutdown --scope={}",
                                scope.as_deref().unwrap_or("default")
                            )
                        } else {
                            String::new()
                        };
                        return Err(format!(
                            "workspace {} is held by {holder} (pid {pid}{}) — one writer \
                             per workspace across daemons (§1b); stop it or use that \
                             daemon{hint}",
                            workspace.display(),
                            scope.map(|s| format!(", scope {s}")).unwrap_or_default(),
                        ));
                    }
                }
                // Dead (or unreadable) holder: reap and retake once.
                if attempt == 0 {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                return Err(format!("cannot reap stale lock {}", path.display()));
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    unreachable!("two attempts covered above")
}

impl Drop for OutLock {
    fn drop(&mut self) {
        // pid-checked release: only remove OUR lock file.
        if let Ok(held) = std::fs::read_to_string(&self.path)
            && field_u32(&held, "pid") == Some(std::process::id())
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Release this workspace's writer lock IF we hold it (pid-checked) — the explicit counterpart to
/// `OutLock`'s Drop, for a path that can't run Drop: `do_shutdown` ends in `process::exit`, which
/// skips destructors, so without this the lock file would linger and block the next daemon. Portable
/// (just a pid-checked file unlink).
pub fn release_if_ours(workspace: &Path) {
    let path = workspace.join(".razel-cache").join("workspace.lock");
    if let Ok(held) = std::fs::read_to_string(&path)
        && field_u32(&held, "pid") == Some(std::process::id())
    {
        let _ = std::fs::remove_file(&path);
    }
}

fn field_u32(json_line: &str, key: &str) -> Option<u32> {
    let pat = format!("\"{key}\":");
    let rest = &json_line[json_line.find(&pat)? + pat.len()..];
    rest.trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

fn field_str(json_line: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let start = json_line.find(&pat)? + pat.len();
    let rest = &json_line[start..];
    Some(rest[..rest.find('"')?].to_string())
}

/// signal 0: existence probe, no signal delivered.
pub fn pid_alive(pid: u32) -> bool {
    unsafe { libc_kill(pid as i32, 0) == 0 }
}

/// The pid recorded in this workspace's writer lock (`.razel-cache/workspace.lock`), if a LIVE
/// holder exists. The daemon holds this lock for its whole life (rpc::serve), so the pid is the
/// running daemon's — the way to stop a daemon too old to answer the `shutdown` RPC. `None` if no
/// lock, an unreadable/dead holder, or no pid field.
pub fn holder_pid(workspace: &Path) -> Option<u32> {
    let path = workspace.join(".razel-cache").join("workspace.lock");
    let held = std::fs::read_to_string(&path).ok()?;
    let pid = field_u32(&held, "pid")?;
    pid_alive(pid).then_some(pid)
}

/// Stop a process: SIGTERM, then SIGKILL if it outlives a short grace. Returns true once it's gone.
/// The fallback for stopping a stale/unresponsive daemon by pid.
pub fn terminate(pid: u32) -> bool {
    const SIGTERM: i32 = 15;
    const SIGKILL: i32 = 9;
    unsafe { libc_kill(pid as i32, SIGTERM) };
    for _ in 0..40 {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    unsafe { libc_kill(pid as i32, SIGKILL) };
    for _ in 0..40 {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    !pid_alive(pid)
}

unsafe extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("razel-outlock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn acquire_release_reacquire() {
        let w = ws("rr");
        let l = acquire(&w, "razeld", "").unwrap();
        drop(l);
        let _l2 = acquire(&w, "razel-local", "").unwrap();
    }

    #[test]
    fn live_holder_fails_loud_naming_daemon_scope_pid() {
        let w = ws("held");
        let _l = acquire(&w, "grazeld", "customerA").unwrap();
        let e = acquire(&w, "razeld", "").expect_err("second writer must fail");
        assert!(e.contains("grazeld") && e.contains("customerA"), "{e}");
        assert!(e.contains(&std::process::id().to_string()), "{e}");
        // The remedy is named in the error (RG's shutdown verb, inbox 0007).
        assert!(e.contains("grazel shutdown --scope=customerA"), "{e}");
    }

    #[test]
    fn dead_holder_is_reaped() {
        let w = ws("dead");
        let dir = w.join(".razel-cache");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("workspace.lock"),
            "{\"pid\":999999999,\"daemon\":\"grazeld\",\"scope\":\"x\"}\n",
        )
        .unwrap();
        let _l = acquire(&w, "razeld", "").expect("dead holder reaped");
    }
}
