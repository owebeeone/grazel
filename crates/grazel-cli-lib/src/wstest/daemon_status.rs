//! `grazel ws test` — the workstream's acceptance instrument (GrazelWorkstream §1).
//!
//! Stages exercise the REAL binary end-to-end (CLI → scope resolution → socket →
//! daemon → wire → back); a stage that could pass against a mock is wrongly
//! written. The same ladder runs as the shipped verb and in-process under
//! `cargo test` (grazel-cli's integration test) — only `StageCtx.grazel_bin`
//! differs (`current_exe` vs `CARGO_BIN_EXE_grazel`).

use crate::paths::ScopePaths;
use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;


use super::*;

/// Poll until `f` holds or patience runs out.
pub(crate) fn eventually(what: &str, mut f: impl FnMut() -> bool) -> Result<(), String> {
    for _ in 0..100 {
        if f() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("timed out waiting for {what}"))
}

/// An executable fixture: cc-compiled C printing a marker and exiting 7.
pub(crate) const RUN_BUILD: &str = r#"
def _impl(ctx):
    out = ctx.attr.name
    ctx.actions.run(executable="/usr/bin/cc", outputs=[out], inputs=[ctx.attr.src],
                    arguments=[ctx.attr.src, "-o", out])
    return [DefaultInfo(files=[out])]
cc_bin = rule(implementation=_impl, attrs={"src":1})
cc_bin(name="hello", src="hello.c")
"#;
/// Stage-side WS client, masked frames per RFC (the server requires masking).
pub(crate) struct WsClient(pub(crate) std::net::TcpStream);

impl WsClient {
    pub(crate) fn connect(port: u16) -> Result<Self, String> {
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

    pub(crate) fn send(&mut self, payload: &[u8]) -> Result<(), String> {
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

    pub(crate) fn recv(&mut self) -> Result<Vec<u8>, String> {
        crate::ws::read_message(&mut self.0)
            .map_err(|e| e.to_string())?
            .ok_or("server closed the stream".into())
    }

    /// RFC close frame (masked, empty payload), then drop the TCP side.
    pub(crate) fn close(&mut self) {
        use std::io::Write;
        let _ = self.0.write_all(&[0x88, 0x80, 0, 0, 0, 0]);
        let _ = self.0.flush();
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

/// Survey P2: read-only observability — live, dead-stale, and empty scopes all
/// render without error (and without any wire call).
pub(crate) fn daemon_status(ctx: &StageCtx) -> Result<(), String> {
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

// --- inbox 0010: grazel test + the gryth-examples corpus -----------------------

pub(crate) fn fanout_count(home: &Path, scope: &str, key: &str) -> Result<Option<u32>, String> {
    Ok(crate::daemon::status_lines(&ScopePaths::new(home, scope)?)
        .iter()
        .find_map(|l| l.strip_prefix(&format!("fanout={key}:")).and_then(|n| n.parse().ok())))
}

/// Two WS subscribers, same key ⇒ ONE shared upstream, identical byte streams.
pub(crate) fn view_fanout_dedup(ctx: &StageCtx) -> Result<(), String> {
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
        razel_daemon::rpc::call(&socket, &razel_daemon::rpc::req_build(&["hello".into()], "."))
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
pub(crate) fn view_resync_after_drop(ctx: &StageCtx) -> Result<(), String> {
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

/// `"http_port":N` out of daemon.json.
pub(crate) fn http_port_of(home: &Path, scope: &str) -> Result<u16, String> {
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
pub(crate) fn http_post(port: u16, path: &str, body: &[u8]) -> Result<(u16, Vec<u8>), String> {
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
pub(crate) fn http_equivalence(ctx: &StageCtx) -> Result<(), String> {
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

/// With OPT-IN `--idle-timeout`, an idle daemon times out and cleans up after
/// itself. (Default grazeld runs indefinitely — the long-lived scope service.)
pub(crate) fn idle_out(ctx: &StageCtx) -> Result<(), String> {
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

/// Two scopes ⇒ two daemons on two sockets, both answering the hello — the §1e
/// isolation unit, live. DISTINCT workspaces per scope: one scope per workspace
/// is the rule the output-base lock enforces (GR2). SIGKILL cleanup is fine here.
pub(crate) fn two_scopes_hello(ctx: &StageCtx) -> Result<(), String> {
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
