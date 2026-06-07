//! `razel` — the command-line interface to the razel build engine.
//!
//! A consumer of the build driver (`razel_build::build_target`) that reports
//! results as the `razel-wire` contract types (`BuildResult`, `VersionInfo`).
//! Runs the build **in-process** by default, or routes to a running daemon with
//! `--daemon` — the daemon serves the *same* wire types over UDS/CBOR, so the
//! two paths are byte-identical. `--cbor` emits the exact wire bytes.
//!
//!   razel build <target> [-C <dir>] [--cache <dir>] [--daemon] [--socket <s>] [--cbor]
//!   razel version [--daemon] [--socket <s>] [--cbor]
//!   razel daemon [-C <dir>] [--cache <dir>] [--socket <s>]
//!
//! Scope: single-package `BUILD`, exec_root = the workspace dir. The daemon does
//! **cold** builds today; warm/incremental reuse + streaming surfaces are next.

use razel_build::build_target;
use razel_core::Digest;
use razel_daemon::rpc::{self, Server};
use razel_exec::Cache;
use razel_wire::{BuildResult, BuildStatus, OutputArtifact, VersionInfo, encode};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Wire protocol revision reported by `version` (bumped on breaking IR changes).
const PROTOCOL: i64 = 1;
/// sysexits EX_USAGE — bad invocation (vs. EX failure for a real build error).
const EX_USAGE: u8 = 64;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("build") => cmd_build(&args[1..]),
        Some("version") | Some("-V") | Some("--version") => cmd_version(&args[1..]),
        Some("daemon") => cmd_daemon(&args[1..]),
        Some("-h") | Some("--help") | None => {
            print_usage();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("razel: unknown command {other:?}\n");
            print_usage();
            ExitCode::from(EX_USAGE)
        }
    }
}

fn print_usage() {
    eprint!(
        "razel — build engine CLI

USAGE:
  razel build <target> [-C <dir>] [--cache <dir>] [--daemon] [--socket <s>] [--cbor]
  razel version [--daemon] [--socket <s>] [--cbor]
  razel daemon [-C <dir>] [--cache <dir>] [--socket <s>]

  <target>          name, :name, or //pkg:name (single-package BUILD)
  -C, --workspace   workspace dir with BUILD + sources (default: .)
  --cache <dir>     content-addressed cache dir (default: <workspace>/.razel-cache)
  --daemon          route the request to a running `razel daemon` over UDS
  --socket <s>      daemon socket path (default: <workspace>/.razel-daemon.sock)
  --cbor            print the result as taut-wire CBOR (hex) instead of text
"
    );
}

/// Parsed flags shared across subcommands.
struct Opts {
    workspace: PathBuf,
    cache: Option<PathBuf>,
    socket: Option<PathBuf>,
    daemon: bool,
    cbor: bool,
    positionals: Vec<String>,
}

fn parse_opts(args: &[String]) -> Result<Opts, ExitCode> {
    let mut o = Opts {
        workspace: PathBuf::from("."),
        cache: None,
        socket: None,
        daemon: false,
        cbor: false,
        positionals: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-C" | "--workspace" => o.workspace = PathBuf::from(value(args, &mut i, "-C")?),
            "--cache" => o.cache = Some(PathBuf::from(value(args, &mut i, "--cache")?)),
            "--socket" => o.socket = Some(PathBuf::from(value(args, &mut i, "--socket")?)),
            "--daemon" => o.daemon = true,
            "--cbor" => o.cbor = true,
            s if s.starts_with('-') => {
                eprintln!("razel: unknown flag {s:?}");
                return Err(ExitCode::from(EX_USAGE));
            }
            s => o.positionals.push(s.to_string()),
        }
        i += 1;
    }
    Ok(o)
}

fn value(args: &[String], i: &mut usize, flag: &str) -> Result<String, ExitCode> {
    match args.get(*i + 1) {
        Some(v) => {
            *i += 1;
            Ok(v.clone())
        }
        None => {
            eprintln!("razel: {flag} requires a value");
            Err(ExitCode::from(EX_USAGE))
        }
    }
}

fn default_socket(workspace: &Path) -> PathBuf {
    workspace.join(".razel-daemon.sock")
}

fn cmd_version(args: &[String]) -> ExitCode {
    let o = match parse_opts(args) {
        Ok(o) => o,
        Err(c) => return c,
    };
    let info = if o.daemon {
        let socket = o.socket.unwrap_or_else(|| default_socket(&o.workspace));
        match daemon_call(&socket, &rpc::req_version()) {
            Ok(p) => VersionInfo::from_cbor(&p),
            Err(c) => return c,
        }
    } else {
        VersionInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol: PROTOCOL,
        }
    };
    if o.cbor {
        println!("{}", hex(&encode(&info.to_cbor())));
    } else {
        println!("razel {} (wire protocol {})", info.version, info.protocol);
    }
    ExitCode::SUCCESS
}

