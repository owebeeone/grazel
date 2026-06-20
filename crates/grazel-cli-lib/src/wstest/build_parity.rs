//! `grazel ws test` — the workstream's acceptance instrument (GrazelWorkstream §1).
//!
//! Stages exercise the REAL binary end-to-end (CLI → scope resolution → socket →
//! daemon → wire → back); a stage that could pass against a mock is wrongly
//! written. The same ladder runs as the shipped verb and in-process under
//! `cargo test` (grazel-cli's integration test) — only `StageCtx.grazel_bin`
//! differs (`current_exe` vs `CARGO_BIN_EXE_grazel`).

use crate::paths::ScopePaths;
use std::path::Path;
use std::process::Command;


use super::*;

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
        Stage { name: "test-verb-protocol", run: test_verb_protocol },
        Stage { name: "gryth-examples-corpus", run: gryth_examples_corpus },
    ]
}

/// The GR2 exit: two fixture workspaces in different scopes BUILD concurrently —
/// real `build` wire calls racing into two daemons.
pub(crate) fn two_scopes_concurrent(ctx: &StageCtx) -> Result<(), String> {
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
                    let resp = razel_daemon::rpc::call(
                        &s,
                        &razel_daemon::rpc::req_build(&["widget".into()], "."),
                    )
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
pub(crate) fn build_parity(ctx: &StageCtx) -> Result<(), String> {
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
            "razel binary missing at {} — build it (cargo build -p razel); parity needs both CLIs",
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

pub(crate) const RUN_SRC: &str = "#include <stdio.h>\nint main(void){ printf(\"hello-from-grazel-run\\n\"); return 7; }\n";

/// The §4b ordering guarantees, asserted THROUGH grazeld (the GR3 point — the
/// stream crosses the scope socket and the member router): id-before-events,
/// gap-free per-invocation seq, progress strictly before the terminal result.
pub(crate) fn build_streamed(ctx: &StageCtx) -> Result<(), String> {
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
pub(crate) fn run_verb(ctx: &StageCtx) -> Result<(), String> {
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

/// GR4's full claim: a WS subscriber sees the SAME event byte-sequence a UDS
/// subscriber sees — payload-identical, only the framing differs by transport.
pub(crate) fn ws_stream_equivalence(ctx: &StageCtx) -> Result<(), String> {
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
pub(crate) fn js_client_roundtrip(ctx: &StageCtx) -> Result<(), String> {
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
pub(crate) fn shutdown_verb(ctx: &StageCtx) -> Result<(), String> {
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
pub(crate) fn no_daemon_escape(ctx: &StageCtx) -> Result<(), String> {
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

/// Last subscriber out closes the shared upstream (refcount → zero → key gone).
pub(crate) fn view_idle_closes_upstream(ctx: &StageCtx) -> Result<(), String> {
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

