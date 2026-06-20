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
use std::time::Instant;


use super::*;

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

pub(crate) fn harness_selftest(ctx: &StageCtx) -> Result<(), String> {
    if !ctx.grazel_bin.is_file() {
        return Err(format!("grazel binary missing: {}", ctx.grazel_bin.display()));
    }
    std::fs::write(ctx.tmp.join("probe"), b"ok").map_err(|e| format!("tmp not writable: {e}"))
}

/// Run `grazel scope` in `dir` with a controlled env; parse its key=value output.
pub(crate) fn scope_cmd(
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
pub(crate) fn scope_chain(ctx: &StageCtx) -> Result<(), String> {
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
pub(crate) fn uds_namespace(ctx: &StageCtx) -> Result<(), String> {
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
/// `scope: None` exercises rc/default resolution instead of the flag.
pub(crate) fn ping(
    ctx: &StageCtx,
    home: &Path,
    ws: &Path,
    scope: Option<&str>,
    no_autostart: bool,
) -> Result<HashMap<String, String>, String> {
    let mut cmd = Command::new(&ctx.grazel_bin);
    cmd.current_dir(ws)
        .env_remove("GRAZEL_SCOPE")
        .env_remove("GRAZEL_FAKE_WIRE_PROTOCOL")
        .env("GRAZEL_HOME", home)
        .args(["daemon", "ping"]);
    if let Some(s) = scope {
        cmd.arg(format!("--scope={s}"));
    }
    if no_autostart {
        cmd.arg("--no_autostart");
    }
    let out = cmd.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "ping {scope:?} failed ({}): {}",
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
pub(crate) fn stop_scope(ctx: &StageCtx, home: &Path, ws: &Path, scope: &str) {
    let _ = Command::new(&ctx.grazel_bin)
        .current_dir(ws)
        .env("GRAZEL_HOME", home)
        .args(["daemon", "stop", &format!("--scope={scope}")])
        .output();
}

pub(crate) fn pid_of(m: &HashMap<String, String>) -> Result<u32, String> {
    m.get("pid")
        .and_then(|p| p.parse().ok())
        .ok_or(format!("no pid in ping output: {m:?}"))
}

/// No daemon → dial → daemon autostarts → hello answers. `--no_autostart` refuses.
pub(crate) fn cold_autostart(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    if ping(ctx, &home, &ws, Some("cold"), true).is_ok() {
        return Err("--no_autostart dialed a daemon that cannot exist".into());
    }
    let m = ping(ctx, &home, &ws, Some("cold"), false)?;
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
pub(crate) fn warm_dial(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let result = (|| {
        let first = pid_of(&ping(ctx, &home, &ws, Some("warm"), false)?)?;
        let second = pid_of(&ping(ctx, &home, &ws, Some("warm"), false)?)?;
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
pub(crate) fn version_handshake(ctx: &StageCtx) -> Result<(), String> {
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
        let m = ping(ctx, &home, &ws, Some("hs"), false)?;
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
pub(crate) fn stale_socket_recovery(ctx: &StageCtx) -> Result<(), String> {
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
        let new_pid = pid_of(&ping(ctx, &home, &ws, Some("stale"), false)?)?;
        if new_pid == old_pid {
            return Err("dial returned the killed daemon's pid".into());
        }
        Ok(())
    })();
    stop_scope(ctx, &home, &ws, "stale");
    result
}

/// `grazel daemon stop` takes the daemon down and clears socket + daemon.json.
pub(crate) fn graceful_shutdown(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let pid = pid_of(&ping(ctx, &home, &ws, Some("bye"), false)?)?;
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

// --- GR2: service scopes for real (GrazelScopes.md) ---------------------------

pub(crate) fn spawn_daemon(ctx: &StageCtx, home: &Path, ws: &Path, args: &[&str]) -> Result<Child, String> {
    Command::new(&ctx.grazel_bin)
        .current_dir(ws)
        .env("GRAZEL_HOME", home)
        .env_remove("GRAZEL_SCOPE")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())
}

/// §1e config arrow: a grazel key in `.razelrc` is an ERROR (razel must never
/// become grazel-aware).
pub(crate) fn grazel_key_in_razelrc_errors(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    std::fs::write(ws.join(".razelrc"), "service_scope=oops\n").map_err(|e| e.to_string())?;
    let out = Command::new(&ctx.grazel_bin)
        .current_dir(&ws)
        .env("GRAZEL_HOME", &home)
        .arg("scope")
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        return Err("service_scope in .razelrc was accepted".into());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stderr.contains(".razelrc") || !stderr.contains(".grazelrc") {
        return Err(format!("error doesn't point from .razelrc to .grazelrc: {stderr}"));
    }
    Ok(())
}

/// An rc-bound workspace's hello lands its membership in the RIGHT scope's state.
pub(crate) fn scope_routing(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    std::fs::write(ws.join(".grazelrc"), "service_scope=routed\n").map_err(|e| e.to_string())?;
    let result = (|| {
        let m = ping(ctx, &home, &ws, None, false)?; // rc decides, not flag/env
        if m.get("scope").map(String::as_str) != Some("routed") {
            return Err(format!("rc binding ignored: {m:?}"));
        }
        let routed = members_of(&home, "routed")?;
        // Member roots are canonical (daemon-side); canonicalize the expectation.
        let ws_str = ws.canonicalize().map_err(|e| e.to_string())?.display().to_string();
        if !routed.iter().any(|l| l.contains(&ws_str)) {
            return Err(format!("workspace not a member of scope routed: {routed:?}"));
        }
        if !members_of(&home, "default")?.is_empty() {
            return Err("workspace leaked into the default scope".into());
        }
        Ok(())
    })();
    stop_scope(ctx, &home, &ws, "routed");
    result
}

/// scope.rc-pinned workspaces are claimed and opened AT DAEMON START — no hello.
pub(crate) fn pinned_membership(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let paths = ScopePaths::new(&home, "pin")?;
    std::fs::create_dir_all(&paths.state_dir).map_err(|e| e.to_string())?;
    std::fs::write(paths.state_dir.join("scope.rc"), format!("pinned={}\n", ws.display()))
        .map_err(|e| e.to_string())?;
    let child = spawn_daemon(ctx, &home, &ws, &["daemon", "run", "--scope=pin"])?;
    let _reap = Reap(vec![child]);
    let result = (|| {
        eventually("pin daemon up", || paths.socket.exists())?;
        let members = members_of(&home, "pin")?;
        let want = format!("pinned {}", ws.canonicalize().map_err(|e| e.to_string())?.display());
        if !members.iter().any(|l| l == &want) {
            return Err(format!("no pinned member {want:?}: {members:?}"));
        }
        if !ws.join(".razel-cache/workspace.lock").is_file() {
            return Err("pinned open took no output-base lock".into());
        }
        Ok(())
    })();
    stop_scope(ctx, &home, &ws, "pin");
    result
}

/// A dynamic member idles out (lock + member file released) while the DAEMON
/// stays up — memberships idle, grazeld doesn't.
pub(crate) fn dynamic_membership_idle_out(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let paths = ScopePaths::new(&home, "dyn")?;
    let child = spawn_daemon(
        ctx,
        &home,
        &ws,
        &["daemon", "run", "--scope=dyn", "--member-idle-timeout=1"],
    )?;
    let _reap = Reap(vec![child]);
    let result = (|| {
        eventually("dyn daemon up", || paths.socket.exists())?;
        let pid = pid_of(&ping(ctx, &home, &ws, Some("dyn"), false)?)?;
        let lock = ws.join(".razel-cache/workspace.lock");
        if members_of(&home, "dyn")?.is_empty() || !lock.is_file() {
            return Err("hello opened no dynamic member/lock".into());
        }
        eventually("member idle-out", || {
            members_of(&home, "dyn").is_ok_and(|m| m.is_empty()) && !lock.exists()
        })?;
        if !pid_alive(pid) {
            return Err("daemon died with its member — only the MEMBERSHIP idles".into());
        }
        Ok(())
    })();
    stop_scope(ctx, &home, &ws, "dyn");
    result
}

/// One scope per workspace, enforced: the second scope's claim is refused LOUD,
/// naming the holder (§1b cross-daemon single-writer).
pub(crate) fn double_claim_fails_loud(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let result = (|| {
        let first = ping(ctx, &home, &ws, Some("left"), false)?;
        let holder_pid = pid_of(&first)?;
        let second = ping(ctx, &home, &ws, Some("right"), false);
        let Err(e) = second else {
            return Err("second scope claimed an already-held workspace".into());
        };
        if !e.contains("held by") || !e.contains("left") || !e.contains(&holder_pid.to_string()) {
            return Err(format!("refusal doesn't name the holder (scope left, pid {holder_pid}): {e}"));
        }
        Ok(())
    })();
    stop_scope(ctx, &home, &ws, "left");
    stop_scope(ctx, &home, &ws, "right");
    result
}

/// Kills the daemon children even when the stage errors out early.
pub(crate) struct Reap(pub(crate) Vec<Child>);
