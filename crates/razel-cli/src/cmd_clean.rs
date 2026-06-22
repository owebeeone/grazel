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

use std::process::ExitCode;

// C3: ONE command-line parser, shared by the CLI + daemon (razel_loading::args, re-exported via
// razel-build). The CLI only wraps it to map the library's String parse error onto an ExitCode
// (parse_opts/parse_opts_with_rc below); Opts + its fields + global_flags + default_socket + rc-lite
// all come from the shared module, so the CLI and the daemon parse identically.
use razel_build::args::{bazel_build_compat_env, default_socket};
use razel_daemon::rpc;


use crate::*;

/// `razel clean` (Bazel `clean`): remove razel's BUILD OUTPUTS for this workspace — the exec-root
/// forest (`.razel-exec/`), the output tree (`razel-out/`), and the Bazel-style convenience symlinks
/// (`razel-bin`, `razel-testlogs`), just as `bazel clean` wipes `bazel-out` + its `bazel-*` symlinks.
/// The content-addressed cache (`.razel-cache/`) and fetched external deps (`.razel-crates/`) are
/// KEPT, so the next build re-materializes outputs from cache near-instantly (bazel-with-disk-cache
/// style) and the warm daemon REUSES its analysis rather than reloading cold (no ~8s clean stall).
/// Under `--bazel_build_compat` (or the env var) razel wrote into Bazel's tree, so the `bazel-out` /
/// `bazel-bin` / `bazel-testlogs` set is removed too. `--expunge` ADDITIONALLY wipes `.razel-cache`
/// + `.razel-crates` and tells the daemon to drop its analysis (a genuine cold rebuild next).
/// Symlinks are unlinked (never followed); `--async` runs synchronously. Idempotent (nothing present
/// = success), like Bazel; the summary goes to stderr (Bazel stream discipline).
pub(crate) fn cmd_clean(args: &[String]) -> ExitCode {
    let o = match parse_opts(args) {
        Ok(o) => o,
        Err(c) => return c,
    };
    let compat = o.bazel_build_compat || bazel_build_compat_env();
    // Tell the warm daemon to re-validate. A plain clean invalidates only EXECUTION: the next build
    // re-materializes outputs from the kept content cache but REUSES the warm analysis — no cold
    // reload, so clean stays snappy. `--expunge` drops the analysis too (the on-disk cache is wiped
    // below, so it genuinely must re-analyze). We do NOT kill the daemon (clean ≠ shutdown; bazel
    // keeps its server too). Best-effort: no daemon → no-op.
    let socket = o.socket.clone().unwrap_or_else(|| default_socket(&o.workspace));
    let _ = rpc::call(&socket, &rpc::req_clean(o.expunge));
    // The REAL output storage is the exec-root forest `.razel-exec` (external-crate builds);
    // `razel-out`/`razel-bin`/`razel-testlogs` are convenience SYMLINKS into it, so removing only
    // those leaves the actual outputs behind (and the warm daemon keeps serving them). Remove
    // `.razel-exec` too so a clean is a REAL clean. The content-addressed cache `.razel-cache` is
    // KEPT (the next build re-materializes from it near-instantly, bazel-with-disk-cache style) along
    // with the fetched external deps `.razel-crates` — both are dropped only by `--expunge`.
    let mut names: Vec<&str> = vec![
        ".razel-exec",
        "razel-out",
        "razel-bin",
        "razel-testlogs",
    ];
    if compat {
        names.extend(["bazel-out", "bazel-bin", "bazel-testlogs"]);
    }
    if o.expunge {
        names.push(".razel-cache");
        names.push(".razel-crates");
    }
    let how = if o.expunge { "expunged" } else { "cleaned" };
    let (mut removed, mut errs) = (Vec::new(), 0u32);
    for name in &names {
        let p = o.workspace.join(name);
        // symlink_metadata: classify WITHOUT following — a convenience symlink is unlinked,
        // its target (already covered as its own entry) is not chased.
        let meta = match std::fs::symlink_metadata(&p) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                eprintln!("razel clean: {}: {e}", p.display());
                errs += 1;
                continue;
            }
        };
        let r = if meta.file_type().is_symlink() || meta.is_file() {
            std::fs::remove_file(&p)
        } else {
            std::fs::remove_dir_all(&p)
        };
        match r {
            Ok(()) => removed.push(*name),
            Err(e) => {
                eprintln!("razel clean: {}: {e}", p.display());
                errs += 1;
            }
        }
    }
    if errs > 0 {
        return ExitCode::FAILURE;
    }
    if removed.is_empty() {
        eprintln!("razel: nothing to clean");
    } else {
        eprintln!("razel: {how} ({})", removed.join(", "));
    }
    ExitCode::SUCCESS
}

