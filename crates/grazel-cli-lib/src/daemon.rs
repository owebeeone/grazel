//! grazeld = `grazel` in daemon mode (§1d one-binary rule), riding the
//! razel-daemon LIB (the allowed dependency direction; GrazelWorkstream GR1).
//!
//! This round: bind the per-scope socket, record daemon.json, answer the wire.
//! Hello v0 is the existing `version` method (build version + wire protocol);
//! the §1e hello with the WORKSPACE ROOT needs a new taut message — seam-requested
//! razel-side, consumed here when it lands. Autostart-on-dial, version handshake,
//! stale-socket recovery and idle-out are the rest of GR1, not this round.

use crate::paths::ScopePaths;
use razel_daemon::rpc;
use razel_wire::VersionInfo;
use std::path::Path;
use std::time::Duration;

pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Create the scope dirs (sockets dir user-only), write daemon.json, serve.
/// Blocks for the daemon's lifetime.
pub fn run(paths: &ScopePaths, workspace: &Path) -> Result<(), String> {
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
    // daemon.json is a filesystem artifact, not protocol (taut governs the wire,
    // §0); tiny enough to write by hand — the workspace has no JSON dep by policy.
    let json = format!(
        "{{\"pid\":{},\"version\":\"{}\",\"protocol\":{}}}\n",
        std::process::id(),
        BUILD_VERSION,
        rpc::PROTOCOL
    );
    std::fs::write(&paths.daemon_json, json)
        .map_err(|e| format!("{}: {e}", paths.daemon_json.display()))?;

    let server = rpc::Server::new(workspace.to_path_buf(), paths.state_dir.join("cache"));
    server
        .serve(&paths.socket)
        .map_err(|e| format!("serve {}: {e}", paths.socket.display()))
}

/// One hello round-trip: dial the scope socket, ask `version`.
pub fn dial_version(socket: &Path) -> Result<VersionInfo, String> {
    let resp = rpc::call(socket, &rpc::req_version()).map_err(|e| e.to_string())?;
    Ok(VersionInfo::from_cbor(&rpc::payload(&resp)?))
}

/// Dial with retries — covers the daemon's startup window in autospawn callers
/// and ws-test stages. Total patience ≈ `attempts × interval`.
pub fn wait_dial_version(
    socket: &Path,
    attempts: u32,
    interval: Duration,
) -> Result<VersionInfo, String> {
    let mut last = String::new();
    for _ in 0..attempts {
        match dial_version(socket) {
            Ok(v) => return Ok(v),
            Err(e) => last = e,
        }
        std::thread::sleep(interval);
    }
    Err(format!("daemon at {} never answered: {last}", socket.display()))
}
