//! The §1e dial procedure, client side: resolve scope → connect its socket →
//! taut hello → on wire-version mismatch, graceful-shutdown and relaunch THAT
//! scope's daemon only (Bazel's restart semantics, scope-local); on stale socket
//! (refused + dead pid) clean and autostart. `--no_autostart` for CI/scripting.

use crate::daemon;
use crate::paths::ScopePaths;
use razel_daemon::rpc;
use razel_wire::VersionInfo;
use std::path::Path;
use std::time::Duration;

pub struct DialOutcome {
    pub version: VersionInfo,
    pub pid: u32,
    /// True when the dial replaced a wire-version-mismatched daemon.
    pub restarted: bool,
}

enum HelloErr {
    /// Couldn't reach a daemon at all (no socket / refused / reset).
    Connect(String),
    /// Reached one; it answered with a protocol-level error.
    App(String),
}

fn try_hello(socket: &Path, workspace: &Path) -> Result<VersionInfo, HelloErr> {
    let resp = rpc::call(socket, &daemon::req_hello(workspace))
        .map_err(|e| HelloErr::Connect(e.to_string()))?;
    let payload = rpc::payload(&resp).map_err(HelloErr::App)?;
    Ok(VersionInfo::from_cbor(&payload))
}

/// `"pid":N` out of daemon.json — hand-parsed; the file is grazel's own
/// three-field artifact (no JSON dep by workspace policy).
fn daemon_pid(paths: &ScopePaths) -> Option<u32> {
    let text = std::fs::read_to_string(&paths.daemon_json).ok()?;
    let digits = text.split("\"pid\":").nth(1)?;
    digits[..digits.find(|c: char| !c.is_ascii_digit())?].parse().ok()
}

