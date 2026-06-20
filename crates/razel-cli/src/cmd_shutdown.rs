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
use std::process::ExitCode;

// C3: ONE command-line parser, shared by the CLI + daemon (razel_loading::args, re-exported via
// razel-build). The CLI only wraps it to map the library's String parse error onto an ExitCode
// (parse_opts/parse_opts_with_rc below); Opts + its fields + global_flags + default_socket + rc-lite
// all come from the shared module, so the CLI and the daemon parse identically.
use razel_build::args::default_socket;


use crate::*;

/// `razel shutdown` (bazel `shutdown`): tell the workspace's daemon to terminate. Never auto-spawns
/// (that would be absurd); a missing daemon is a no-op success.
pub(crate) fn cmd_shutdown(args: &[String]) -> ExitCode {
    let o = match parse_opts(args) {
        Ok(o) => o,
        Err(c) => return c,
    };
    let socket = o.socket.unwrap_or_else(|| default_socket(&o.workspace));
    match rpc::call(&socket, &rpc::req_shutdown()) {
        // The daemon acks, then exits; we may get the ack or a clean disconnect — both mean "down".
        Ok(resp) => match rpc::payload(&resp) {
            Ok(_) => {
                eprintln!("razel: daemon shut down ({})", socket.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("razel: daemon error: {e}");
                ExitCode::FAILURE
            }
        },
        Err(_) => {
            eprintln!("razel: no daemon running at {}", socket.display());
            ExitCode::SUCCESS
        }
    }
}

