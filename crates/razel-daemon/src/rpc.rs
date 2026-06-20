//! UDS + CBOR transport for the razel daemon.
//!
//! A length-prefixed CBOR envelope over a Unix domain socket: one request → one
//! response per connection. The dispatch runs the same driver the CLI uses and
//! answers in the `razel-wire` contract types — so a daemon build and a local
//! build produce byte-identical results.
//!
//! The daemon is **warm**: an unchanged BUILD is analyzed once and reused across
//! builds; action-level incrementality comes from the content cache (`recomputes
//! == 0` on a fully-cached rebuild). Connections are handled per-thread.
//!
//! Methods: `version`, `build`, `affected` are unary (one request → one response);
//! `build.subscribe` is an **atom stream** — the connection stays open and the
//! daemon pushes the whole build-graph state (a `BuildState` frame) on connect and
//! again whenever a build advances the revision, until the client disconnects.
//!
//! Envelope (CBOR maps, integer tags):
//!   request  `{1: method:text, 2: args:cbor}`
//!   response `{1: ok:bool, 2: payload:cbor|null, 3: error:text|null}`  (one per frame)

use crate::actor::{ActorHandle, WorkspaceActor};
use crate::transport;
use razel_build::affected;
use razel_wire::{
    BuildResult, BuildState, BuildStatus, Cbor, Hello, ImpactSet, InvocationEvent,
    InvocationStarted, Progress, TargetRef, VersionInfo, decode, encode,
};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

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

/// Shared daemon state, behind an `Arc` so each connection runs on its own thread
/// (a long-lived `build.subscribe` stream must not block other clients).
struct Inner {
    workspace: PathBuf,
    /// The warm single-owner build actor (§3.5a). All builds flow through it; analysis +
    /// the incremental engine graph live there, not here.
    actor: ActorHandle,
    /// The live build-graph state (the `build.subscribe` atom); `revision` advances on every
    /// build, and `bump` wakes subscribers. Shared (`Arc`) with the actor, which commits the
    /// post-build snapshot on its own thread.
    state: Arc<Mutex<BuildState>>,
    bump: Arc<Condvar>,
    /// S3c: the invocation-event LOG (`invocation.events`, shape=log — ordered,
    /// append-only; subscribers replay from 0 then follow). Unbounded v1 — the
    /// bounded-buffer + drop-with-resync discipline arrives with the View work.
    events: Mutex<Vec<InvocationEvent>>,
    events_bump: Condvar,
    /// Invocation-id source (per-daemon monotonic).
    invocations: AtomicUsize,
}

/// A daemon bound to one workspace + cache. Warm (analysis reused across builds),
/// and a publisher of build-graph state to `build.subscribe` streams.
pub struct Server {
    inner: Arc<Inner>,
}

impl Server {
    pub fn new(workspace: PathBuf, cache_dir: PathBuf) -> Self {
        let state = Arc::new(Mutex::new(BuildState {
            revision: 0,
            targets: vec![],
        }));
        let bump = Arc::new(Condvar::new());
        // Spawn the warm actor up front (the `!Send` engine graph lives on its thread). It blocks
        // on its inbox until the first build; on `Server` drop the inbox `Sender` drops and the
        // actor thread exits cleanly.
        let actor = WorkspaceActor::spawn(workspace.clone(), cache_dir, state.clone(), bump.clone());
        Self {
            inner: Arc::new(Inner {
                workspace,
                actor,
                state,
                bump,
                events: Mutex::new(Vec::new()),
                events_bump: Condvar::new(),
                invocations: AtomicUsize::new(0),
            }),
        }
    }

    /// How many times analysis has actually run (cold + each BUILD change). Stays
    /// flat across rebuilds of an unchanged BUILD — the warm-reuse signal.
    pub fn analyses_run(&self) -> usize {
        self.inner.actor.analyses_run()
    }

