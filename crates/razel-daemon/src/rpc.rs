//! UDS + CBOR transport for the razel daemon.
//!
//! A length-prefixed CBOR envelope over a Unix domain socket: one request → one
//! response per connection. The dispatch runs the *same* `build_target` the CLI
//! uses and answers in the `razel-wire` contract types — so a daemon build and a
//! local build produce byte-identical results.
//!
//! This is the transport layer + real **cold** builds over it. Reusing the warm
//! incremental engine ([`crate::Workspace`]) so a daemon build recomputes only
//! the affected subgraph — and the streaming `build.subscribe`/`affected`
//! surfaces over a persistent connection — are the next arc. The envelope shape
//! is deliberately request/response only for now.
//!
//! Envelope (CBOR maps, integer tags):
//!   request  `{1: method:text, 2: args:cbor}`
//!   response `{1: ok:bool, 2: payload:cbor|null, 3: error:text|null}`

use razel_build::build_target;
use razel_core::Digest;
use razel_exec::Cache;
use razel_wire::{BuildResult, BuildStatus, Cbor, OutputArtifact, VersionInfo, decode, encode};
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// Wire protocol revision (kept in step with the CLI's `version`).
pub const PROTOCOL: i64 = 1;

// --- framing ----------------------------------------------------------------

fn write_frame(stream: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    let len = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame too large"))?;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(bytes)?;
    stream.flush()
}

fn read_frame(stream: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len)?;
    let n = u32::from_be_bytes(len) as usize;
    let mut buf = vec![0u8; n];
    stream.read_exact(&mut buf)?;
    Ok(buf)
}

// --- server -----------------------------------------------------------------

/// A daemon bound to one workspace + cache. Serves request/response over UDS.
pub struct Server {
    workspace: PathBuf,
    cache_dir: PathBuf,
}

impl Server {
    pub fn new(workspace: PathBuf, cache_dir: PathBuf) -> Self {
        Self {
            workspace,
            cache_dir,
        }
    }

    /// Bind `socket` (removing a stale file first) and serve until the listener
    /// errors. Blocks — run it in its own thread or process.
    pub fn serve(&self, socket: &Path) -> io::Result<()> {
        let _ = std::fs::remove_file(socket);
        let listener = UnixListener::bind(socket)?;
        for conn in listener.incoming() {
            let mut conn = conn?;
            if let Err(e) = self.handle_conn(&mut conn) {
                // Best-effort error frame; a dead connection's write just fails.
                let _ = write_frame(&mut conn, &encode(&err(&e.to_string())));
            }
        }
        Ok(())
    }

    fn handle_conn(&self, conn: &mut UnixStream) -> io::Result<()> {
        let req = decode(&read_frame(conn)?);
        let resp = self.dispatch(&req);
        write_frame(conn, &encode(&resp))
    }

    /// Route one request envelope to its handler and produce a response envelope.
    pub fn dispatch(&self, req: &Cbor) -> Cbor {
        let Cbor::Text(method) = req.get(1) else {
            return err("malformed request: missing method");
        };
        let args = req.get(2);
        match method.as_str() {
            "version" => ok(&version_info().to_cbor()),
            "build" => match self.do_build(args) {
                Ok(r) => ok(&r.to_cbor()),
                Err(e) => err(&e),
            },
            other => err(&format!("unknown method {other:?}")),
        }
    }