#[cfg(unix)]
pub(crate) fn pid_alive(pid: u32) -> bool {
    // kill -0: existence probe, no signal delivered; output captured so a dead
    // pid's "No such process" never leaks into verb output.
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn eventually(what: &str, patience: Duration, mut f: impl FnMut() -> bool) -> Result<(), String> {
    let step = Duration::from_millis(50);
    let mut waited = Duration::ZERO;
    while waited < patience {
        if f() {
            return Ok(());
        }
        std::thread::sleep(step);
        waited += step;
    }
    Err(format!("timed out waiting for {what}"))
}

/// Spawn `grazel daemon run` for this scope, detached, guarded by the §1e launch
/// lock (two racing dials → one spawn; the loser waits for the winner's socket).
fn launch(paths: &ScopePaths, workspace: &Path, grazel_bin: &Path) -> Result<(), String> {
    std::fs::create_dir_all(&paths.state_dir).map_err(|e| e.to_string())?;
    let lock = paths.state_dir.join("launch.lock");
    let acquired = match std::fs::OpenOptions::new().write(true).create_new(true).open(&lock) {
        Ok(mut f) => {
            use std::io::Write;
            let _ = writeln!(f, "{}", std::process::id());
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(e) => return Err(format!("{}: {e}", lock.display())),
    };
    if !acquired {
        // Someone else is launching; ride their daemon. A crashed launcher
        // leaves the lock — surfaced as this timeout, not hidden.
        return eventually("competing launch", Duration::from_secs(10), || {
            daemon_pid(paths).is_some_and(pid_alive) && paths.socket.exists()
        })
        .map_err(|e| format!("{e} (stale {}? remove it)", lock.display()));
    }
    let spawned = std::process::Command::new(grazel_bin)
        .args([
            "daemon",
            "run",
            &format!("--scope={}", paths.scope),
            &format!("--workspace={}", workspace.display()),
        ])
        .env("GRAZEL_HOME", &paths.home)
        // The test seam must never survive a relaunch (daemon.rs).
        .env_remove("GRAZEL_FAKE_WIRE_PROTOCOL")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let _ = std::fs::remove_file(&lock);
    spawned.map(|_| ()).map_err(|e| format!("spawn {}: {e}", grazel_bin.display()))
}

fn hello_or_fail(socket: &Path, workspace: &Path, patience: Duration) -> Result<VersionInfo, String> {
    let mut last = String::new();
    let step = Duration::from_millis(50);
    let mut waited = Duration::ZERO;
    while waited < patience {
        match try_hello(socket, workspace) {
            Ok(v) => return Ok(v),
            // An App error means a daemon ANSWERED and refused (e.g. the
            // workspace is held by another scope) — terminal, don't retry it.
            Err(HelloErr::App(e)) => return Err(e),
            Err(HelloErr::Connect(e)) => last = e,
        }
        std::thread::sleep(step);
        waited += step;
    }
    Err(format!("daemon at {} never answered hello: {last}", socket.display()))
}

/// One hello round-trip, no autostart, no recovery — the probe ws-test stages use.
pub fn hello_once(socket: &Path, workspace: &Path) -> Result<VersionInfo, String> {
    try_hello(socket, workspace).map_err(|e| match e {
        HelloErr::Connect(s) | HelloErr::App(s) => s,
    })
}

/// Graceful stop. Ok(false) = nothing was running (idempotent), Ok(true) = a
/// daemon acknowledged shutdown and its socket cleared.
pub fn stop(paths: &ScopePaths) -> Result<bool, String> {
    if !paths.socket.exists() {
        return Ok(false);
    }
    match rpc::call(&paths.socket, &daemon::req_shutdown()) {
        Ok(_) => {
            eventually("shutdown to clear the socket", Duration::from_secs(5), || {
                !paths.socket.exists()
            })?;
            Ok(true)
        }
        Err(_) => {
            // Socket present but nobody home — the stale case; not stop's job.
            Ok(false)
        }
    }
}

/// The full dial: hello → mismatch ⇒ scope-local restart → stale ⇒ clean +
/// autostart → cold ⇒ autostart. `autostart=false` never spawns or stops.
pub fn ensure(
    paths: &ScopePaths,
    workspace: &Path,
    autostart: bool,
    grazel_bin: &Path,
) -> Result<DialOutcome, String> {
    let outcome = |version: VersionInfo, restarted: bool| -> Result<DialOutcome, String> {
        let pid = daemon_pid(paths)
            .ok_or_else(|| format!("daemon answered but {} has no pid", paths.daemon_json.display()))?;
        Ok(DialOutcome { version, pid, restarted })
    };
    match try_hello(&paths.socket, workspace) {
        Ok(v) if v.protocol == rpc::PROTOCOL => outcome(v, false),
        Ok(v) => {
            if !autostart {
                return Err(format!(
                    "scope {:?} daemon speaks wire protocol {}, client speaks {} (--no_autostart: not restarting)",
                    paths.scope, v.protocol, rpc::PROTOCOL
                ));
            }
            stop(paths)?;
            launch(paths, workspace, grazel_bin)?;
            let v = hello_or_fail(&paths.socket, workspace, Duration::from_secs(10))?;
            if v.protocol != rpc::PROTOCOL {
                return Err(format!("relaunched daemon STILL speaks {}", v.protocol));
            }
            outcome(v, true)
        }
        Err(HelloErr::App(e)) => Err(format!("scope {:?} hello rejected: {e}", paths.scope)),
        Err(HelloErr::Connect(e)) => {
            if let Some(pid) = daemon_pid(paths)
                && pid_alive(pid)
                && paths.socket.exists()
            {
                return Err(format!(
                    "scope {:?} daemon (pid {pid}) is alive but its socket refuses: {e}",
                    paths.scope
                ));
            }
            if !autostart {
                return Err(format!("no daemon for scope {:?} (--no_autostart)", paths.scope));
            }
            let _ = std::fs::remove_file(&paths.socket); // dead pid ⇒ stale socket
            launch(paths, workspace, grazel_bin)?;
            let v = hello_or_fail(&paths.socket, workspace, Duration::from_secs(10))?;
            outcome(v, false)
        }
    }
}
