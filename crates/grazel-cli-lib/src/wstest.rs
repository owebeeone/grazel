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
