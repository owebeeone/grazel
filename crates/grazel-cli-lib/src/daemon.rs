//! grazeld = `grazel` in daemon mode (§1d one-binary rule), riding the
//! razel-daemon LIB (the allowed dependency direction; GrazelWorkstream GR1).
//!
//! The serve loop is grazel's own (hello / shutdown / idle-out are scope-daemon
//! concerns razel doesn't have); everything razel-shaped (`version`, `build`,
//! `affected`) delegates to `razel_daemon::rpc::Server::dispatch`, same envelope:
//! 4-byte BE length prefix + CBOR map `{1: method, 2: args}` per request,
//! `{1: ok, 2: payload, 3: error}` per response. `build.subscribe` through
//! grazeld is a recorded debt until GR3 wires streaming over the scope socket.

use crate::paths::ScopePaths;
use razel_daemon::{rpc, transport};
use razel_wire::{Cbor, Hello, VersionInfo, decode, encode};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
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
    pub workspace: PathBuf,
    /// Exit after this long with no connection activity. None = no idle-out.
    pub idle_timeout: Option<Duration>,
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

// --- the daemon ---------------------------------------------------------------

struct State {
    paths: ScopePaths,
    inner: rpc::Server,
    advertised: i64,
    active: AtomicUsize,
    last_done: Mutex<Instant>,
}

impl State {
    /// Remove the rendezvous artifacts and exit — the one way out, shared by
    /// `shutdown` and the idle watchdog, so a live socket always means a live pid.
    fn cleanup_and_exit(&self) -> ! {
        let _ = std::fs::remove_file(&self.paths.socket);
        let _ = std::fs::remove_file(&self.paths.daemon_json);
        std::process::exit(0)
    }

    fn handle(&self, conn: &mut dyn transport::Conn) -> std::io::Result<()> {
        let req = decode(&read_frame(conn)?);
        let Cbor::Text(method) = req.get(1) else {
            return write_frame(conn, &encode(&err("malformed request: missing method")));
        };
        match method.as_str() {
            "hello" => {
                let _hello = Hello::from_cbor(req.get(2)); // workspace_root: GR2 routes on it
                let v = VersionInfo {
                    version: BUILD_VERSION.to_string(),
                    protocol: self.advertised,
                };
                write_frame(conn, &encode(&ok(v.to_cbor())))
            }
            "shutdown" => {
                write_frame(conn, &encode(&ok(Cbor::Null)))?;
                self.cleanup_and_exit()
            }
            "build.subscribe" => write_frame(
                conn,
                &encode(&err("build.subscribe via grazeld arrives with GR3")),
            ),
            _ => write_frame(conn, &encode(&self.inner.dispatch(&req))),
        }
    }
}

/// Create the scope dirs (sockets dir user-only), record daemon.json, serve.
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
    // daemon.json is a filesystem artifact, not protocol (taut governs the wire,
    // §0); tiny enough to write by hand — the workspace has no JSON dep by policy.
    let json = format!(
        "{{\"pid\":{},\"version\":\"{}\",\"protocol\":{}}}\n",
        std::process::id(),
        BUILD_VERSION,
        advertised
    );
    std::fs::write(&paths.daemon_json, json)
        .map_err(|e| format!("{}: {e}", paths.daemon_json.display()))?;

    let listener =
        transport::bind(&paths.socket).map_err(|e| format!("bind {}: {e}", paths.socket.display()))?;
    let state = Arc::new(State {
        paths: paths.clone(),
        inner: rpc::Server::new(opts.workspace, paths.state_dir.join("cache")),
        advertised,
        active: AtomicUsize::new(0),
        last_done: Mutex::new(Instant::now()),
    });

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