    /// Route one request envelope and produce a response (the unary path; exposed
    /// for in-process tests).
    pub fn dispatch(&self, req: &Cbor) -> Cbor {
        self.inner.dispatch(req)
    }

    /// Serve ONE already-accepted connection: a unary request → one response, or a
    /// stream (`build.subscribe`/`invocation.events`) until the client disconnects.
    /// The HOST-DAEMON entry (seam contract, ws-razel/inbox/0006): grazeld accepts
    /// and routes by scope/membership, then hands the connection to the member
    /// workspace's server here.
    pub fn serve_conn<C: Read + Write>(&self, conn: &mut C) -> io::Result<()> {
        self.inner.handle_conn(conn)
    }

    /// Bind the rendezvous `socket` (UDS on unix, loopback TCP on Windows) and
    /// serve; each connection is handled on its own thread. Blocks.
    pub fn serve(&self, socket: &Path) -> io::Result<()> {
        // §1b: razeld is this workspace's WRITER for its lifetime (one writer per
        // workspace across daemons; a live grazeld holding it fails us loud).
        let _writer = crate::outlock::acquire(&self.inner.workspace, "razeld", "")
            .map_err(|e| io::Error::other(e))?;
        let listener = transport::bind(socket)?;
        loop {
            let mut conn = listener.accept()?;
            let inner = self.inner.clone();
            std::thread::spawn(move || {
                if let Err(e) = inner.handle_conn(&mut conn) {
                    // Best-effort error frame; a dead connection's write just fails.
                    let _ = write_frame(&mut conn, &encode(&err(&e.to_string())));
                }
            });
        }
    }
}

impl Inner {
    /// One connection: a `build.subscribe`/`invocation.events` request streams until
    /// the client disconnects; everything else is one request → one response.
    /// Generic over the byte stream — the transport decides the concrete type.
    fn handle_conn<C: Read + Write>(self: &Arc<Self>, conn: &mut C) -> io::Result<()> {
        let req = decode(&read_frame(conn)?);
        let Cbor::Text(method) = req.get(1) else {
            return write_frame(conn, &encode(&err("malformed request: missing method")));
        };
        match method.as_str() {
            "build.subscribe" => self.stream_build_state(conn),
            "invocation.events" => self.stream_invocation_events(conn),
            _ => {
                let resp = self.dispatch(&req);
                write_frame(conn, &encode(&resp))
            }
        }
    }

    /// `invocation.events` (log): replay the log from 0, then follow appends until
    /// the client disconnects. Order on the log IS the §4b ordering contract.
    fn stream_invocation_events<C: Write>(&self, conn: &mut C) -> io::Result<()> {
        let mut idx = 0usize;
        loop {
            let batch: Vec<InvocationEvent> = {
                let guard = self.events.lock().unwrap();
                let guard = self.events_bump.wait_while(guard, |e| e.len() == idx).unwrap();
                let batch = guard[idx..].to_vec();
                idx = guard.len();
                batch
            };
            for ev in &batch {
                write_frame(conn, &encode(&ok(&ev.to_cbor())))?; // Err == client gone
            }
        }
    }

    /// Append one event to the invocation log and wake followers.
    fn emit(&self, ev: InvocationEvent) {
        let mut log = self.events.lock().unwrap();
        log.push(ev);
        drop(log);
        self.events_bump.notify_all();
    }