    fn do_build(&self, args: &Cbor) -> Result<BuildResult, String> {
        let Cbor::Text(target_arg) = args.get(1) else {
            return Err("build: missing target".into());
        };
        let target_arg = target_arg.clone();
        let name = target_arg
            .rsplit(':')
            .next()
            .unwrap_or(&target_arg)
            .to_string();

        let build_path = ["BUILD", "BUILD.bazel"]
            .iter()
            .map(|f| self.workspace.join(f))
            .find(|p| p.exists())
            .ok_or_else(|| format!("no BUILD in {}", self.workspace.display()))?;
        let build_src = std::fs::read_to_string(&build_path).map_err(|e| e.to_string())?;
        let cache = Cache::new(&self.cache_dir).map_err(|e| e.to_string())?;

        // Build success vs. action failure both yield a BuildResult (Built/Failed);
        // Err is reserved for protocol/IO problems (no BUILD, unreadable, …).
        Ok(
            match build_target(&build_src, &name, &self.workspace, &cache) {
                Ok(produced) => BuildResult {
                    target: target_arg,
                    status: BuildStatus::Built,
                    recomputes: 0, // cold build; warm-engine reuse is the next arc
                    outputs: produced
                        .iter()
                        .map(|p| OutputArtifact {
                            path: p.clone(),
                            digest: digest_of(&self.workspace.join(p)),
                        })
                        .collect(),
                    message: None,
                },
                Err(e) => BuildResult {
                    target: target_arg,
                    status: BuildStatus::Failed,
                    recomputes: 0,
                    outputs: vec![],
                    message: Some(e),
                },
            },
        )
    }
}

fn version_info() -> VersionInfo {
    VersionInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        protocol: PROTOCOL,
    }
}

fn ok(payload: &Cbor) -> Cbor {
    Cbor::Map(vec![
        (1, Cbor::Bool(true)),
        (2, payload.clone()),
        (3, Cbor::Null),
    ])
}

fn err(msg: &str) -> Cbor {
    Cbor::Map(vec![
        (1, Cbor::Bool(false)),
        (2, Cbor::Null),
        (3, Cbor::Text(msg.to_string())),
    ])
}

fn digest_of(path: &Path) -> Vec<u8> {
    std::fs::read(path)
        .map(|b| Digest::of(&b).as_bytes().to_vec())
        .unwrap_or_default()
}

// --- client -----------------------------------------------------------------

/// `version` request envelope.
pub fn req_version() -> Cbor {
    Cbor::Map(vec![(1, Cbor::Text("version".into())), (2, Cbor::Null)])
}

/// `build <target>` request envelope.
pub fn req_build(target: &str) -> Cbor {
    Cbor::Map(vec![
        (1, Cbor::Text("build".into())),
        (2, Cbor::Map(vec![(1, Cbor::Text(target.to_string()))])),
    ])
}

/// Send one request envelope to the daemon at `socket`; return its response.
pub fn call(socket: &Path, req: &Cbor) -> io::Result<Cbor> {
    let mut stream = UnixStream::connect(socket)?;
    write_frame(&mut stream, &encode(req))?;
    Ok(decode(&read_frame(&mut stream)?))
}

/// Unwrap a response envelope: the `payload` on success, the `error` text on
/// failure. (Protocol-level failure; a *failed build* is a `BuildResult` payload
/// with `status = Failed`, which this returns as `Ok`.)
pub fn payload(resp: &Cbor) -> Result<Cbor, String> {
    if matches!(resp.get(1), Cbor::Bool(true)) {
        Ok(resp.get(2).clone())
    } else {
        Err(match resp.get(3) {
            Cbor::Text(s) => s.clone(),
            _ => "unknown daemon error".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_version_in_process() {
        let srv = Server::new(PathBuf::from("."), std::env::temp_dir());
        let resp = srv.dispatch(&req_version());
        let v = VersionInfo::from_cbor(&payload(&resp).unwrap());
        assert_eq!(v.protocol, PROTOCOL);
        assert!(!v.version.is_empty());
    }

    #[test]
    fn dispatch_unknown_method_is_error_envelope() {
        let srv = Server::new(PathBuf::from("."), std::env::temp_dir());
        let resp = srv.dispatch(&Cbor::Map(vec![
            (1, Cbor::Text("nope".into())),
            (2, Cbor::Null),
        ]));
        assert!(payload(&resp).is_err());
    }

    #[test]
    fn dispatch_build_missing_build_file_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let srv = Server::new(dir.path().to_path_buf(), std::env::temp_dir());
        let resp = srv.dispatch(&req_build("widget"));
        assert!(payload(&resp).unwrap_err().contains("no BUILD"));
    }
}
