//! T1 (PublicSurfaces §6): the FIRST service-contract transcripts — scripted
//! request/response/stream sequences against a real daemon over a real socket.
//! Covers S3c: the `hello` handshake (good / protocol-mismatch / wrong-root) and the
//! `run` invocation envelope (id first, gap-free seq, progress strictly before the
//! terminal result — the §4b guarantees GR3's build-streamed stage asserts).

use razel_daemon::rpc::{self, Server};
use razel_wire::{Hello, InvocationEvent, InvocationStarted, VersionInfo};
use std::path::PathBuf;

fn start_daemon(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("razel-t1-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let ws = root.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(
        ws.join("BUILD"),
        "genrule(name = \"g\", outs = [\"g.txt\"], cmd = \"echo hi > $@\")\n",
    )
    .unwrap();
    let socket = root.join("daemon.sock");
    let server = Server::new(ws.clone(), root.join("cache"));
    let s2 = socket.clone();
    std::thread::spawn(move || {
        let _ = server.serve(&s2);
    });
    // Wait for the socket to accept (bounded).
    for _ in 0..100 {
        if rpc::call(&socket, &rpc::req_version()).is_ok() {
            return (ws, socket);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("daemon never came up on {}", socket.display());
}

#[test]
fn hello_handshake_accepts_and_rejects() {
    let (ws, socket) = start_daemon("hello");
    // Good hello → the daemon's VersionInfo.
    let h = Hello {
        build_version: "t1".into(),
        protocol: 1,
        workspace_root: ws.display().to_string(),
    };
    let resp = rpc::call(&socket, &rpc::req_hello(&h)).unwrap();
    let v = VersionInfo::from_cbor(&rpc::payload(&resp).expect("good hello accepted"));
    assert_eq!(v.protocol, 1);
    // Wire-protocol mismatch → loud, names both versions (the restart signal).
    let bad = Hello { protocol: 999, ..h.clone() };
    let resp = rpc::call(&socket, &rpc::req_hello(&bad)).unwrap();
    let e = rpc::payload(&resp).expect_err("protocol mismatch must be rejected");
    assert!(e.contains("999") && e.contains("protocol"), "{e}");
    // Wrong workspace root → loud, names the served root (§1e discrimination).
    let wrong = Hello { workspace_root: "/nonexistent/elsewhere".into(), ..h };
    let resp = rpc::call(&socket, &rpc::req_hello(&wrong)).unwrap();
    let e = rpc::payload(&resp).expect_err("wrong root must be rejected");
    assert!(e.contains("workspace"), "{e}");
}

#[test]
fn run_id_first_then_gap_free_events_then_terminal_result() {
    let (_ws, socket) = start_daemon("run");
    // Subscribe the invocation log BEFORE running (a subscriber sees history from 0,
    // so order in the log — not connection timing — is what's asserted).
    let mut stream = rpc::invocation_events(&socket).expect("events stream");
    // run → the id, immediately.
    let resp = rpc::call(&socket, &rpc::req_run("g", &[])).unwrap();
    let started = InvocationStarted::from_cbor(&rpc::payload(&resp).expect("run accepted"));
    assert!(!started.invocation_id.is_empty(), "id returned first");
    // Collect this invocation's events until its terminal result.
    let mut seqs = Vec::new();
    let mut progress_count = 0usize;
    let mut result = None;
    while result.is_none() {
        let ev = InvocationEvent::from_cbor(
            &rpc::payload(&rpc::next_frame(&mut stream).expect("frame")).expect("event"),
        );
        if ev.invocation_id != started.invocation_id {
            continue; // another invocation's traffic — ours is filtered by id
        }
        assert!(
            result.is_none(),
            "no events after the terminal result (terminal closes the invocation)"
        );
        seqs.push(ev.seq);
        if ev.progress.is_some() {
            progress_count += 1;
            assert!(ev.result.is_none(), "exactly one arm set");
        }
        if ev.result.is_some() {
            result = ev.result;
        }
    }
    // Gap-free per-invocation ordering from 1.
    let expect: Vec<i64> = (1..=seqs.len() as i64).collect();
    assert_eq!(seqs, expect, "seq is gap-free in log order");
    assert!(progress_count >= 1, "at least one progress event precedes the result");
    let r = result.unwrap();
    assert_eq!(r.target, "g");
    assert!(
        !matches!(r.status, razel_wire::BuildStatus::Failed),
        "fixture build succeeds: {:?}",
        r.message
    );
}
