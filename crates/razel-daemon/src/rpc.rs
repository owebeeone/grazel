//! UDS + CBOR transport for the razel daemon.
//!
//! A length-prefixed CBOR envelope over a Unix domain socket: one request → one
//! response per connection. The dispatch runs the same driver the CLI uses and
//! answers in the `razel-wire` contract types — so a daemon build and a local
//! build produce byte-identical results.
//!
//! The daemon is **warm**: an unchanged BUILD is analyzed once and reused across
//! builds ([`Server::warm_analyze`]); action-level incrementality comes from the
//! content cache (`recomputes == 0` on a fully-cached rebuild). Served methods:
//! `version`, `build`, `affected`. The remaining streaming surface
//! (`build.subscribe`, an atom over a persistent connection) is the next arc; the
//! envelope is request/response for now.
//!
//! Envelope (CBOR maps, integer tags):
//!   request  `{1: method:text, 2: args:cbor}`
//!   response `{1: ok:bool, 2: payload:cbor|null, 3: error:text|null}`

use razel_build::{AnalyzedTarget, affected, analyze_build, execute};
use razel_core::Digest;
use razel_exec::Cache;
use razel_wire::{
    BuildResult, BuildStatus, Cbor, ImpactSet, OutputArtifact, TargetRef, VersionInfo, decode,
    encode,
};
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

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

/// Analysis cached in RAM, keyed by the BUILD file's content digest.
struct WarmAnalysis {
    build_digest: Digest,
    targets: Vec<AnalyzedTarget>,
}

/// A daemon bound to one workspace + cache. Serves request/response over UDS, and
/// keeps **warm** state: an unchanged BUILD is parsed/analyzed once, then reused
/// across builds (action-level incrementality still comes from the content cache).
pub struct Server {
    workspace: PathBuf,
    cache_dir: PathBuf,
    warm: Mutex<Option<WarmAnalysis>>,
    analyses: AtomicUsize,
}

impl Server {
    pub fn new(workspace: PathBuf, cache_dir: PathBuf) -> Self {
        Self {
            workspace,
            cache_dir,
            warm: Mutex::new(None),
            analyses: AtomicUsize::new(0),
        }
    }

    /// How many times analysis has actually run (cold + each BUILD change). Stays
    /// flat across rebuilds of an unchanged BUILD — the warm-reuse signal.
    pub fn analyses_run(&self) -> usize {
        self.analyses.load(Ordering::SeqCst)
    }

