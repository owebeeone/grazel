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
        Stage { name: "grazel-key-in-razelrc-errors", run: grazel_key_in_razelrc_errors },
        Stage { name: "scope-routing", run: scope_routing },
        Stage { name: "pinned-membership", run: pinned_membership },
        Stage { name: "dynamic-membership-idle-out", run: dynamic_membership_idle_out },
        Stage { name: "double-claim-fails-loud", run: double_claim_fails_loud },
        Stage { name: "two-scopes-concurrent", run: two_scopes_concurrent },
        Stage { name: "http-equivalence", run: http_equivalence },
        Stage { name: "http-localhost-only", run: http_localhost_only },
        Stage { name: "build-parity", run: build_parity },
        Stage { name: "build-streamed", run: build_streamed },
        Stage { name: "run-verb", run: run_verb },
        Stage { name: "ws-stream-equivalence", run: ws_stream_equivalence },
        Stage { name: "js-client-roundtrip", run: js_client_roundtrip },
        Stage { name: "shutdown-verb", run: shutdown_verb },
        Stage { name: "no-daemon-escape", run: no_daemon_escape },
        Stage { name: "daemon-status", run: daemon_status },
        Stage { name: "view-fanout-dedup", run: view_fanout_dedup },
        Stage { name: "view-resync-after-drop", run: view_resync_after_drop },
        Stage { name: "view-idle-closes-upstream", run: view_idle_closes_upstream },
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
/// `scope: None` exercises rc/default resolution instead of the flag.
fn ping(
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
fn warm_dial(ctx: &StageCtx) -> Result<(), String> {
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
fn graceful_shutdown(ctx: &StageCtx) -> Result<(), String> {
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

/// Member files under `<state>/members/` — contents are `<kind> <root>` lines.
fn members_of(home: &Path, scope: &str) -> Result<Vec<String>, String> {
    let dir = ScopePaths::new(home, scope)?.state_dir.join("members");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(vec![]);
    };
    Ok(entries
        .filter_map(|e| std::fs::read_to_string(e.ok()?.path()).ok())
        .map(|s| s.trim().to_string())
        .collect())
}

fn spawn_daemon(ctx: &StageCtx, home: &Path, ws: &Path, args: &[&str]) -> Result<Child, String> {
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
fn grazel_key_in_razelrc_errors(ctx: &StageCtx) -> Result<(), String> {
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
fn scope_routing(ctx: &StageCtx) -> Result<(), String> {
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
fn pinned_membership(ctx: &StageCtx) -> Result<(), String> {
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
fn dynamic_membership_idle_out(ctx: &StageCtx) -> Result<(), String> {
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
fn double_claim_fails_loud(ctx: &StageCtx) -> Result<(), String> {
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

/// The GR2 exit: two fixture workspaces in different scopes BUILD concurrently —
/// real `build` wire calls racing into two daemons.
fn two_scopes_concurrent(ctx: &StageCtx) -> Result<(), String> {
    const BUILD: &str = r#"
def _impl(ctx):
    out = ctx.attr.name + ".o"
    ctx.actions.run(executable="/usr/bin/cc", outputs=[out], inputs=[ctx.attr.src],
                    arguments=["-c", ctx.attr.src, "-o", out])
    return [DefaultInfo(files=[out])]
cc_obj = rule(implementation=_impl, attrs={"src":1})
cc_obj(name="widget", src="widget.c")
"#;
    let home = ctx.tmp.join("home");
    let mut sockets = vec![];
    for scope in ["ca", "cb"] {
        let ws = ctx.tmp.join(format!("ws-{scope}"));
        std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
        std::fs::write(ws.join("BUILD"), BUILD).map_err(|e| e.to_string())?;
        std::fs::write(ws.join("widget.c"), "int answer(void) { return 42; }\n")
            .map_err(|e| e.to_string())?;
        ping(ctx, &home, &ws, Some(scope), false)?; // autostart + membership
        sockets.push(ScopePaths::new(&home, scope)?.socket);
    }
    let result = (|| {
        let builds: Vec<_> = sockets
            .iter()
            .map(|s| {
                let s = s.clone();
                std::thread::spawn(move || -> Result<(), String> {
                    let resp = razel_daemon::rpc::call(&s, &razel_daemon::rpc::req_build("widget"))
                        .map_err(|e| e.to_string())?;
                    let r = razel_wire::BuildResult::from_cbor(
                        &razel_daemon::rpc::payload(&resp)?,
                    );
                    if r.status != razel_wire::BuildStatus::Built {
                        return Err(format!("widget not Built: {:?} {:?}", r.status, r.message));
                    }
                    Ok(())
                })
            })
            .collect();
        for (i, b) in builds.into_iter().enumerate() {
            b.join().map_err(|_| "build thread panicked".to_string())?
                .map_err(|e| format!("scope {}: {e}", ["ca", "cb"][i]))?;
        }
        Ok(())
    })();
    for scope in ["ca", "cb"] {
        stop_scope(ctx, &home, &ctx.tmp.join(format!("ws-{scope}")), scope);
    }
    result
}

// --- GR3a: verbs through the scope daemon (GrazelVerbs.md) ---------------------

/// The §1d claim re-proven over the daemon path (GR3 exit): WARM `grazel build`
/// and WARM `razel build --daemon --socket <same grazeld>` must be byte-identical
/// in stdout and exit code — same lib, same daemon, same bytes.
fn build_parity(ctx: &StageCtx) -> Result<(), String> {
    const BUILD: &str = r#"
def _impl(ctx):
    out = ctx.attr.name + ".o"
    ctx.actions.run(executable="/usr/bin/cc", outputs=[out], inputs=[ctx.attr.src],
                    arguments=["-c", ctx.attr.src, "-o", out])
    return [DefaultInfo(files=[out])]
cc_obj = rule(implementation=_impl, attrs={"src":1})
cc_obj(name="widget", src="widget.c")
"#;
    let razel_bin = ctx.grazel_bin.parent().expect("bin dir").join("razel");
    if !razel_bin.is_file() {
        return Err(format!(
            "razel binary missing at {} — build it (cargo build -p razel-cli); parity needs both CLIs",
            razel_bin.display()
        ));
    }
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    std::fs::write(ws.join("BUILD"), BUILD).map_err(|e| e.to_string())?;
    std::fs::write(ws.join("widget.c"), "int answer(void) { return 42; }\n")
        .map_err(|e| e.to_string())?;
    let result = (|| {
        let run = |bin: &Path, args: &[&str]| -> Result<(i32, Vec<u8>), String> {
            let out = Command::new(bin)
                .current_dir(&ws)
                .env("GRAZEL_HOME", &home)
                .env_remove("GRAZEL_SCOPE")
                .args(args)
                .output()
                .map_err(|e| e.to_string())?;
            Ok((out.status.code().unwrap_or(-1), out.stdout))
        };
        // Cold grazel build autostarts the scope daemon and warms the cache…
        let (c0, _) = run(&ctx.grazel_bin, &["build", "widget", "--scope=par"])?;
        if c0 != 0 {
            return Err(format!("cold grazel build failed ({c0})"));
        }
        // …then the WARM pair must agree byte-for-byte.
        let (gc, gout) = run(&ctx.grazel_bin, &["build", "widget", "--scope=par"])?;
        let socket = ScopePaths::new(&home, "par")?.socket;
        let (rc, rout) = run(
            &razel_bin,
            &["build", "widget", "--daemon", "--socket", &socket.display().to_string()],
        )?;
        if gc != rc {
            return Err(format!("exit codes diverge: grazel {gc} ≠ razel {rc}"));
        }
        if gout != rout {
            return Err(format!(
                "stdout diverges ({} vs {} bytes):\n--- grazel ---\n{}--- razel ---\n{}",
                gout.len(),
                rout.len(),
                String::from_utf8_lossy(&gout),
                String::from_utf8_lossy(&rout)
            ));
        }
        Ok(())
    })();
    stop_scope(ctx, &home, &ws, "par");
    result
}

/// An executable fixture: cc-compiled C printing a marker and exiting 7.
const RUN_BUILD: &str = r#"
def _impl(ctx):
    out = ctx.attr.name
    ctx.actions.run(executable="/usr/bin/cc", outputs=[out], inputs=[ctx.attr.src],
                    arguments=[ctx.attr.src, "-o", out])
    return [DefaultInfo(files=[out])]
cc_bin = rule(implementation=_impl, attrs={"src":1})
cc_bin(name="hello", src="hello.c")
"#;
const RUN_SRC: &str = "#include <stdio.h>\nint main(void){ printf(\"hello-from-grazel-run\\n\"); return 7; }\n";

/// The §4b ordering guarantees, asserted THROUGH grazeld (the GR3 point — the
/// stream crosses the scope socket and the member router): id-before-events,
/// gap-free per-invocation seq, progress strictly before the terminal result.
fn build_streamed(ctx: &StageCtx) -> Result<(), String> {
    use razel_wire::{InvocationEvent, InvocationStarted};
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    std::fs::write(ws.join("BUILD"), RUN_BUILD).map_err(|e| e.to_string())?;
    std::fs::write(ws.join("hello.c"), RUN_SRC).map_err(|e| e.to_string())?;
    let result = (|| {
        ping(ctx, &home, &ws, Some("bs"), false)?; // daemon + membership
        let socket = ScopePaths::new(&home, "bs")?.socket;
        let resp = razel_daemon::rpc::call(&socket, &razel_daemon::rpc::req_run("hello", &[]))
            .map_err(|e| e.to_string())?;
        let started = InvocationStarted::from_cbor(
            &razel_daemon::rpc::payload(&resp).map_err(|e| format!("run: {e}"))?,
        );
        if started.invocation_id.is_empty() {
            return Err("no invocation id (id-first violated)".into());
        }
        let mut events = razel_daemon::rpc::invocation_events(&socket).map_err(|e| e.to_string())?;
        // seq is 1-based: the server's transcripts pin gap-free-from-1 (rpc.rs).
        let (mut next_seq, mut progress_seen, mut terminal) = (1i64, 0u32, false);
        while !terminal {
            let frame = razel_daemon::rpc::next_frame(&mut events).map_err(|e| e.to_string())?;
            let ev = InvocationEvent::from_cbor(&razel_daemon::rpc::payload(&frame)?);
            if ev.invocation_id != started.invocation_id {
                continue;
            }
            if ev.seq != next_seq {
                return Err(format!("seq gap: got {} want {next_seq}", ev.seq));
            }
            next_seq += 1;
            match (&ev.progress, &ev.result) {
                (Some(_), None) if !terminal => progress_seen += 1,
                (None, Some(_)) => terminal = true,
                _ => return Err(format!("event has bad arm shape at seq {}", ev.seq)),
            }
        }
        if progress_seen == 0 {
            return Err("no progress events before the terminal result".into());
        }
        Ok(())
    })();
    stop_scope(ctx, &home, &ws, "bs");
    result
}

/// `grazel run` end-to-end: streamed build through grazeld, then the program
/// runs locally — stdout is the program's, exit code propagates.
fn run_verb(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    std::fs::write(ws.join("BUILD"), RUN_BUILD).map_err(|e| e.to_string())?;
    std::fs::write(ws.join("hello.c"), RUN_SRC).map_err(|e| e.to_string())?;
    let result = (|| {
        let out = Command::new(&ctx.grazel_bin)
            .current_dir(&ws)
            .env("GRAZEL_HOME", &home)
            .env_remove("GRAZEL_SCOPE")
            .args(["run", "hello", "--scope=rv"])
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.code() != Some(7) {
            return Err(format!(
                "exit code {:?}, want the program's 7; stderr: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        if !stdout.contains("hello-from-grazel-run") {
            return Err(format!("program output missing from stdout: {stdout:?}"));
        }
        Ok(())
    })();
    stop_scope(ctx, &home, &ws, "rv");
    result
}

/// Stage-side WS client, masked frames per RFC (the server requires masking).
struct WsClient(std::net::TcpStream);

impl WsClient {
    fn connect(port: u16) -> Result<Self, String> {
        use std::io::{Read, Write};
        let mut s =
            std::net::TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
        // Fixed nonce: the handshake's accept hash is what we verify, not entropy.
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let req = format!(
            "GET /events HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            s.read_exact(&mut byte).map_err(|e| format!("upgrade read: {e}"))?;
            head.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&head);
        if !head.starts_with("HTTP/1.1 101") {
            return Err(format!("upgrade refused: {}", head.lines().next().unwrap_or("")));
        }
        if !head.contains(&crate::ws::accept_key(key)) {
            return Err("Sec-WebSocket-Accept mismatch".into());
        }
        Ok(Self(s))
    }

    fn send(&mut self, payload: &[u8]) -> Result<(), String> {
        use std::io::Write;
        let mask = [0x12u8, 0x34, 0x56, 0x78];
        let mut frame = vec![0x82u8];
        match payload.len() {
            n if n < 126 => frame.push(0x80 | n as u8),
            n if n < 65536 => {
                frame.push(0x80 | 126);
                frame.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                frame.push(0x80 | 127);
                frame.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.0.write_all(&frame).map_err(|e| e.to_string())
    }

    fn recv(&mut self) -> Result<Vec<u8>, String> {
        crate::ws::read_message(&mut self.0)
            .map_err(|e| e.to_string())?
            .ok_or("server closed the stream".into())
    }

    /// RFC close frame (masked, empty payload), then drop the TCP side.
    fn close(&mut self) {
        use std::io::Write;
        let _ = self.0.write_all(&[0x88, 0x80, 0, 0, 0, 0]);
        let _ = self.0.flush();
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

/// GR4's full claim: a WS subscriber sees the SAME event byte-sequence a UDS
/// subscriber sees — payload-identical, only the framing differs by transport.
fn ws_stream_equivalence(ctx: &StageCtx) -> Result<(), String> {
    use razel_wire::{InvocationEvent, InvocationStarted};
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    std::fs::write(ws.join("BUILD"), RUN_BUILD).map_err(|e| e.to_string())?;
    std::fs::write(ws.join("hello.c"), RUN_SRC).map_err(|e| e.to_string())?;
    let result = (|| {
        ping(ctx, &home, &ws, Some("wse"), false)?;
        let socket = ScopePaths::new(&home, "wse")?.socket;
        // One finished invocation in the log…
        let resp = razel_daemon::rpc::call(&socket, &razel_daemon::rpc::req_run("hello", &[]))
            .map_err(|e| e.to_string())?;
        let id = InvocationStarted::from_cbor(&razel_daemon::rpc::payload(&resp)?).invocation_id;
        // …then both transports replay it from 0; collect raw envelopes through
        // our invocation's terminal event.
        let until_terminal = |mut next: Box<dyn FnMut() -> Result<Vec<u8>, String>>| {
            let mut seen = Vec::new();
            loop {
                let raw = next()?;
                let env = razel_wire::decode(&raw);
                let ev = InvocationEvent::from_cbor(
                    &razel_daemon::rpc::payload(&env).map_err(|e| format!("event: {e}"))?,
                );
                let terminal = ev.invocation_id == id && ev.result.is_some();
                seen.push(raw);
                if terminal {
                    return Ok::<_, String>(seen);
                }
            }
        };
        let mut uds = razel_daemon::transport::connect(&socket).map_err(|e| e.to_string())?;
        {
            use std::io::Write;
            let req = razel_wire::encode(&razel_daemon::rpc::req_invocation_events());
            let mut framed = (req.len() as u32).to_be_bytes().to_vec();
            framed.extend_from_slice(&req);
            uds.write_all(&framed).map_err(|e| e.to_string())?;
        }
        let uds_events = until_terminal(Box::new(move || {
            use std::io::Read;
            let mut len = [0u8; 4];
            uds.read_exact(&mut len).map_err(|e| e.to_string())?;
            let mut buf = vec![0u8; u32::from_be_bytes(len) as usize];
            uds.read_exact(&mut buf).map_err(|e| e.to_string())?;
            Ok(buf)
        }))?;
        let mut wsc = WsClient::connect(http_port_of(&home, "wse")?)?;
        wsc.send(&razel_wire::encode(&razel_daemon::rpc::req_invocation_events()))?;
        let ws_events = until_terminal(Box::new(move || wsc.recv()))?;
        if uds_events != ws_events {
            return Err(format!(
                "transport divergence: UDS {} events ≠ WS {} events (or bytes differ)",
                uds_events.len(),
                ws_events.len()
            ));
        }
        Ok(())
    })();
    stop_scope(ctx, &home, &ws, "wse");
    result
}

/// GR5a: the gryth attach point, proven from node — the IR-GENERATED js client
/// (clients/grazel-js) decodes the live grazeld over HTTP + WS. Host-node
/// posture: non-hermetic, version digest-logged here, loud if absent.
fn js_client_roundtrip(ctx: &StageCtx) -> Result<(), String> {
    let node_version = Command::new("node")
        .arg("--version")
        .output()
        .map_err(|_| "host node missing — GR5 takes the cc-toolchain posture: install node ≥22")?;
    eprintln!(
        "  [host-node digest] {}",
        String::from_utf8_lossy(&node_version.stdout).trim()
    );
    // Dev-tree posture (same as build-parity's razel-bin requirement): the
    // committed client lives at <repo>/clients/grazel-js relative to the bin.
    let client_dir = ctx
        .grazel_bin
        .ancestors()
        .find(|p| p.join("clients/grazel-js/smoke.js").is_file())
        .map(|p| p.join("clients/grazel-js"))
        .ok_or("clients/grazel-js/smoke.js not found above the grazel binary (dev tree required)")?;
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    std::fs::write(ws.join("BUILD"), RUN_BUILD).map_err(|e| e.to_string())?;
    std::fs::write(ws.join("hello.c"), RUN_SRC).map_err(|e| e.to_string())?;
    let result = (|| {
        ping(ctx, &home, &ws, Some("js"), false)?;
        let port = http_port_of(&home, "js")?;
        let ws_c = ws.canonicalize().map_err(|e| e.to_string())?;
        let out = Command::new("node")
            .arg(client_dir.join("smoke.js"))
            .arg(port.to_string())
            .arg(&ws_c)
            .output()
            .map_err(|e| e.to_string())?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() || !stdout.contains("smoke-ok") {
            return Err(format!(
                "smoke client failed ({}):\n{stdout}{}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        for marker in ["hello-ok", "run-ok", "events-ok"] {
            if !stdout.contains(marker) {
                return Err(format!("missing {marker} in smoke output:\n{stdout}"));
            }
        }
        Ok(())
    })();
    stop_scope(ctx, &home, &ws, "js");
    result
}

/// `grazel shutdown` — the user-facing stop (inbox 0007): targeted stop kills
/// one scope and leaves the other; `--all` sweeps the rest; idempotent.
fn shutdown_verb(ctx: &StageCtx) -> Result<(), String> {
    let home = ctx.tmp.join("home");
    let grazel = |ws: &Path, args: &[&str]| -> Result<(bool, String), String> {
        let out = Command::new(&ctx.grazel_bin)
            .current_dir(ws)
            .env("GRAZEL_HOME", &home)
            .env_remove("GRAZEL_SCOPE")
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        Ok((out.status.success(), String::from_utf8_lossy(&out.stdout).to_string()))
    };
    for scope in ["sa", "sb"] {
        let ws = ctx.tmp.join(format!("ws-{scope}"));
        std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
        ping(ctx, &home, &ws, Some(scope), false)?;
    }
    let (pa, pb) = (ScopePaths::new(&home, "sa")?, ScopePaths::new(&home, "sb")?);
    let wsa = ctx.tmp.join("ws-sa");
    let (ok, out) = grazel(&wsa, &["shutdown", "--scope=sa"])?;
    if !ok || !out.contains("scope=sa stopped") {
        return Err(format!("targeted shutdown failed: {out}"));
    }
    eventually("sa socket cleanup", || !pa.socket.exists() && !pa.daemon_json.exists())?;
    if !pb.socket.exists() {
        return Err("targeted shutdown took the OTHER scope down too".into());
    }
    let (ok, out) = grazel(&wsa, &["shutdown", "--all"])?;
    if !ok || !out.contains("scope=sb stopped") {
        return Err(format!("shutdown --all missed sb: {out}"));
    }
    eventually("sb socket cleanup", || !pb.socket.exists())?;
    // Idempotent: a second sweep over stopped scopes is calm.
    let (ok, _) = grazel(&wsa, &["shutdown", "--all"])?;
    if !ok {
        return Err("second shutdown --all errored".into());
    }
    let (ok, _) = grazel(&wsa, &["shutdown", "--all", "--scope=sa"])?;
    if ok {
        return Err("--all with --scope was accepted".into());
    }
    Ok(())
}

/// Survey P1: `--no_daemon` = razel-local semantics under grazel — the build
/// succeeds and NO daemon artifact (socket, daemon.json) is created.
fn no_daemon_escape(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    std::fs::write(ws.join("BUILD"), RUN_BUILD).map_err(|e| e.to_string())?;
    std::fs::write(ws.join("hello.c"), RUN_SRC).map_err(|e| e.to_string())?;
    let ws_abs = ws.canonicalize().map_err(|e| e.to_string())?.display().to_string();
    let out = Command::new(&ctx.grazel_bin)
        .current_dir(&ws)
        .env("GRAZEL_HOME", &home)
        .env_remove("GRAZEL_SCOPE")
        // ABSOLUTE -C works around razel's relative-workspace staging bug
        // (inbox 0010 — bare and `-C .` both fail cold); drop when fixed.
        .args(["build", "hello", "--no_daemon", "-C", &ws_abs])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "--no_daemon build failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    if home.join(".uds").exists() || home.join("scopes").exists() {
        return Err("--no_daemon created daemon artifacts under the grazel home".into());
    }
    // run --no_daemon: razel's local run verb — program output + exit code 7.
    let out = Command::new(&ctx.grazel_bin)
        .current_dir(&ws)
        .env("GRAZEL_HOME", &home)
        .args(["run", "hello", "--no_daemon", "-C", &ws_abs])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.code() != Some(7)
        || !String::from_utf8_lossy(&out.stdout).contains("hello-from-grazel-run")
    {
        return Err(format!(
            "run --no_daemon: exit {:?}, stdout {:?}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout)
        ));
    }
    if home.join(".uds").exists() {
        return Err("run --no_daemon started a daemon".into());
    }
    Ok(())
}

/// Survey P2: read-only observability — live, dead-stale, and empty scopes all
/// render without error (and without any wire call).
fn daemon_status(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let status = |scope: &str| -> Result<HashMap<String, String>, String> {
        let out = Command::new(&ctx.grazel_bin)
            .current_dir(&ws)
            .env("GRAZEL_HOME", &home)
            .env_remove("GRAZEL_SCOPE")
            .args(["daemon", "status", &format!("--scope={scope}")])
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!("status {scope} errored: {}", String::from_utf8_lossy(&out.stderr)));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect())
    };
    let result = (|| {
        // Live: status agrees with ping (pid, liveness, membership, port).
        let pid = pid_of(&ping(ctx, &home, &ws, Some("live"), false)?)?;
        let st = status("live")?;
        if st.get("alive").map(String::as_str) != Some("true")
            || st.get("pid") != Some(&pid.to_string())
        {
            return Err(format!("live status wrong: {st:?}"));
        }
        let ws_c = ws.canonicalize().map_err(|e| e.to_string())?;
        if !st.get("member").is_some_and(|m| m.contains(&ws_c.display().to_string())) {
            return Err(format!("live status lacks the member: {st:?}"));
        }
        if st.get("http_port").and_then(|p| p.parse::<u16>().ok()).unwrap_or(0) == 0 {
            return Err(format!("live status lacks http_port: {st:?}"));
        }
        // Dead-stale: SIGKILL leaves daemon.json; status says alive=false, no error.
        let mut victim = spawn_daemon(ctx, &home, &ws, &["daemon", "run", "--scope=stale"])?;
        let stale_paths = ScopePaths::new(&home, "stale")?;
        eventually("stale daemon up", || stale_paths.socket.exists())?;
        victim.kill().map_err(|e| e.to_string())?;
        victim.wait().map_err(|e| e.to_string())?;
        let st = status("stale")?;
        if st.get("alive").map(String::as_str) != Some("false") {
            return Err(format!("stale status not alive=false: {st:?}"));
        }
        // Empty: never-started scope renders daemon=none, exit 0.
        let st = status("never")?;
        if st.get("daemon").map(String::as_str) != Some("none") {
            return Err(format!("empty status missing daemon=none: {st:?}"));
        }
        // scope --list sees live=true and stale=false.
        let out = Command::new(&ctx.grazel_bin)
            .current_dir(&ws)
            .env("GRAZEL_HOME", &home)
            .args(["scope", "--list"])
            .output()
            .map_err(|e| e.to_string())?;
        let listing = String::from_utf8_lossy(&out.stdout).to_string();
        if !listing.contains("scope=live alive=true") || !listing.contains("scope=stale alive=false") {
            return Err(format!("scope --list wrong:\n{listing}"));
        }
        Ok(())
    })();
    stop_scope(ctx, &home, &ws, "live");
    result
}

// --- View seam (GrazelViewSeam.md): fan-out machinery over real streams --------

fn fanout_count(home: &Path, scope: &str, key: &str) -> Result<Option<u32>, String> {
    Ok(crate::daemon::status_lines(&ScopePaths::new(home, scope)?)
        .iter()
        .find_map(|l| l.strip_prefix(&format!("fanout={key}:")).and_then(|n| n.parse().ok())))
}

/// Two WS subscribers, same key ⇒ ONE shared upstream, identical byte streams.
fn view_fanout_dedup(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    std::fs::write(ws.join("BUILD"), RUN_BUILD).map_err(|e| e.to_string())?;
    std::fs::write(ws.join("hello.c"), RUN_SRC).map_err(|e| e.to_string())?;
    let result = (|| {
        ping(ctx, &home, &ws, Some("vf"), false)?;
        let port = http_port_of(&home, "vf")?;
        let sub_req = razel_wire::encode(&razel_daemon::rpc::req_subscribe());
        let mut a = WsClient::connect(port)?;
        a.send(&sub_req)?;
        let a0 = a.recv()?; // the atom's on-connect snapshot
        let mut b = WsClient::connect(port)?;
        b.send(&sub_req)?;
        let b0 = b.recv()?;
        eventually("two subscribers, one key", || {
            fanout_count(&home, "vf", "build.subscribe").ok().flatten() == Some(2)
        })?;
        if a0 != b0 {
            return Err("subscribers saw different snapshots".into());
        }
        // A revision bump reaches BOTH, byte-identical.
        let socket = ScopePaths::new(&home, "vf")?.socket;
        razel_daemon::rpc::call(&socket, &razel_daemon::rpc::req_build("hello"))
            .map_err(|e| e.to_string())?;
        let (a1, b1) = (a.recv()?, b.recv()?);
        if a1 != b1 {
            return Err("post-build frames diverge between subscribers".into());
        }
        Ok(())
    })();
    stop_scope(ctx, &home, &ws, "vf");
    result
}

/// The §4b drop-with-resync marker reaches a WS client through the whole stack.
/// `--view-buffer=0` is the determinism knob: every frame is preceded by a
/// marker, so one event proves the path without TCP-backpressure games (the
/// real bound's overflow arithmetic is unit-tested in views.rs).
fn view_resync_after_drop(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    std::fs::write(ws.join("BUILD"), RUN_BUILD).map_err(|e| e.to_string())?;
    std::fs::write(ws.join("hello.c"), RUN_SRC).map_err(|e| e.to_string())?;
    let child = spawn_daemon(
        ctx,
        &home,
        &ws,
        &["daemon", "run", "--scope=vr", "--view-buffer=0"],
    )?;
    let _reap = Reap(vec![child]);
    let result = (|| {
        let paths = ScopePaths::new(&home, "vr")?;
        eventually("vr daemon up", || paths.socket.exists())?;
        ping(ctx, &home, &ws, Some("vr"), false)?; // membership for the upstream
        let mut c = WsClient::connect(http_port_of(&home, "vr")?)?;
        c.send(&razel_wire::encode(&razel_daemon::rpc::req_invocation_events()))?;
        razel_daemon::rpc::call(&paths.socket, &razel_daemon::rpc::req_run("hello", &[]))
            .map_err(|e| e.to_string())?;
        let mut marker = false;
        let mut valid_after_marker = false;
        for _ in 0..20 {
            let frame = c.recv()?;
            let env = razel_wire::decode(&frame);
            let is_marker = matches!(env.get(1), razel_wire::Cbor::Bool(false))
                && matches!(env.get(3), razel_wire::Cbor::Text(t) if t.starts_with("resync"));
            if is_marker {
                marker = true;
            } else if marker {
                // A coherent envelope after the marker: the stream survived.
                razel_daemon::rpc::payload(&env).map_err(|e| format!("post-resync frame: {e}"))?;
                valid_after_marker = true;
                break;
            }
        }
        if !marker {
            return Err("no resync marker reached the client".into());
        }
        if !valid_after_marker {
            return Err("no coherent frame after the resync marker".into());
        }
        Ok(())
    })();
    stop_scope(ctx, &home, &ws, "vr");
    result
}

/// Last subscriber out closes the shared upstream (refcount → zero → key gone).
fn view_idle_closes_upstream(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let result = (|| {
        ping(ctx, &home, &ws, Some("vi"), false)?;
        let port = http_port_of(&home, "vi")?;
        let mut c = WsClient::connect(port)?;
        c.send(&razel_wire::encode(&razel_daemon::rpc::req_subscribe()))?;
        let _ = c.recv()?; // subscription is live
        eventually("fanout count 1", || {
            fanout_count(&home, "vi", "build.subscribe").ok().flatten() == Some(1)
        })?;
        c.close();
        eventually("fanout entry gone", || {
            fanout_count(&home, "vi", "build.subscribe").ok().flatten().is_none()
        })?;
        Ok(())
    })();
    stop_scope(ctx, &home, &ws, "vi");
    result
}

// --- GR4a: the HTTP edge (GrazelHttpEdge.md) -----------------------------------

/// `"http_port":N` out of daemon.json.
fn http_port_of(home: &Path, scope: &str) -> Result<u16, String> {
    let dj = ScopePaths::new(home, scope)?.daemon_json;
    let text = std::fs::read_to_string(&dj).map_err(|e| format!("{}: {e}", dj.display()))?;
    let digits = text
        .split("\"http_port\":")
        .nth(1)
        .ok_or(format!("no http_port in {}", text.trim()))?;
    digits[..digits.find(|c: char| !c.is_ascii_digit()).unwrap_or(digits.len())]
        .parse()
        .map_err(|e| format!("bad http_port: {e}"))
}

/// Minimal HTTP/1.1 POST from the stage side (the client mirror of the server's
/// minimalism — no deps either side).
fn http_post(port: u16, path: &str, body: &[u8]) -> Result<(u16, Vec<u8>), String> {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/cbor\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    s.write_all(body).map_err(|e| e.to_string())?;
    let mut resp = Vec::new();
    s.read_to_end(&mut resp).map_err(|e| e.to_string())?;
    let split = resp
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("no header/body split in HTTP response")?;
    let head = String::from_utf8_lossy(&resp[..split]).to_string();
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or(format!("bad status line: {head}"))?;
    Ok((status, resp[split + 4..].to_vec()))
}

/// One protocol, two transports: the SAME hello over UDS and HTTP must produce
/// byte-identical response envelopes (GR4's whole claim).
fn http_equivalence(ctx: &StageCtx) -> Result<(), String> {
    let (home, ws) = (ctx.tmp.join("home"), ctx.tmp.join("ws"));
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let result = (|| {
        ping(ctx, &home, &ws, Some("http"), false)?;
        let ws_c = ws.canonicalize().map_err(|e| e.to_string())?;
        let req = crate::daemon::req_hello(&ws_c);
        let socket = ScopePaths::new(&home, "http")?.socket;
        let uds_resp =
            razel_daemon::rpc::call(&socket, &req).map_err(|e| format!("uds hello: {e}"))?;
        let uds_bytes = razel_wire::encode(&uds_resp);
        let port = http_port_of(&home, "http")?;
        let (status, http_bytes) = http_post(port, "/rpc", &razel_wire::encode(&req))?;
        if status != 200 {
            return Err(format!("HTTP rpc returned {status}"));
        }
        if http_bytes != uds_bytes {
            return Err(format!(
                "transport divergence: UDS {} bytes ≠ HTTP {} bytes",
                uds_bytes.len(),
                http_bytes.len()
            ));
        }
        let (nf_status, _) = http_post(port, "/definitely-not-rpc", b"")?;
        if nf_status != 404 {
            return Err(format!("unknown path got {nf_status}, want 404"));
        }
        Ok(())
    })();
    stop_scope(ctx, &home, &ws, "http");
    result
}

/// Any non-loopback bind is refused at startup — localhost-only by construction
/// until the iroh-era auth story exists.
fn http_localhost_only(ctx: &StageCtx) -> Result<(), String> {
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

/// With OPT-IN `--idle-timeout`, an idle daemon times out and cleans up after
/// itself. (Default grazeld runs indefinitely — the long-lived scope service.)
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
/// isolation unit, live. DISTINCT workspaces per scope: one scope per workspace
/// is the rule the output-base lock enforces (GR2). SIGKILL cleanup is fine here.
fn two_scopes_hello(ctx: &StageCtx) -> Result<(), String> {
    let home = ctx.tmp.join("home");
    let mut reap = Reap(vec![]);
    for scope in ["a", "b"] {
        let ws = ctx.tmp.join(format!("ws-{scope}"));
        std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
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
        let ws = ctx.tmp.join(format!("ws-{scope}"));
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
