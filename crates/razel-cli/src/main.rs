//! `razel` — the command-line interface to the razel build engine.
//!
//! A thin, in-process consumer of the build driver (`razel_build::build_target`)
//! that reports results as the `razel-wire` contract types (`BuildResult`,
//! `VersionInfo`). The daemon will serve those *same* types over UDS/CBOR; the
//! CLI proves the wire contract is the API surface, end to end, today —
//! `--cbor` emits the exact bytes the daemon would.
//!
//!   razel build <target> [-C <dir>] [--cache <dir>] [--cbor]
//!   razel version [--cbor]
//!
//! Scope: single-package `BUILD`, in-process (no daemon yet); exec_root = the
//! workspace dir, so outputs land beside sources. The daemon UDS server + a
//! client mode is the next increment — it transports these result types, it
//! does not change them.

use razel_build::build_target;
use razel_core::Digest;
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
  razel build <target> [-C <dir>] [--cache <dir>] [--cbor]
  razel version [--cbor]

build:
  <target>          target to build (name, :name, or //pkg:name; single-package BUILD)
  -C, --workspace   workspace dir with BUILD + sources (default: .)
  --cache <dir>     content-addressed cache dir (default: <workspace>/.razel-cache)
  --cbor            print the result as taut-wire CBOR (hex) instead of text
"
    );
}

fn cmd_version(args: &[String]) -> ExitCode {
    let info = VersionInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        protocol: PROTOCOL,
    };
    if args.iter().any(|a| a == "--cbor") {
        println!("{}", hex(&encode(&info.to_cbor())));
    } else {
        println!("razel {} (wire protocol {})", info.version, info.protocol);
    }
    ExitCode::SUCCESS
}

fn cmd_build(args: &[String]) -> ExitCode {
    let mut target: Option<String> = None;
    let mut workspace = PathBuf::from(".");
    let mut cache_dir: Option<PathBuf> = None;
    let mut cbor = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-C" | "--workspace" => match args.get(i + 1) {
                Some(v) => {
                    workspace = PathBuf::from(v);
                    i += 1;
                }
                None => return missing_value(a),
            },
            "--cache" => match args.get(i + 1) {
                Some(v) => {
                    cache_dir = Some(PathBuf::from(v));
                    i += 1;
                }
                None => return missing_value(a),
            },
            "--cbor" => cbor = true,
            s if s.starts_with('-') => {
                eprintln!("razel build: unknown flag {s:?}");
                return ExitCode::from(EX_USAGE);
            }
            s => {
                if target.is_some() {
                    eprintln!("razel build: more than one target given");
                    return ExitCode::from(EX_USAGE);
                }
                target = Some(s.to_string());
            }
        }
        i += 1;
    }

    let Some(target_arg) = target else {
        eprintln!("razel build: missing <target>");
        return ExitCode::from(EX_USAGE);
    };
    // Accept name | :name | //pkg:name — build the bare name (single-package BUILD).
    let name = target_arg
        .rsplit(':')
        .next()
        .unwrap_or(&target_arg)
        .to_string();

    let Some(build_path) = ["BUILD", "BUILD.bazel"]
        .iter()
        .map(|f| workspace.join(f))
        .find(|p| p.exists())
    else {
        eprintln!(
            "razel build: no BUILD or BUILD.bazel in {}",
            workspace.display()
        );
        return ExitCode::FAILURE;
    };
    let build_src = match std::fs::read_to_string(&build_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("razel build: cannot read {}: {e}", build_path.display());
            return ExitCode::FAILURE;
        }
    };

    let cache_path = cache_dir.unwrap_or_else(|| workspace.join(".razel-cache"));
    let cache = match Cache::new(&cache_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "razel build: cannot open cache {}: {e}",
                cache_path.display()
            );
            return ExitCode::FAILURE;
        }
    };

    // The driver returns produced output paths or a build error; map either into
    // the wire contract's BuildResult so the CLI and a future daemon agree.
    let result = match build_target(&build_src, &name, &workspace, &cache) {
        Ok(produced) => BuildResult {
            target: target_arg.clone(),
            status: BuildStatus::Built,
            // Cold one-shot build: `recomputes` is the warm daemon's incremental
            // metric, 0 here. (Cached-vs-built per action awaits the driver
            // surfacing cache-hit info — a small follow-up.)
            recomputes: 0,
            outputs: produced
                .iter()
                .map(|p| OutputArtifact {
                    path: p.clone(),
                    digest: digest_of(&workspace.join(p)),
                })
                .collect(),
            message: None,
        },
        Err(e) => BuildResult {
            target: target_arg.clone(),
            status: BuildStatus::Failed,
            recomputes: 0,
            outputs: vec![],
            message: Some(e),
        },
    };

    if cbor {
        println!("{}", hex(&encode(&result.to_cbor())));
    } else {
        print_build_result(&result);
    }
    match result.status {
        BuildStatus::Failed => ExitCode::FAILURE,
        _ => ExitCode::SUCCESS,
    }
}

fn missing_value(flag: &str) -> ExitCode {
    eprintln!("razel build: {flag} requires a value");
    ExitCode::from(EX_USAGE)
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
