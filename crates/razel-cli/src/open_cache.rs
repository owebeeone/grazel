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

use razel_exec::Cache;
use std::process::ExitCode;

// C3: ONE command-line parser, shared by the CLI + daemon (razel_loading::args, re-exported via
// razel-build). The CLI only wraps it to map the library's String parse error onto an ExitCode
// (parse_opts/parse_opts_with_rc below); Opts + its fields + global_flags + default_socket + rc-lite
// all come from the shared module, so the CLI and the daemon parse identically.
use razel_build::args::Opts;



/// Run a build in-process; maps the driver's outcome onto the wire contract.
///
/// A `//pkg:name` label builds through the **multi-package workspace** loader
/// (cross-package deps load on demand); a bare `name`/`:name` builds the workspace's
/// own `BUILD` single-package. Both honor the global cc flags (`-c`/`--copt`/…).
/// Open the workspace's content-addressed cache (`--disk_cache` or `<ws>/.razel-cache`).
pub(crate) fn open_cache(o: &Opts) -> Result<Cache, ExitCode> {
    let cache_path = o
        .cache
        .clone()
        .unwrap_or_else(|| o.workspace.join(".razel-cache"));
    Cache::new(&cache_path).map_err(|e| {
        eprintln!("razel build: cannot open cache {}: {e}", cache_path.display());
        ExitCode::FAILURE
    })
}

