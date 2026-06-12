//! `grazel ws test` — the workstream's acceptance instrument (GrazelWorkstream §1).
//!
//! Stages exercise the REAL binary end-to-end (CLI → scope resolution → socket →
//! daemon → wire → back); a stage that could pass against a mock is wrongly
//! written. The same ladder runs as the shipped verb and in-process under
//! `cargo test` (grazel-cli's integration test) — only `StageCtx.grazel_bin`
//! differs (`current_exe` vs `CARGO_BIN_EXE_grazel`).

use crate::paths::ScopePaths;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct StageCtx {
    /// The real `grazel` binary under test.
    pub grazel_bin: PathBuf,
    /// Per-stage scratch root; stages create what they need under it.
    pub tmp: PathBuf,
}

pub struct Stage {
    pub name: &'static str,
    pub run: fn(&StageCtx) -> Result<(), String>,
}

/// The ordered ladder. GR steps append here; names are the `--stage=` ids.
pub fn stages() -> Vec<Stage> {
    vec![
        Stage { name: "harness-selftest", run: harness_selftest },
        Stage { name: "scope-chain", run: scope_chain },
        Stage { name: "uds-namespace", run: uds_namespace },
        Stage { name: "two-scopes-hello", run: two_scopes_hello },
        Stage { name: "cold-autostart", run: cold_autostart },
        Stage { name: "warm-dial", run: warm_dial },
        Stage { name: "version-handshake", run: version_handshake },
        Stage { name: "stale-socket-recovery", run: stale_socket_recovery },
        Stage { name: "graceful-shutdown", run: graceful_shutdown },
        Stage { name: "idle-out", run: idle_out },
    ]
}

/// Run the ladder (or one `--stage=`), one line per stage. False on any failure;
/// an unknown stage name is a failure, not a no-op.
pub fn run_stages(ctx: &StageCtx, only: Option<&str>, out: &mut dyn Write) -> bool {
    let ladder = stages();
    if let Some(name) = only
        && !ladder.iter().any(|s| s.name == name)
    {
        let _ = writeln!(out, "no such stage {name:?}");
        return false;
    }
    let mut all_ok = true;
    for (idx, stage) in ladder.into_iter().enumerate() {
        if only.is_some_and(|o| o != stage.name) {
            continue;
        }
        // Numeric scratch dirs: socket paths live under here and macOS caps a
        // UDS path at 104 bytes — stage names are too fat for the budget.
        let dir = ctx.tmp.join(idx.to_string());
        if let Err(e) = std::fs::create_dir_all(&dir) {
            let _ = writeln!(out, "{:<24} FAIL  ({e})", stage.name);
            all_ok = false;
            continue;
        }
        let sub = StageCtx { grazel_bin: ctx.grazel_bin.clone(), tmp: dir };
        let t0 = Instant::now();
        match (stage.run)(&sub) {
            Ok(()) => {
                let _ = writeln!(out, "{:<24} PASS  ({:?})", stage.name, t0.elapsed());
            }
            Err(e) => {
                let _ = writeln!(out, "{:<24} FAIL  ({:?})\n  {e}", stage.name, t0.elapsed());
                all_ok = false;
            }
        }
    }
    all_ok
}

// --- the stages --------------------------------------------------------------

fn harness_selftest(ctx: &StageCtx) -> Result<(), String> {
    if !ctx.grazel_bin.is_file() {
        return Err(format!("grazel binary missing: {}", ctx.grazel_bin.display()));
    }
    std::fs::write(ctx.tmp.join("probe"), b"ok").map_err(|e| format!("tmp not writable: {e}"))
}

