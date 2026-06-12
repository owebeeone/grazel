//! grazeld = `grazel` in daemon mode (§1d one-binary rule), riding the
//! razel-daemon LIB (the allowed dependency direction; GR1 + GR2).
//!
//! A scope daemon serves a workspace COLLECTION (GrazelScopes.md): a member map
//! `root → handle`, each handle a `razel_daemon::rpc::Server` bound to that root
//! with engine state in the workspace's own `.razel-cache/` (§1b: output bases
//! are not keyed by distribution or scope). Members are PINNED (scope.rc,
//! opened at start, never idle) or DYNAMIC (opened by hello, idle out on the
//! member sweep). The daemon itself runs indefinitely unless `--idle-timeout`.
//!
//! Envelope: 4-byte BE length prefix + CBOR map `{1: method, 2: args}` request,
//! `{1: ok, 2: payload, 3: error}` response — razel-daemon's, verbatim.
//! `build.subscribe` through grazeld is debt D2 until GR3 wires streaming.

use crate::paths::ScopePaths;
use razel_daemon::{outlock, rpc, transport};
use razel_wire::{Cbor, Hello, VersionInfo, decode, encode};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The wire protocol this daemon ADVERTISES in its hello. The env override is a
/// TEST SEAM (ws-test `version-handshake` plants a mismatched daemon with it);
/// autostart scrubs it from spawned daemons, so it cannot leak past a test.
pub fn advertised_protocol() -> i64 {
    std::env::var("GRAZEL_FAKE_WIRE_PROTOCOL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(rpc::PROTOCOL)
}

pub struct ServeOpts {
    pub paths: ScopePaths,
    /// Exit after this long with no connection activity. None (the default) =
    /// grazeld runs indefinitely — it's the long-lived scope service (debt D8).
    pub idle_timeout: Option<Duration>,
    /// Close DYNAMIC members idle this long. Memberships idle; the daemon doesn't.
    pub member_idle_timeout: Duration,
    /// `--http-bind` override; must be loopback (GR4: localhost-only). None =
    /// `127.0.0.1:0`, the chosen port recorded in daemon.json.
    pub http_bind: Option<String>,
}

// --- framing (the razel-daemon envelope; its helpers are private) ------------

fn write_frame(stream: &mut dyn Write, bytes: &[u8]) -> std::io::Result<()> {
    let len = u32::try_from(bytes.len())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "frame too large"))?;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(bytes)?;
    stream.flush()
}

fn read_frame(stream: &mut dyn Read) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len)?;
    let mut buf = vec![0u8; u32::from_be_bytes(len) as usize];
    stream.read_exact(&mut buf)?;
    Ok(buf)
}

fn ok(payload: Cbor) -> Cbor {
    Cbor::Map(vec![(1, Cbor::Bool(true)), (2, payload)])
}

fn err(msg: &str) -> Cbor {
    Cbor::Map(vec![(1, Cbor::Bool(false)), (3, Cbor::Text(msg.into()))])
}

