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

use crate::Progress;
use razel_daemon::rpc::{self};
use razel_wire::{
    BuildResult, InvocationEvent,
};
use std::path::Path;

// C3: ONE command-line parser, shared by the CLI + daemon (razel_loading::args, re-exported via
// razel-build). The CLI only wraps it to map the library's String parse error onto an ExitCode
// (parse_opts/parse_opts_with_rc below); Opts + its fields + global_flags + default_socket + rc-lite
// all come from the shared module, so the CLI and the daemon parse identically.



/// Run a build through the daemon's `build.stream`, printing each per-action progress line as it
/// arrives (WS-E.2 — so a daemon build isn't silent), and returning the terminal `BuildResult`.
/// `Err(())` means the daemon was UNREACHABLE or died mid-stream (connection/abnormal close) — the
/// caller falls back to an in-process build ("builds never break"). A real *build* failure is a
/// normal `Ok(BuildResult{Failed})`, not `Err`.
pub(crate) fn daemon_build_streamed(
    socket: &Path,
    args: &[String],
    cwd: &str,
    target: &str,
    cbor: bool,
    progress: &mut Progress,
) -> Result<BuildResult, ()> {
    // Run the stream loop, then ALWAYS erase the bar on the way out (terminal result, build failure,
    // or daemon-died) so the caller's summary / fallback lands on a clean line.
    let result = (|| {
        let mut stream = rpc::build_stream(socket, args, cwd).map_err(|_| ())?;
        loop {
            // An early/abnormal close (daemon crashed, actor died) or a protocol error → unreachable.
            let frame = rpc::next_frame(&mut stream).map_err(|_| ())?;
            let payload = rpc::payload(&frame).map_err(|_| ())?;
            let ev = InvocationEvent::from_cbor(&payload);
            if let Some(mut r) = ev.result {
                // A streamed Failed result carries no target; fill it so the message isn't "ERROR: :".
                if r.target.is_empty() {
                    r.target = target.to_string();
                }
                return Ok(r); // terminal frame
            }
            // Per-action progress snapshot → the bottom-pinned in-place block (bazel-style; never
            // scrolls). `detail` is the running-action descriptions, newline-joined. `--cbor` is
            // machine output, so the block is suppressed there.
            if !cbor
                && let Some(p) = ev.progress
            {
                let detail = p.detail.unwrap_or_default();
                let running: Vec<&str> = if detail.is_empty() {
                    Vec::new()
                } else {
                    detail.split('\n').collect()
                };
                progress.update(p.done, p.total, &running);
            }
        }
    })();
    progress.finish();
    result
}

