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
use razel_wire::{
    BuildState, BuildStatus, ImpactSet, encode,
};
use std::process::ExitCode;

// C3: ONE command-line parser, shared by the CLI + daemon (razel_loading::args, re-exported via
// razel-build). The CLI only wraps it to map the library's String parse error onto an ExitCode
// (parse_opts/parse_opts_with_rc below); Opts + its fields + global_flags + default_socket + rc-lite
// all come from the shared module, so the CLI and the daemon parse identically.
use razel_build::args::default_socket;


use crate::*;

pub(crate) fn print_impact(i: &ImpactSet) {
    println!(
        "razel: {} source{} → {} target{}, {} test{}",
        i.sources.len(),
        plural(i.sources.len()),
        i.targets.len(),
        plural(i.targets.len()),
        i.tests.len(),
        plural(i.tests.len()),
    );
    for t in &i.targets {
        println!("  target  {}", t.label);
    }
    for t in &i.tests {
        println!("  test    {}", t.label);
    }
}

pub(crate) fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

pub(crate) fn cmd_subscribe(args: &[String]) -> ExitCode {
    let o = match parse_opts(args) {
        Ok(o) => o,
        Err(c) => return c,
    };
    let socket = o.socket.unwrap_or_else(|| default_socket(&o.workspace));
    let mut stream = match rpc::subscribe(&socket) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "razel subscribe: cannot reach daemon at {} ({e})",
                socket.display()
            );
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "razel: subscribed to build-graph state on {} (Ctrl-C to stop)",
        socket.display()
    );
    loop {
        let frame = match rpc::next_frame(&mut stream) {
            Ok(f) => f,
            Err(_) => {
                eprintln!("razel: subscription closed");
                return ExitCode::SUCCESS;
            }
        };
        match rpc::payload(&frame) {
            Ok(p) if o.cbor => println!("{}", hex(&encode(&p))),
            Ok(p) => print_build_state(&BuildState::from_cbor(&p)),
            Err(e) => {
                eprintln!("razel subscribe: daemon error: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
}

pub(crate) fn print_build_state(s: &BuildState) {
    println!(
        "razel: build-graph @ rev {} ({} target{})",
        s.revision,
        s.targets.len(),
        plural(s.targets.len())
    );
    for t in &s.targets {
        let status = match t.status {
            BuildStatus::Cached => "cached",
            BuildStatus::Built => "built",
            BuildStatus::Failed => "failed",
        };
        println!("  {status:<7} {}", t.label);
    }
}