    /// `hello` (§1e dial procedure): version handshake + workspace discrimination.
    fn do_hello(&self, args: &Cbor) -> Result<VersionInfo, String> {
        let h = Hello::from_cbor(args);
        let me = version_info();
        if h.protocol != me.protocol {
            return Err(format!(
                "wire protocol mismatch: client speaks {}, daemon speaks {} — restart the \
                 older side (client build {}, daemon build {})",
                h.protocol, me.protocol, h.build_version, me.version
            ));
        }
        let served = self.workspace.canonicalize().unwrap_or_else(|_| self.workspace.clone());
        let asked = std::path::Path::new(&h.workspace_root)
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from(&h.workspace_root));
        if served != asked {
            return Err(format!(
                "this daemon serves workspace {}, hello named {}",
                served.display(),
                asked.display()
            ));
        }
        Ok(me)
    }

    /// `run` (S3c, §4b stream-first): answer with the invocation id IMMEDIATELY;
    /// the build proceeds on its own thread, emitting Progress events then the
    /// terminal result onto the invocation log. v1 progress is phase-grained
    /// (load/execute); sched_hook-fed counts arrive with the View work.
    fn do_run(self: &Arc<Self>, args: &Cbor) -> Result<InvocationStarted, String> {
        let Cbor::Text(target) = args.get(1) else {
            return Err("run: missing target".into());
        };
        let target = target.clone();
        let n = self.invocations.fetch_add(1, Ordering::SeqCst) + 1;
        let id = format!("inv-{n}");
        let me = Arc::clone(self);
        let ev_id = id.clone();
        std::thread::spawn(move || {
            let mut seq = 0i64;
            let mut next = |progress, result| {
                seq += 1;
                InvocationEvent {
                    invocation_id: ev_id.clone(),
                    seq,
                    progress,
                    result,
                }
            };
            me.emit(next(
                Some(Progress {
                    invocation_id: ev_id.clone(),
                    phase: "load".into(),
                    done: 0,
                    total: 0,
                    detail: Some(target.clone()),
                }),
                None,
            ));
            // Synthesize a build through the warm actor (the same path `build` takes).
            let result = me
                .actor
                .build(vec![target.clone()], me.workspace.clone())
                .unwrap_or_else(|e| BuildResult {
                    target: target.clone(),
                    status: BuildStatus::Failed,
                    recomputes: 0,
                    outputs: vec![],
                    message: Some(e),
                });
            me.emit(next(None, Some(result)));
        });
        Ok(InvocationStarted { invocation_id: id })
    }

    /// `build.subscribe` (atom): send the current state, then a fresh snapshot each
    /// time a build advances the revision, until the client disconnects.
    fn stream_build_state<C: Write>(&self, conn: &mut C) -> io::Result<()> {
        let mut last = i64::MIN;
        loop {
            let snapshot = {
                let guard = self.state.lock().unwrap();
                // Park until the revision differs from what we last sent. wait_while
                // re-checks the predicate up front, so a build that fired between
                // frames is never missed.
                let guard = self.bump.wait_while(guard, |s| s.revision == last).unwrap();
                last = guard.revision;
                guard.clone()
            };
            write_frame(conn, &encode(&ok(&snapshot.to_cbor())))?; // Err == client gone → stop
        }
    }

    fn dispatch(self: &Arc<Self>, req: &Cbor) -> Cbor {
        let Cbor::Text(method) = req.get(1) else {
            return err("malformed request: missing method");
        };
        let args = req.get(2);
        match method.as_str() {
            "version" => ok(&version_info().to_cbor()),
            "hello" => match self.do_hello(args) {
                Ok(v) => ok(&v.to_cbor()),
                Err(e) => err(&e),
            },
            "build" => match self.do_build(args) {
                Ok(r) => ok(&r.to_cbor()),
                Err(e) => err(&e),
            },
            "run" => match self.do_run(args) {
                Ok(s) => ok(&s.to_cbor()),
                Err(e) => err(&e),
            },
            "affected" => match self.do_affected(args) {
                Ok(i) => ok(&i.to_cbor()),
                Err(e) => err(&e),
            },
            other => err(&format!("unknown method {other:?}")),
        }
    }

    /// Decode the C3 build envelope (`{1: args:[text], 2: cwd:text}`) and run it through the warm
    /// actor; the actor parses the args server-side (the one shared parser), drives the warm engine,
    /// and commits the post-build snapshot itself. A failed build is a `BuildResult{Failed}`; `Err`
    /// is reserved for protocol problems.
    fn do_build(&self, args: &Cbor) -> Result<BuildResult, String> {
        let Cbor::Array(items) = args.get(1) else {
            return Err("build: missing args".into());
        };
        let arg_tokens: Vec<String> = items
            .iter()
            .filter_map(|c| match c {
                Cbor::Text(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        let cwd = match args.get(2) {
            Cbor::Text(s) => PathBuf::from(s),
            _ => self.workspace.clone(),
        };
        self.actor.build(arg_tokens, cwd)
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

// --- client -----------------------------------------------------------------

/// `version` request envelope.
pub fn req_version() -> Cbor {
    Cbor::Map(vec![(1, Cbor::Text("version".into())), (2, Cbor::Null)])
}

/// `build` request envelope (C3): forward the raw client arg tokens + cwd; the daemon parses them
/// server-side with the one shared parser (so daemon == local == bazel parse identically).
pub fn req_build(args: &[String], cwd: &str) -> Cbor {
    Cbor::Map(vec![
        (1, Cbor::Text("build".into())),
        (
            2,
            Cbor::Map(vec![
                (
                    1,
                    Cbor::Array(args.iter().map(|a| Cbor::Text(a.clone())).collect()),
                ),
                (2, Cbor::Text(cwd.to_string())),
            ]),
        ),
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

/// `build.subscribe` request envelope (atom stream of build-graph state).
pub fn req_subscribe() -> Cbor {
    Cbor::Map(vec![
        (1, Cbor::Text("build.subscribe".into())),
        (2, Cbor::Null),
    ])
}

/// `hello` request envelope (§1e handshake).
pub fn req_hello(h: &Hello) -> Cbor {
    Cbor::Map(vec![(1, Cbor::Text("hello".into())), (2, h.to_cbor())])
}

/// `run <target> [args…]` request envelope (S3c stream-first command).
pub fn req_run(target: &str, args: &[String]) -> Cbor {
    Cbor::Map(vec![
        (1, Cbor::Text("run".into())),
        (
            2,
            Cbor::Map(vec![
                (1, Cbor::Text(target.to_string())),
                (2, Cbor::Array(args.iter().map(|a| Cbor::Text(a.clone())).collect())),
            ]),
        ),
    ])
}

/// `invocation.events` request envelope (the log subscription).
pub fn req_invocation_events() -> Cbor {
    Cbor::Map(vec![
        (1, Cbor::Text("invocation.events".into())),
        (2, Cbor::Null),
    ])
}

/// Open the `invocation.events` log stream (replays from 0, then follows). Read
/// frames with [`next_frame`]; each payload is an `InvocationEvent`.
pub fn invocation_events(socket: &Path) -> io::Result<Box<dyn transport::Conn>> {
    let mut conn = transport::connect(socket)?;
    write_frame(&mut conn, &encode(&req_invocation_events()))?;
    Ok(conn)
}

/// Send one request envelope to the daemon at `socket`; return its response.
pub fn call(socket: &Path, req: &Cbor) -> io::Result<Cbor> {
    let mut conn = transport::connect(socket)?;
    write_frame(&mut conn, &encode(req))?;
    Ok(decode(&read_frame(&mut conn)?))
}

/// Open a `build.subscribe` stream: returns the connection to read frames from
/// (each frame via [`next_frame`]). The initial frame is the current state.
pub fn subscribe(socket: &Path) -> io::Result<Box<dyn transport::Conn>> {
    let mut conn = transport::connect(socket)?;
    write_frame(&mut conn, &encode(&req_subscribe()))?;
    Ok(conn)
}

/// Read the next streamed frame (a response envelope) from a [`subscribe`] stream.
pub fn next_frame(stream: &mut impl Read) -> io::Result<Cbor> {
    Ok(decode(&read_frame(stream)?))
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

        let build =
            |s: &Server| payload(&s.dispatch(&req_build(&["widget".into()], "."))).expect("build ok");

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
        let resp = srv.dispatch(&req_build(&["widget".into()], "."));
        assert!(payload(&resp).unwrap_err().contains("no BUILD"));
    }
}
