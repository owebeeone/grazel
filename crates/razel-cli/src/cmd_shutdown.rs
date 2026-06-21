//! `razel` — the command-line interface to the razel build engine.
//!
//! A consumer of the build driver (`razel_build::build_target`) that reports
//! results as the `razel-wire` contract types (`BuildResult`, `VersionInfo`).
//! Runs the build **in-process** by default, or routes to a running daemon with
//! `--daemon` — the daemon serves the *same* wire types over UDS/CBOR, so the
//! two paths are byte-identical. `--cbor` emits the exact wire bytes.
//!
//!   razel build <target> [-C <dir>] [--disk_cache <dir>] [--daemon] [--socket <s>] [--cbor]
//!   razel version [--daemon] [--socket <s>] [--cbor]
//!   razel daemon [-C <dir>] [--disk_cache <dir>] [--socket <s>]
//!
//! The command line is **Bazel-syntax**: every Bazel flag (the generated
//! `bazel_flags` table) is recognized and parsed; the handful razel honors take
//! effect (see `HANDLERS`), language flags are silently accepted, and the rest are
//! recognized-but-diagnosed. razel's own flags (`-C`/`--daemon`/`--socket`/`--cbor`)
//! have no Bazel equivalent and stay.
//!
//! A `//pkg:name` target builds through the multi-package workspace loader
//! (cross-package deps load on demand from `-C <root>`); a bare `name` builds the
//! workspace's own `BUILD` single-package. exec_root = the workspace dir. The daemon
//! does **cold** builds today; warm/incremental reuse + streaming surfaces are next.

use razel_daemon::rpc::{self};
use std::path::Path;
use std::process::ExitCode;

// C3: ONE command-line parser, shared by the CLI + daemon (razel_loading::args, re-exported via
// razel-build). The CLI only wraps it to map the library's String parse error onto an ExitCode
// (parse_opts/parse_opts_with_rc below); Opts + its fields + global_flags + default_socket + rc-lite
// all come from the shared module, so the CLI and the daemon parse identically.
use razel_build::args::default_socket;


use crate::*;

/// Outcome of [`stop_daemon`].
pub(crate) enum StopOutcome {
    /// No daemon was bound for this workspace.
    NotRunning,
    /// A daemon was running and is now down (graceful `shutdown`, or killed by pid).
    Stopped,
    /// A daemon is still up — couldn't confirm it's down.
    Failed,
}

/// Stop the workspace daemon at `socket`, robustly. Ask it to `shutdown` (a current daemon acks and
/// exits); if it's too old to know the verb — exactly the stale-daemon skew this guards against — or
/// won't die, kill it by the pid in the workspace writer lock (`.razel-cache/workspace.lock`, which
/// the daemon holds for its whole life). Unlinks the leftover socket so the next spawn binds clean.
/// The shared stop primitive behind `shutdown`, `clean`, and the auto-restart in `ensure_daemon`.
pub(crate) fn stop_daemon(socket: &Path, workspace: &Path) -> StopOutcome {
    use std::time::Duration;
    // Liveness over the RPC transport: a daemon answers `version`, a dead/missing one errors at
    // connect. Portable (no unix-socket assumption) AND immune to a zombie pid that `kill -0` would
    // still report alive — the daemon's exit unbinds the socket, which is what we actually need.
    let alive = || rpc::call(socket, &rpc::req_version()).is_ok();
    if !alive() {
        return StopOutcome::NotRunning;
    }
    let wait_down = || {
        for _ in 0..160 {
            if !alive() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    };
    // Graceful first: a daemon new enough to know the verb acks `shutdown` then exits.
    let _ = rpc::call(socket, &rpc::req_shutdown());
    let mut down = wait_down();
    // Still answering → a daemon too old to know `shutdown` (the legacy skew this guards against);
    // kill it by pid. The next daemon can then reap the orphaned lock (its holder pid is now dead).
    if !down {
        kill_holder(workspace);
        down = wait_down();
    }
    if !down {
        return StopOutcome::Failed;
    }
    let _ = std::fs::remove_file(socket); // a unix socket file lingers after its listener dies
    StopOutcome::Stopped
}

/// Kill the daemon recorded in `workspace`'s writer lock by pid — the fallback for a daemon too old
/// to answer `shutdown`. Process signalling is unix-only; on other platforms graceful `shutdown` is
/// the only portable stop, so this is a no-op there and the caller reports `Failed`.
#[cfg(unix)]
fn kill_holder(workspace: &Path) {
    if let Some(pid) = razel_daemon::outlock::holder_pid(workspace) {
        razel_daemon::outlock::terminate(pid);
    }
}
#[cfg(not(unix))]
fn kill_holder(_workspace: &Path) {}

/// `razel shutdown` (bazel `shutdown`): stop the workspace's daemon. Never auto-spawns; a missing
/// daemon is a no-op success. Robust against a stale daemon that predates the `shutdown` verb —
/// it's stopped by pid — which is the exact situation this mechanism exists to clear.
pub(crate) fn cmd_shutdown(args: &[String]) -> ExitCode {
    let o = match parse_opts(args) {
        Ok(o) => o,
        Err(c) => return c,
    };
    let socket = o.socket.unwrap_or_else(|| default_socket(&o.workspace));
    match stop_daemon(&socket, &o.workspace) {
        StopOutcome::NotRunning => {
            eprintln!("razel: no daemon running at {}", socket.display());
            ExitCode::SUCCESS
        }
        StopOutcome::Stopped => {
            eprintln!("razel: daemon stopped ({})", socket.display());
            ExitCode::SUCCESS
        }
        StopOutcome::Failed => {
            eprintln!("razel: could not stop the daemon at {}", socket.display());
            ExitCode::FAILURE
        }
    }
}