/// FNV-1a 64 — member filenames (short, stable, no extra dep).
fn digest16(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

// --- members ------------------------------------------------------------------

struct Member {
    /// Arc: stream connections borrow the server long-lived, outside the map lock.
    server: Arc<rpc::Server>,
    pinned: bool,
    last_used: Instant,
    /// The §1b writer claim — RAII from razel-daemon (the ONE impl of the
    /// contract this lane proposed in inbox 0005); releases on drop, pid-checked.
    _lock: outlock::OutLock,
}

pub(crate) struct State {
    paths: ScopePaths,
    advertised: i64,
    active: AtomicUsize,
    last_done: Mutex<Instant>,
    members: Mutex<HashMap<PathBuf, Member>>,
}

impl State {
    fn member_file(&self, root: &Path) -> PathBuf {
        self.paths
            .state_dir
            .join("members")
            .join(digest16(&root.display().to_string()))
    }

    /// Open (or touch) a member: output-base lock first — the §1b single-writer
    /// claim — then the handle and the observability file (GrazelScopes.md).
    fn open_member(&self, root: &Path, pinned: bool) -> Result<(), String> {
        let root = root
            .canonicalize()
            .map_err(|e| format!("workspace {}: {e}", root.display()))?;
        let mut members = self.members.lock().unwrap();
        if let Some(m) = members.get_mut(&root) {
            m.last_used = Instant::now();
            return Ok(());
        }
        let lock = outlock::acquire(&root, "grazeld", &self.paths.scope)?;
        let mfile = self.member_file(&root);
        std::fs::create_dir_all(mfile.parent().unwrap()).map_err(|e| e.to_string())?;
        let kind = if pinned { "pinned" } else { "dynamic" };
        std::fs::write(&mfile, format!("{kind} {}\n", root.display())).map_err(|e| e.to_string())?;
        members.insert(
            root.clone(),
            Member {
                server: Arc::new(rpc::Server::new(root.clone(), root.join(".razel-cache"))),
                pinned,
                last_used: Instant::now(),
                _lock: lock,
            },
        );
        Ok(())
    }

    fn close_member(&self, members: &mut HashMap<PathBuf, Member>, root: &Path) {
        members.remove(root); // Member drop releases the outlock
        let _ = std::fs::remove_file(self.member_file(root));
    }

    /// Close dynamic members idle past `timeout`.
    fn sweep_members(&self, timeout: Duration) {
        let mut members = self.members.lock().unwrap();
        let idle: Vec<PathBuf> = members
            .iter()
            .filter(|(_, m)| !m.pinned && m.last_used.elapsed() > timeout)
            .map(|(root, _)| root.clone())
            .collect();
        for root in idle {
            self.close_member(&mut members, &root);
        }
    }

    /// Remove every claim and rendezvous artifact, then exit — the one way out
    /// (shutdown verb + idle watchdog), so a live socket always means a live pid.
    fn cleanup_and_exit(&self) -> ! {
        let mut members = self.members.lock().unwrap();
        let roots: Vec<PathBuf> = members.keys().cloned().collect();
        for root in roots {
            self.close_member(&mut members, &root);
        }
        let _ = std::fs::remove_file(&self.paths.socket);
        let _ = std::fs::remove_file(&self.paths.daemon_json);
        std::process::exit(0)
    }

    /// One-shot dispatch, TRANSPORT-AGNOSTIC — UDS frames and HTTP bodies carry
    /// the same request map and get the same response envelope, byte-for-byte
    /// (GR4's transport-equivalence contract). `.1` = shut down after replying.
    pub(crate) fn respond(&self, req: &Cbor) -> (Cbor, bool) {
        let Cbor::Text(method) = req.get(1) else {
            return (err("malformed request: missing method"), false);
        };
        match method.as_str() {
            "hello" => {
                let hello = Hello::from_cbor(req.get(2));
                // Protocol check FIRST: a mismatched client is about to restart
                // us (§1e) — it must not cause lock churn. The client compares.
                if hello.protocol == self.advertised
                    && let Err(e) = self.open_member(Path::new(&hello.workspace_root), false)
                {
                    return (err(&e), false);
                }
                let v = VersionInfo {
                    version: BUILD_VERSION.to_string(),
                    protocol: self.advertised,
                };
                (ok(v.to_cbor()), false)
            }
            "shutdown" => (ok(Cbor::Null), true),
            "build.subscribe" => (err("build.subscribe via grazeld arrives with GR3"), false),
            _ => {
                // Interim routing (debt D9): the wire's requests don't carry a
                // workspace until GR3's invocation envelope — route to the sole
                // member, refuse ambiguity rather than guess.
                let mut members = self.members.lock().unwrap();
                let resp = match members.len() {
                    0 => err("no workspace member: hello first"),
                    1 => {
                        let m = members.values_mut().next().expect("len checked");
                        m.last_used = Instant::now();
                        m.server.dispatch(req)
                    }
                    n => err(&format!(
                        "scope {:?} serves {n} workspaces; per-invocation routing arrives with GR3",
                        self.paths.scope
                    )),
                };
                (resp, false)
            }
        }
    }

    /// The stream-routing target (debt D9: sole member until GR3's successor
    /// gives requests workspace identity). Shared by UDS, WS, and HTTP paths.
    pub(crate) fn sole_member_server(&self) -> Result<Arc<rpc::Server>, String> {
        let members = self.members.lock().unwrap();
        match members.len() {
            0 => Err("no workspace member: hello first".into()),
            1 => Ok(members.values().next().expect("len checked").server.clone()),
            n => Err(format!(
                "scope {:?} serves {n} workspaces; per-invocation routing arrives with GR3",
                self.paths.scope
            )),
        }
    }

    /// Track a serviced request for the idle watchdog — both transports count.
    pub(crate) fn enter(&self) {
        self.active.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn leave(&self) {
        *self.last_done.lock().unwrap() = Instant::now();
        self.active.fetch_sub(1, Ordering::SeqCst);
    }

    pub(crate) fn exit_now(&self) -> ! {
        self.cleanup_and_exit()
    }

    fn handle(&self, conn: &mut dyn transport::Conn) -> std::io::Result<()> {
        let raw = read_frame(conn)?;
        let req = decode(&raw);
        if let Cbor::Text(method) = req.get(1)
            && matches!(method.as_str(), "invocation.events" | "build.subscribe")
        {
            // STREAM methods: hand the whole connection to the member's server
            // (`serve_conn`, the inbox-0006 seam). It reads the first frame
            // itself, so replay the one we consumed — deterministic CBOR makes
            // the replayed bytes identical to what the client sent.
            return match self.sole_member_server() {
                Ok(server) => {
                    let mut replay = ReplayConn::new(&raw, conn);
                    server.serve_conn(&mut replay)
                }
                Err(e) => write_frame(conn, &encode(&err(&e))),
            };
        }
        let (resp, shutdown) = self.respond(&req);
        write_frame(conn, &encode(&resp))?;
        if shutdown {
            self.cleanup_and_exit()
        }
        Ok(())
    }
}

/// A connection whose first frame has already been read by the router: replays
/// those bytes (length prefix + payload) before passing through to the inner
/// connection. Lets `Server::serve_conn` re-read the routed request verbatim.
struct ReplayConn<'a> {
    head: std::io::Cursor<Vec<u8>>,
    inner: &'a mut dyn transport::Conn,
}

impl<'a> ReplayConn<'a> {
    fn new(frame_payload: &[u8], inner: &'a mut dyn transport::Conn) -> Self {
        let mut head = (frame_payload.len() as u32).to_be_bytes().to_vec();
        head.extend_from_slice(frame_payload);
        Self { head: std::io::Cursor::new(head), inner }
    }
}

impl Read for ReplayConn<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.head.read(buf)?;
        if n > 0 { Ok(n) } else { self.inner.read(buf) }
    }
}

