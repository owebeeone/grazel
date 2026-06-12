//! `grazel ws test` — the workstream's acceptance instrument (GrazelWorkstream §1).
//!
//! Stages exercise the REAL binary end-to-end (CLI → scope resolution → socket →
//! daemon → wire → back); a stage that could pass against a mock is wrongly
//! written. The same ladder runs as the shipped verb and in-process under
//! `cargo test` (grazel-cli's integration test) — only `StageCtx.grazel_bin`
//! differs (`current_exe` vs `CARGO_BIN_EXE_grazel`).

use crate::daemon;
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
    for stage in ladder {
        if only.is_some_and(|o| o != stage.name) {
            continue;
        }
        let dir = ctx.tmp.join(stage.name);
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
        let v = daemon::wait_dial_version(&paths.socket, 100, Duration::from_millis(50))
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