/// Run `grazel scope` in `dir` with a controlled env; parse its key=value output.
fn scope_cmd(
    ctx: &StageCtx,
    dir: &Path,
    home: &Path,
    flag: Option<&str>,
    env_scope: Option<&str>,
) -> Result<HashMap<String, String>, String> {
    let mut cmd = Command::new(&ctx.grazel_bin);
    cmd.current_dir(dir)
        .env_remove("GRAZEL_SCOPE")
        .env("GRAZEL_HOME", home)
        .arg("scope");
    if let Some(f) = flag {
        cmd.arg(format!("--scope={f}"));
    }
    if let Some(e) = env_scope {
        cmd.env("GRAZEL_SCOPE", e);
    }
    let out = cmd.output().map_err(|e| format!("spawn {}: {e}", ctx.grazel_bin.display()))?;
    if !out.status.success() {
        return Err(format!(
            "`grazel scope` failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect())
}

/// The §1e override chain, through the real CLI: flag > env > rc > default.
fn scope_chain(ctx: &StageCtx) -> Result<(), String> {
    let ws = ctx.tmp.join("ws");
    let home = ctx.tmp.join("home");
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let expect = |got: &HashMap<String, String>, want: &str, case: &str| {
        (got.get("scope").map(String::as_str) == Some(want))
            .then_some(())
            .ok_or(format!("{case}: want scope={want}, got {got:?}"))
    };
    expect(&scope_cmd(ctx, &ws, &home, None, None)?, "default", "bare")?;
    std::fs::write(ws.join(".grazelrc"), "service_scope=rcscope\n").map_err(|e| e.to_string())?;
    expect(&scope_cmd(ctx, &ws, &home, None, None)?, "rcscope", "rc")?;
    expect(&scope_cmd(ctx, &ws, &home, None, Some("envscope"))?, "envscope", "env>rc")?;
    expect(
        &scope_cmd(ctx, &ws, &home, Some("flagscope"), Some("envscope"))?,
        "flagscope",
        "flag>env",
    )
}

/// Scope → socket mapping: distinct per scope, inside grazel's own namespace,
/// nowhere near razel's `_razel_<user>/daemon/` rendezvous. Bad names fail loud.
fn uds_namespace(ctx: &StageCtx) -> Result<(), String> {
    let ws = ctx.tmp.join("ws");
    let home = ctx.tmp.join("home");
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let a = scope_cmd(ctx, &ws, &home, Some("alpha"), None)?;
    let b = scope_cmd(ctx, &ws, &home, Some("beta"), None)?;
    let sock = |m: &HashMap<String, String>| -> Result<String, String> {
        m.get("socket").cloned().ok_or(format!("no socket line: {m:?}"))
    };
    let (sa, sb) = (sock(&a)?, sock(&b)?);
    if sa == sb {
        return Err(format!("alpha and beta share a socket: {sa}"));
    }
    let uds_root = home.join(".uds");
    for (scope, s) in [("alpha", &sa), ("beta", &sb)] {
        if !Path::new(s).starts_with(&uds_root) || !s.ends_with(scope) {
            return Err(format!("{scope} socket {s} not at {}/{scope}", uds_root.display()));
        }
        if s.contains("_razel_") {
            return Err(format!("{scope} socket {s} strays into razel's namespace"));
        }
    }
    let bad = Command::new(&ctx.grazel_bin)
        .current_dir(&ws)
        .env("GRAZEL_HOME", &home)
        .args(["scope", "--scope=Not/Valid"])
        .output()
        .map_err(|e| e.to_string())?;
    if bad.status.success() {
        return Err("invalid scope name was accepted".into());
    }
    Ok(())
}

// --- GR1: the dial-procedure stages (§1e) ------------------------------------

/// `grazel daemon ping` with a controlled env; parses key=value output.
/// `fake_protocol` plants GRAZEL_FAKE_WIRE_PROTOCOL (the documented test seam —
/// only an already-running daemon's ADVERTISED protocol is affected).
fn ping(
    ctx: &StageCtx,
    home: &Path,
    ws: &Path,
    scope: &str,
    no_autostart: bool,
) -> Result<HashMap<String, String>, String> {
    let mut cmd = Command::new(&ctx.grazel_bin);
    cmd.current_dir(ws)
        .env_remove("GRAZEL_SCOPE")
        .env_remove("GRAZEL_FAKE_WIRE_PROTOCOL")
        .env("GRAZEL_HOME", home)
        .args(["daemon", "ping", &format!("--scope={scope}")]);
    if no_autostart {
        cmd.arg("--no_autostart");
    }
    let out = cmd.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "ping {scope} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect())
}

/// Best-effort `daemon stop` — stage-end cleanup for autostarted daemons
/// (they are grandchildren; no Child handle to reap).
fn stop_scope(ctx: &StageCtx, home: &Path, ws: &Path, scope: &str) {
    let _ = Command::new(&ctx.grazel_bin)
        .current_dir(ws)
        .env("GRAZEL_HOME", home)
        .args(["daemon", "stop", &format!("--scope={scope}")])
        .output();
}

fn pid_of(m: &HashMap<String, String>) -> Result<u32, String> {
    m.get("pid")
        .and_then(|p| p.parse().ok())
        .ok_or(format!("no pid in ping output: {m:?}"))
}

fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output() // captured: a dead pid's "No such process" is the expected case
        .is_ok_and(|o| o.status.success())
}

/// Poll until `f` holds or patience runs out.
fn eventually(what: &str, mut f: impl FnMut() -> bool) -> Result<(), String> {
    for _ in 0..100 {
        if f() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("timed out waiting for {what}"))
}

/// No daemon → dial → daemon autostarts → hello answers. `--no_autostart` refuses.
fn cold_autostart(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    if ping(ctx, &home, &ws, "cold", true).is_ok() {
        return Err("--no_autostart dialed a daemon that cannot exist".into());
    }
    let m = ping(ctx, &home, &ws, "cold", false)?;
    let result = (|| {
        let pid = pid_of(&m)?;
        if !pid_alive(pid) {
            return Err(format!("autostarted daemon pid {pid} not alive"));
        }
        let paths = ScopePaths::new(&home, "cold")?;
        if !paths.daemon_json.is_file() {
            return Err("no daemon.json after autostart".into());
        }
        Ok(())
    })();
    stop_scope(ctx, &home, &ws, "cold");
    result
}

/// Second dial reaches the SAME daemon — no respawn on a warm socket.
fn warm_dial(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let result = (|| {
        let first = pid_of(&ping(ctx, &home, &ws, "warm", false)?)?;
        let second = pid_of(&ping(ctx, &home, &ws, "warm", false)?)?;
        if first != second {
            return Err(format!("warm dial respawned: pid {first} → {second}"));
        }
        Ok(())
    })();
    stop_scope(ctx, &home, &ws, "warm");
    result
}

/// A daemon advertising the wrong wire protocol is gracefully replaced,
/// scope-local (§1e: Bazel's restart semantics).
fn version_handshake(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let paths = ScopePaths::new(&home, "hs")?;
    let mismatched = Command::new(&ctx.grazel_bin)
        .current_dir(&ws)
        .env("GRAZEL_HOME", &home)
        .env("GRAZEL_FAKE_WIRE_PROTOCOL", "99")
        .args(["daemon", "run", "--scope=hs"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let _reap = Reap(vec![mismatched]);
    let result = (|| {
        eventually("mismatched daemon up", || paths.socket.exists())?;
        let m = ping(ctx, &home, &ws, "hs", false)?;
        if m.get("protocol").map(String::as_str) != Some("1") {
            return Err(format!("post-handshake daemon still wrong: {m:?}"));
        }
        if m.get("restarted").map(String::as_str) != Some("true") {
            return Err(format!("expected restarted=true after mismatch: {m:?}"));
        }
        Ok(())
    })();
    stop_scope(ctx, &home, &ws, "hs");
    result
}

/// kill -9 leaves a dead socket + stale daemon.json; the dial detects the dead
/// pid, cleans, and relaunches.
fn stale_socket_recovery(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let paths = ScopePaths::new(&home, "stale")?;
    let mut victim = Command::new(&ctx.grazel_bin)
        .current_dir(&ws)
        .env("GRAZEL_HOME", &home)
        .args(["daemon", "run", "--scope=stale"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    eventually("victim daemon up", || paths.socket.exists())?;
    let old_pid = victim.id();
    victim.kill().map_err(|e| e.to_string())?; // SIGKILL: no cleanup runs
    victim.wait().map_err(|e| e.to_string())?;
    if !paths.socket.exists() {
        return Err("SIGKILL removed the socket?! stage premise broken".into());
    }
    let result = (|| {
        let new_pid = pid_of(&ping(ctx, &home, &ws, "stale", false)?)?;
        if new_pid == old_pid {
            return Err("dial returned the killed daemon's pid".into());
        }
        Ok(())
    })();
    stop_scope(ctx, &home, &ws, "stale");
    result
}

/// `grazel daemon stop` takes the daemon down and clears socket + daemon.json.
fn graceful_shutdown(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let pid = pid_of(&ping(ctx, &home, &ws, "bye", false)?)?;
    let paths = ScopePaths::new(&home, "bye")?;
    let out = Command::new(&ctx.grazel_bin)
        .current_dir(&ws)
        .env("GRAZEL_HOME", &home)
        .args(["daemon", "stop", "--scope=bye"])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("daemon stop failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    eventually("daemon death", || !pid_alive(pid))?;
    eventually("socket cleanup", || !paths.socket.exists())?;
    eventually("daemon.json cleanup", || !paths.daemon_json.exists())?;
    // Idempotent: stopping a stopped scope is calm, not an error.
    let again = Command::new(&ctx.grazel_bin)
        .current_dir(&ws)
        .env("GRAZEL_HOME", &home)
        .args(["daemon", "stop", "--scope=bye"])
        .output()
        .map_err(|e| e.to_string())?;
    if !again.status.success() {
        return Err("second `daemon stop` errored on a stopped scope".into());
    }
    Ok(())
}

/// An idle daemon times itself out and cleans up after itself.
fn idle_out(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let paths = ScopePaths::new(&home, "lazy")?;
    let mut child = Command::new(&ctx.grazel_bin)
        .current_dir(&ws)
        .env("GRAZEL_HOME", &home)
        .args(["daemon", "run", "--scope=lazy", "--idle-timeout=1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    eventually("lazy daemon up", || paths.socket.exists())?;
    eventually("idle-out exit", || matches!(child.try_wait(), Ok(Some(_))))?;
    if paths.socket.exists() || paths.daemon_json.exists() {
        return Err("idle-out left socket or daemon.json behind".into());
    }
    Ok(())
}

/// Kills the daemon children even when the stage errors out early.
struct Reap(Vec<Child>);
impl Drop for Reap {
    fn drop(&mut self) {
        for c in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Two scopes ⇒ two daemons on two sockets, both answering the hello — the §1e
/// isolation unit, live. Graceful shutdown is a later GR1 rung; SIGKILL here.
fn two_scopes_hello(ctx: &StageCtx) -> Result<(), String> {
    let ws = ctx.tmp.join("ws");
    let home = ctx.tmp.join("home");
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let mut reap = Reap(vec![]);
    for scope in ["a", "b"] {
        let child = Command::new(&ctx.grazel_bin)
            .current_dir(&ws)
            .env("GRAZEL_HOME", &home)
            .env_remove("GRAZEL_SCOPE")
            .args(["daemon", "run", &format!("--scope={scope}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn daemon {scope}: {e}"))?;
        reap.0.push(child);
    }
    for (i, scope) in ["a", "b"].into_iter().enumerate() {
        let paths = ScopePaths::new(&home, scope)?;
        eventually("daemon socket", || paths.socket.exists())?;
        let v = crate::dial::hello_once(&paths.socket, &ws)
            .map_err(|e| format!("scope {scope}: {e}"))?;
        if v.protocol != razel_daemon::rpc::PROTOCOL {
            return Err(format!("scope {scope}: wire protocol {} ≠ {}", v.protocol, razel_daemon::rpc::PROTOCOL));
        }
        let dj = std::fs::read_to_string(&paths.daemon_json)
            .map_err(|e| format!("scope {scope} daemon.json: {e}"))?;
        let pid = reap.0[i].id();
        if !dj.contains(&format!("\"pid\":{pid}")) {
            return Err(format!("scope {scope} daemon.json {} lacks pid {pid}", dj.trim()));
        }
    }
    Ok(())
}