impl Write for ReplayConn<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Create the scope dirs, claim pinned workspaces, record daemon.json, serve.
/// Blocks for the daemon's lifetime; exits the PROCESS on shutdown/idle-out.
pub fn run(opts: ServeOpts) -> Result<(), String> {
    let paths = &opts.paths;
    let uds_dir = paths.socket.parent().expect("socket has a parent dir");
    std::fs::create_dir_all(uds_dir).map_err(|e| format!("{}: {e}", uds_dir.display()))?;
    std::fs::create_dir_all(&paths.state_dir)
        .map_err(|e| format!("{}: {e}", paths.state_dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(uds_dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("{}: {e}", uds_dir.display()))?;
    }
    let advertised = advertised_protocol();
    // The HTTP edge binds BEFORE daemon.json so the chosen port is in the record
    // from the first byte (GR4: per-scope localhost listener, port published).
    let (http_listener, http_port) = crate::http::bind(opts.http_bind.as_deref())?;
    // daemon.json is a filesystem artifact, not protocol (taut governs the wire,
    // §0); tiny enough to write by hand — the workspace has no JSON dep by policy.
    let json = format!(
        "{{\"pid\":{},\"version\":\"{}\",\"protocol\":{},\"http_port\":{http_port}}}\n",
        std::process::id(),
        BUILD_VERSION,
        advertised
    );
    std::fs::write(&paths.daemon_json, json)
        .map_err(|e| format!("{}: {e}", paths.daemon_json.display()))?;

    let state = Arc::new(State {
        paths: paths.clone(),
        advertised,
        active: AtomicUsize::new(0),
        last_done: Mutex::new(Instant::now()),
        members: Mutex::new(HashMap::new()),
    });

    // Pinned workspaces (scope.rc `pinned=` lines): claimed AT START, loud on
    // failure — a scope whose pin is held elsewhere must not come up half-bound.
    let scope_rc = paths.state_dir.join("scope.rc");
    if let Ok(text) = std::fs::read_to_string(&scope_rc) {
        for line in text.lines().map(str::trim) {
            if let Some(root) = line.strip_prefix("pinned=") {
                state
                    .open_member(Path::new(root.trim()), true)
                    .map_err(|e| format!("pinned workspace: {e}"))?;
            }
        }
    }

    let listener =
        transport::bind(&paths.socket).map_err(|e| format!("bind {}: {e}", paths.socket.display()))?;

    {
        let state = state.clone();
        std::thread::spawn(move || crate::http::serve(http_listener, state));
    }
    {
        let state = state.clone();
        let member_idle = opts.member_idle_timeout;
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(200));
            state.sweep_members(member_idle);
        });
    }
    if let Some(timeout) = opts.idle_timeout {
        let state = state.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(200));
            let idle_since = *state.last_done.lock().unwrap();
            if state.active.load(Ordering::SeqCst) == 0 && idle_since.elapsed() > timeout {
                state.cleanup_and_exit();
            }
        });
    }

    loop {
        let mut conn = listener
            .accept()
            .map_err(|e| format!("accept on {}: {e}", paths.socket.display()))?;
        let state = state.clone();
        state.active.fetch_add(1, Ordering::SeqCst);
        std::thread::spawn(move || {
            let _ = state.handle(conn.as_mut());
            *state.last_done.lock().unwrap() = Instant::now();
            state.active.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

/// The dial-side hello: build/wire versions + WORKSPACE ROOT (§1e).
pub fn req_hello(workspace_root: &Path) -> Cbor {
    let hello = Hello {
        build_version: BUILD_VERSION.to_string(),
        protocol: rpc::PROTOCOL,
        workspace_root: workspace_root.display().to_string(),
    };
    Cbor::Map(vec![(1, Cbor::Text("hello".into())), (2, hello.to_cbor())])
}

pub fn req_shutdown() -> Cbor {
    Cbor::Map(vec![(1, Cbor::Text("shutdown".into()))])
}