    /// Analyze `build_src`, reusing the warm cache when its content digest is
    /// unchanged. Returns the analyzed targets (cloned out so execution doesn't
    /// hold the lock).
    fn warm_analyze(&self, build_src: &str) -> Result<Vec<AnalyzedTarget>, String> {
        let digest = Digest::of(build_src.as_bytes());
        let mut warm = self.warm.lock().unwrap();
        if let Some(w) = warm.as_ref()
            && w.build_digest == digest
        {
            return Ok(w.targets.clone()); // warm hit — no re-analysis
        }
        let targets = analyze_build(build_src)?;
        self.analyses.fetch_add(1, Ordering::SeqCst);
        *warm = Some(WarmAnalysis {
            build_digest: digest,
            targets: targets.clone(),
        });
        Ok(targets)
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
            "affected" => match self.do_affected(args) {
                Ok(i) => ok(&i.to_cbor()),
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
        let targets = self.warm_analyze(&build_src)?;

        // Build success vs. action failure both yield a BuildResult (Built/Failed);
        // Err is reserved for protocol/IO problems (no BUILD, unreadable, …).
        Ok(match execute(&targets, &name, &self.workspace, &cache) {
            Ok(report) => BuildResult {
                target: target_arg,
                status: if report.executed == 0 {
                    BuildStatus::Cached
                } else {
                    BuildStatus::Built
                },
                recomputes: report.executed as i64,
                outputs: report
                    .produced
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
        })
    }

    fn do_affected(&self, args: &Cbor) -> Result<ImpactSet, String> {
        let Cbor::Array(items) = args else {
            return Err("affected: expected a files array".into());
        };
        let files: Vec<String> = items
            .iter()
            .filter_map(|c| match c {
                Cbor::Text(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        impact(&self.workspace, &files)
    }
}

/// The rdep impact of editing `files` in `workspace`, as the wire `ImpactSet`.
/// Shared by the daemon's `affected` method and the CLI's in-process path.
pub fn impact(workspace: &Path, files: &[String]) -> Result<ImpactSet, String> {
    let build_path = ["BUILD", "BUILD.bazel"]
        .iter()
        .map(|f| workspace.join(f))
        .find(|p| p.exists())
        .ok_or_else(|| format!("no BUILD in {}", workspace.display()))?;
    let build_src = std::fs::read_to_string(&build_path).map_err(|e| e.to_string())?;

    // Root package ("") — file ids are "/<path>", matching the query paths.
    let a = affected(&build_src, "", files)?;
    Ok(ImpactSet {
        sources: a.sources,
        targets: a.targets.iter().map(target_ref).collect(),
        tests: a.tests.iter().map(target_ref).collect(),
    })
}

/// Map the engine's coarse target kind onto the wire enum.
fn target_ref(a: &razel_build::AffectedTarget) -> TargetRef {
    use razel_wire::TargetKind as W;
    let kind = match a.kind {
        razel_ir::TargetKind::Library => W::Library,
        razel_ir::TargetKind::Binary => W::Binary,
        razel_ir::TargetKind::Test => W::Test,
    };
    TargetRef {
        label: a.label.clone(),
        kind,
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

/// `affected <files...>` request envelope.
pub fn req_affected(files: &[String]) -> Cbor {
    Cbor::Map(vec![
        (1, Cbor::Text("affected".into())),
        (
            2,
            Cbor::Array(files.iter().map(|f| Cbor::Text(f.clone())).collect()),
        ),
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
    fn warm_daemon_reuses_analysis_until_build_changes() {
        let ws = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        // A no-op action (no toolchain needed) so the test is cc-independent.
        let v1 = r#"
def _impl(ctx):
    ctx.actions.run(executable = "/usr/bin/true", outputs = [], inputs = [], arguments = [])
    return [DefaultInfo(files = [])]
noop = rule(implementation = _impl, attrs = {})
noop(name = "widget")
"#;
        std::fs::write(ws.path().join("BUILD"), v1).unwrap();
        let srv = Server::new(ws.path().to_path_buf(), cache.path().to_path_buf());

        let build = |s: &Server| payload(&s.dispatch(&req_build("widget"))).expect("build ok");

        // Two builds of the unchanged BUILD: analysis runs once, reused on the 2nd.
        build(&srv);
        build(&srv);
        assert_eq!(srv.analyses_run(), 1, "unchanged BUILD analyzed once");

        // Change the BUILD content → analysis re-runs.
        std::fs::write(
            ws.path().join("BUILD"),
            format!("{v1}\nnoop(name = \"extra\")\n"),
        )
        .unwrap();
        build(&srv);
        assert_eq!(srv.analyses_run(), 2, "changed BUILD re-analyzed");
    }

    #[test]
    fn dispatch_affected_walks_the_rdep_graph() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("BUILD"),
            r#"
def _impl(ctx):
    out = ctx.attr.name + ".o"
    ctx.actions.run(executable = "cc", outputs = [out], inputs = [ctx.attr.src], arguments = [])
    return [DefaultInfo(files = [out])]
thing = rule(implementation = _impl, attrs = {"src": 1})
thing(name = "widget", src = "widget.c")
thing(name = "widget_test", src = "widget.c")
"#,
        )
        .unwrap();
        let srv = Server::new(dir.path().to_path_buf(), std::env::temp_dir());
        let resp = srv.dispatch(&req_affected(&["widget.c".into()]));
        let impact = ImpactSet::from_cbor(&payload(&resp).unwrap());
        let labels = |v: &[TargetRef]| v.iter().map(|t| t.label.clone()).collect::<Vec<_>>();
        assert_eq!(labels(&impact.targets), vec!["//:widget"]);
        assert_eq!(labels(&impact.tests), vec!["//:widget_test"]);
    }

    #[test]
    fn dispatch_build_missing_build_file_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let srv = Server::new(dir.path().to_path_buf(), std::env::temp_dir());
        let resp = srv.dispatch(&req_build("widget"));
        assert!(payload(&resp).unwrap_err().contains("no BUILD"));
    }
}