fn cmd_build(args: &[String]) -> ExitCode {
    let o = match parse_opts(args) {
        Ok(o) => o,
        Err(c) => return c,
    };
    if o.positionals.len() != 1 {
        eprintln!("razel build: expected exactly one <target>");
        return ExitCode::from(EX_USAGE);
    }
    let target_arg = o.positionals[0].clone();

    let result = if o.daemon {
        let socket = o
            .socket
            .clone()
            .unwrap_or_else(|| default_socket(&o.workspace));
        match daemon_call(&socket, &rpc::req_build(&target_arg)) {
            Ok(p) => BuildResult::from_cbor(&p),
            Err(c) => return c,
        }
    } else {
        match local_build(&o, &target_arg) {
            Ok(r) => r,
            Err(c) => return c,
        }
    };

    if o.cbor {
        println!("{}", hex(&encode(&result.to_cbor())));
    } else {
        print_build_result(&result);
    }
    match result.status {
        BuildStatus::Failed => ExitCode::FAILURE,
        _ => ExitCode::SUCCESS,
    }
}

fn cmd_daemon(args: &[String]) -> ExitCode {
    let o = match parse_opts(args) {
        Ok(o) => o,
        Err(c) => return c,
    };
    let socket = o.socket.unwrap_or_else(|| default_socket(&o.workspace));
    let cache = o.cache.unwrap_or_else(|| o.workspace.join(".razel-cache"));
    eprintln!(
        "razel daemon: serving {} on {}",
        o.workspace.display(),
        socket.display()
    );
    match Server::new(o.workspace, cache).serve(&socket) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("razel daemon: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Run a build in-process; maps the driver's outcome onto the wire contract.
fn local_build(o: &Opts, target_arg: &str) -> Result<BuildResult, ExitCode> {
    // Accept name | :name | //pkg:name — build the bare name (single-package BUILD).
    let name = target_arg
        .rsplit(':')
        .next()
        .unwrap_or(target_arg)
        .to_string();

    let Some(build_path) = ["BUILD", "BUILD.bazel"]
        .iter()
        .map(|f| o.workspace.join(f))
        .find(|p| p.exists())
    else {
        eprintln!(
            "razel build: no BUILD or BUILD.bazel in {}",
            o.workspace.display()
        );
        return Err(ExitCode::FAILURE);
    };
    let build_src = std::fs::read_to_string(&build_path).map_err(|e| {
        eprintln!("razel build: cannot read {}: {e}", build_path.display());
        ExitCode::FAILURE
    })?;

    let cache_path = o
        .cache
        .clone()
        .unwrap_or_else(|| o.workspace.join(".razel-cache"));
    let cache = Cache::new(&cache_path).map_err(|e| {
        eprintln!(
            "razel build: cannot open cache {}: {e}",
            cache_path.display()
        );
        ExitCode::FAILURE
    })?;

    Ok(
        match build_target(&build_src, &name, &o.workspace, &cache) {
            Ok(produced) => BuildResult {
                target: target_arg.to_string(),
                status: BuildStatus::Built,
                recomputes: 0, // cold one-shot build; recomputes is the warm daemon's metric
                outputs: produced
                    .iter()
                    .map(|p| OutputArtifact {
                        path: p.clone(),
                        digest: digest_of(&o.workspace.join(p)),
                    })
                    .collect(),
                message: None,
            },
            Err(e) => BuildResult {
                target: target_arg.to_string(),
                status: BuildStatus::Failed,
                recomputes: 0,
                outputs: vec![],
                message: Some(e),
            },
        },
    )
}

/// One request/response to the daemon; unwraps the payload or prints the error.
fn daemon_call(socket: &Path, req: &razel_wire::Cbor) -> Result<razel_wire::Cbor, ExitCode> {
    let resp = rpc::call(socket, req).map_err(|e| {
        eprintln!("razel: cannot reach daemon at {} ({e})", socket.display());
        eprintln!(
            "  start one with: razel daemon --socket {}",
            socket.display()
        );
        ExitCode::FAILURE
    })?;
    rpc::payload(&resp).map_err(|e| {
        eprintln!("razel: daemon error: {e}");
        ExitCode::FAILURE
    })
}

fn digest_of(path: &Path) -> Vec<u8> {
    match std::fs::read(path) {
        Ok(bytes) => Digest::of(&bytes).as_bytes().to_vec(),
        Err(_) => vec![], // output not on disk (e.g. restored elsewhere) — empty digest
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn print_build_result(r: &BuildResult) {
    if let BuildStatus::Failed = r.status {
        eprintln!("razel: build of {} FAILED", r.target);
        if let Some(m) = &r.message {
            eprintln!("  {m}");
        }
        return;
    }
    let n = r.outputs.len();
    println!(
        "razel: built {} ({n} output{})",
        r.target,
        if n == 1 { "" } else { "s" }
    );
    for o in &r.outputs {
        let h = hex(&o.digest);
        let short = &h[..h.len().min(12)];
        println!("  {short}  {}", o.path);
    }
}
