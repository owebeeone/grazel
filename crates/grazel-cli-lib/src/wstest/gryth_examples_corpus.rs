//! `grazel ws test` — the workstream's acceptance instrument (GrazelWorkstream §1).
//!
//! Stages exercise the REAL binary end-to-end (CLI → scope resolution → socket →
//! daemon → wire → back); a stage that could pass against a mock is wrongly
//! written. The same ladder runs as the shipped verb and in-process under
//! `cargo test` (grazel-cli's integration test) — only `StageCtx.grazel_bin`
//! differs (`current_exe` vs `CARGO_BIN_EXE_grazel`).

use std::path::Path;
use std::process::{Command, Stdio};


use super::*;

pub(crate) fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(from).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let dst = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &dst)?;
        } else {
            std::fs::copy(entry.path(), &dst).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// The committed corpus (clients/gryth-examples), end-to-end through grazel:
/// build via the daemon, test via the protocol, run via --no_daemon (the
/// daemon-routed run is inbox 0011's pending red test). Copied to scratch —
/// the corpus stays pristine.
pub(crate) fn gryth_examples_corpus(ctx: &StageCtx) -> Result<(), String> {
    let corpus = ctx
        .grazel_bin
        .ancestors()
        .find(|p| p.join("clients/gryth-examples/README.md").is_file())
        .map(|p| p.join("clients/gryth-examples"))
        .ok_or("clients/gryth-examples not found above the grazel binary (dev tree required)")?;
    let (home, work) = (ctx.tmp.join("home"), ctx.tmp.join("corpus"));
    copy_tree(&corpus, &work)?;
    let result = (|| {
        // 01: builds through the scope daemon.
        let ws1 = work.join("01-hello-http");
        let ws1_abs = ws1.canonicalize().map_err(|e| e.to_string())?.display().to_string();
        let out = Command::new(&ctx.grazel_bin)
            .current_dir(&ws1)
            .env("GRAZEL_HOME", &home)
            .env_remove("GRAZEL_SCOPE")
            .args(["build", "//:server", "--scope=gx"])
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!(
                "corpus 01 build failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        // Switching writers: the gx daemon holds the §1b workspace lock from the
        // build; release it (the shutdown verb, in its natural habitat) before
        // the local run takes its own claim.
        stop_scope(ctx, &home, &ws1, "gx");
        // 01: runs (--no_daemon until 0011 lands), serves one real request.
        let mut child = Command::new(&ctx.grazel_bin)
            .current_dir(&ws1)
            .env("GRAZEL_HOME", &home)
            .env("GRYTH_EXAMPLE_ONESHOT", "1")
            .args(["run", "//:server", "--no_daemon", "-C", &ws1_abs])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let port = {
            use std::io::{BufRead, BufReader, Read};
            let stdout = child.stdout.take().ok_or("no child stdout")?;
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next() {
                    Some(Ok(line)) => {
                        if let Some(p) = line.strip_prefix("listening port=") {
                            break p.parse::<u16>().map_err(|e| e.to_string())?;
                        }
                    }
                    _ => {
                        let mut err = String::new();
                        if let Some(mut se) = child.stderr.take() {
                            let _ = se.read_to_string(&mut err);
                        }
                        let _ = child.wait();
                        return Err(format!("server exited before announcing its port: {err}"));
                    }
                }
            }
        };
        let body = {
            use std::io::{Read, Write};
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port))
                .map_err(|e| e.to_string())?;
            s.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .map_err(|e| e.to_string())?;
            let mut resp = String::new();
            s.read_to_string(&mut resp).map_err(|e| e.to_string())?;
            resp
        };
        let status = child.wait().map_err(|e| e.to_string())?; // ONESHOT: exits after the request
        if !status.success() || !body.contains("hello from gryth-examples") {
            return Err(format!("corpus 01 run: exit {status}, body: {body:?}"));
        }
        // 02: the js_test suite, both targets PASSED, exit 0.
        let ws2 = work.join("02-js-tests");
        let ws2_abs = ws2.canonicalize().map_err(|e| e.to_string())?.display().to_string();
        let out = Command::new(&ctx.grazel_bin)
            .current_dir(&ws2)
            .env("GRAZEL_HOME", &home)
            .args(["test", "//:math", "//:strings", "-C", &ws2_abs])
            .output()
            .map_err(|e| e.to_string())?;
        // razel's PASSED summary is on STDERR (Bazel-style); check both streams.
        let stdout = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if out.status.code() != Some(0) || !stdout.contains("PASSED") {
            return Err(format!(
                "corpus 02 test: exit {:?}, output: {stdout}",
                out.status.code()
            ));
        }
        Ok(())
    })();
    let ws1 = work.join("01-hello-http");
    stop_scope(ctx, &home, &ws1, "gx");
    result
}

// --- View seam (GrazelViewSeam.md): fan-out machinery over real streams --------

