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

use razel_build::{GlobalFlags, build_bazel_with, build_workspace_with};
use razel_core::Digest;
use razel_daemon::rpc::{self, Server};
use razel_exec::Cache;
use razel_wire::{
    BuildResult, BuildState, BuildStatus, ImpactSet, OutputArtifact, VersionInfo, encode,
};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod bazel_flags;
use bazel_flags::{BAZEL_FLAGS, FlagSpec};

/// Wire protocol revision reported by `version` (bumped on breaking IR changes).
const PROTOCOL: i64 = 1;
/// sysexits EX_USAGE — bad invocation (vs. EX failure for a real build error).
const EX_USAGE: u8 = 64;

/// The CLI entry (S0): every razel verb, parsed and dispatched. The library IS the
/// CLI — the `razel` bin and grazel's bin are both thin callers of this function, so
/// a razel flag can never behave differently between the two distributions (§1d).
pub fn run(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("build") => cmd_build(&args[1..]),
        Some("run") => cmd_run(&args[1..]),
        Some("affected") => cmd_affected(&args[1..]),
        Some("subscribe") => cmd_subscribe(&args[1..]),
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

USAGE (Bazel-syntax flags; all Bazel options are recognized):
  razel build <target>... [--disk_cache <dir>] [-C <dir>] [--daemon] [--socket <s>] [--cbor]
  razel affected <file>... [-C <dir>] [--daemon] [--socket <s>] [--cbor]
  razel subscribe [-C <dir>] [--socket <s>] [--cbor]
  razel version [--daemon] [--socket <s>] [--cbor]
  razel daemon [-C <dir>] [--disk_cache <dir>] [--socket <s>]

  <target>          //pkg:name (multi-package workspace) or name/:name (single BUILD)
  --disk_cache <d>  content-addressed cache dir (default: <workspace>/.razel-cache)
  -C, --workspace   workspace dir with BUILD + sources (default: .) [razel-only]
  --daemon          route the request to a running `razel daemon` over UDS [razel-only]
  --socket <s>      daemon socket path (default: <workspace>/.razel-daemon.sock) [razel-only]
  --cbor            print the result as taut-wire CBOR (hex) instead of text [razel-only]

  Other Bazel flags (e.g. -c opt, --copt, --jobs) are recognized; unsupported ones
  print a one-line diagnostic and are ignored.
"
    );
}

/// Parsed flags shared across subcommands.
#[derive(Default)]
struct Opts {
    workspace: PathBuf,
    cache: Option<PathBuf>,
    socket: Option<PathBuf>,
    daemon: bool,
    cbor: bool,
    /// `-c` / `--compilation_mode` (fastbuild|dbg|opt).
    compilation_mode: Option<String>,
    /// Global cc flags: `--copt`/`--cxxopt`/`--conlyopt`, `--define` (as `-D`).
    copts: Vec<String>,
    cxxopts: Vec<String>,
    conlyopts: Vec<String>,
    defines: Vec<String>,
    /// `--linkopt`.
    linkopts: Vec<String>,
    positionals: Vec<String>,
}

impl Opts {
    /// Collapse the parsed cc flags into engine [`GlobalFlags`]: compilation mode
    /// expands to compile flags, then copts/cxxopts/conlyopts and `-D`efines ride
    /// every compile; linkopts ride every link.
    fn global_flags(&self) -> GlobalFlags {
        let mut copts = match self.compilation_mode.as_deref() {
            Some("opt") => vec!["-O2".into(), "-DNDEBUG".into()],
            Some("dbg") => vec!["-O0".into(), "-g".into()],
            _ => vec![], // fastbuild (Bazel's default) adds nothing
        };
        copts.extend(self.copts.iter().cloned());
        copts.extend(self.cxxopts.iter().cloned());
        copts.extend(self.conlyopts.iter().cloned());
        copts.extend(self.defines.iter().map(|d| format!("-D{d}")));
        GlobalFlags {
            copts,
            linkopts: self.linkopts.clone(),
            // Structured configuration (config_setting/select matching) — the cc flag
            // expansion above is separate.
            compilation_mode: self.compilation_mode.clone().unwrap_or_default(),
            defines: self
                .defines
                .iter()
                .filter_map(|d| d.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
                .collect(),
            ..Default::default()
        }
    }
}

/// A flag razel acts on: parses the (optional) value and updates [`Opts`]. Boolean
/// flags receive `Some("true")`/`Some("false")` (so negation `--noX` flows through).
type Handler = fn(&mut Opts, Option<String>);

/// razel's own flags — recognized in addition to (and ahead of) Bazel's. Kept here
/// because Bazel has no equivalent; they share [`FlagSpec`] so the parser is uniform.
static RAZEL_FLAGS: &[FlagSpec] = &[
    FlagSpec {
        name: "workspace",
        abbrev: Some('C'),
        takes_value: true,
        allow_multiple: false,
        silent: false,
    },
    FlagSpec {
        name: "socket",
        abbrev: None,
        takes_value: true,
        allow_multiple: false,
        silent: false,
    },
    FlagSpec {
        name: "daemon",
        abbrev: None,
        takes_value: false,
        allow_multiple: false,
        silent: false,
    },
    FlagSpec {
        name: "cbor",
        abbrev: None,
        takes_value: false,
        allow_multiple: false,
        silent: false,
    },
    // Deprecated alias of Bazel's --disk_cache.
    FlagSpec {
        name: "cache",
        abbrev: None,
        takes_value: true,
        allow_multiple: false,
        silent: false,
    },
];

/// The flags razel actually honors → their effect. **This map is the definition of
/// "supported".** Adding a row makes a recognized Bazel flag take effect; a flag with
/// no row + not `silent` self-diagnoses as unsupported (the data-driven default).
static HANDLERS: &[(&str, Handler)] = &[
    ("workspace", |o, v| {
        if let Some(v) = v {
            o.workspace = PathBuf::from(v);
        }
    }),
    ("disk_cache", |o, v| o.cache = v.map(PathBuf::from)),
    ("cache", |o, v| {
        eprintln!("razel: --cache is deprecated; Bazel spells it --disk_cache");
        o.cache = v.map(PathBuf::from);
    }),
    ("socket", |o, v| o.socket = v.map(PathBuf::from)),
    ("daemon", |o, v| o.daemon = v.as_deref() != Some("false")),
    ("cbor", |o, v| o.cbor = v.as_deref() != Some("false")),
    // Bazel cc build flags → razel's existing cc engine (global, every action).
    ("compilation_mode", |o, v| o.compilation_mode = v),
    ("copt", |o, v| o.copts.extend(v)),
    ("cxxopt", |o, v| o.cxxopts.extend(v)),
    ("conlyopt", |o, v| o.conlyopts.extend(v)),
    ("linkopt", |o, v| o.linkopts.extend(v)),
    ("define", |o, v| o.defines.extend(v)),
];

/// Look up a long flag name across razel's flags then Bazel's.
fn spec_long(name: &str) -> Option<&'static FlagSpec> {
    RAZEL_FLAGS
        .iter()
        .chain(BAZEL_FLAGS)
        .find(|f| f.name == name)
}

/// Look up a short (abbreviated) flag, razel's then Bazel's. (`-C` is razel's
/// workspace; `-c` is Bazel's compilation_mode — distinct by case.)
fn spec_short(c: char) -> Option<&'static FlagSpec> {
    RAZEL_FLAGS
        .iter()
        .chain(BAZEL_FLAGS)
        .find(|f| f.abbrev == Some(c))
}

/// Resolve `--name`, honoring Bazel's `--noNAME` boolean negation.
fn resolve_long(name: &str) -> Option<(&'static FlagSpec, bool)> {
    if let Some(s) = spec_long(name) {
        return Some((s, false));
    }
    if let Some(stripped) = name.strip_prefix("no")
        && let Some(s) = spec_long(stripped)
        && !s.takes_value
    {
        return Some((s, true)); // --noX
    }
    None
}

/// Apply a recognized flag: a handler (supported) runs; otherwise it's silently
/// ignored (language flags razel will never need) or diagnosed (recognized, not
/// yet implemented).
fn dispatch(o: &mut Opts, spec: &FlagSpec, value: Option<String>) {
    if let Some((_, h)) = HANDLERS.iter().find(|(n, _)| *n == spec.name) {
        h(o, value);
    } else if !spec.silent {
        eprintln!(
            "razel: `{}` is a recognized Bazel option, not yet supported by razel — ignoring",
            spec.name
        );
    }
}

/// Parse a Bazel-syntax command line: `--flag`/`--flag=val`/`--flag val`, `--noflag`,
/// short `-x`/`-xval`/`-x val`, `--` (rest are targets), positionals. Driven entirely
/// by the flag tables — unknown (non-Bazel) flags error, like Bazel.
fn parse_opts(args: &[String]) -> Result<Opts, ExitCode> {
    let mut o = Opts {
        workspace: PathBuf::from("."),
        ..Default::default()
    };
    let mut i = 0;
    let mut targets_only = false;
    while i < args.len() {
        let arg = args[i].clone();
        i += 1;
        if targets_only || arg == "-" || !arg.starts_with('-') {
            o.positionals.push(arg);
            continue;
        }
        if arg == "--" {
            targets_only = true;
            continue;
        }

        let (spec, negated, mut value) = if let Some(body) = arg.strip_prefix("--") {
            let (name, inline) = match body.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (body.to_string(), None),
            };
            match resolve_long(&name) {
                Some((s, neg)) => (s, neg, inline),
                None => {
                    eprintln!("razel: unrecognized option `--{name}` (not a Bazel flag)");
                    return Err(ExitCode::from(EX_USAGE));
                }
            }
        } else {
            let c = arg[1..].chars().next().unwrap();
            let attached = arg[1 + c.len_utf8()..].to_string();
            match spec_short(c) {
                Some(s) => (s, false, (!attached.is_empty()).then_some(attached)),
                None => {
                    eprintln!("razel: unrecognized option `-{c}`");
                    return Err(ExitCode::from(EX_USAGE));
                }
            }
        };

        if spec.takes_value {
            if value.is_none() && !negated {
                match args.get(i) {
                    Some(v) => {
                        value = Some(v.clone());
                        i += 1;
                    }
                    None => {
                        eprintln!("razel: `{}` requires a value", spec.name);
                        return Err(ExitCode::from(EX_USAGE));
                    }
                }
            }
        } else {
            value = Some(if negated {
                "false".into()
            } else {
                "true".into()
            });
        }

        dispatch(&mut o, spec, value);
    }
    Ok(o)
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

/// rc-lite (S3d, V3sh1): the WORKSPACE layer only of `.bazelrc` then `.razelrc` —
/// command-scoped lines (`build --flag …`), comments/blanks skipped, bazel's command
/// inheritance (`run` ⊃ `build` ⊃ `common`). No `import`, no `--config`, no
/// system/home layers: those are S6, which grows the layer list around this same
/// parse. `.razelrc` is the razel-only DELTA (§3): applied AFTER `.bazelrc` (bazel
/// never reads it); CLI args follow all rc flags, so the command line always wins.
fn rc_lite_flags(workspace: &Path, commands: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for rc in [".bazelrc", ".razelrc"] {
        let Ok(src) = std::fs::read_to_string(workspace.join(rc)) else { continue };
        for line in src.lines() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            if let Some((cmd, rest)) = t.split_once(char::is_whitespace)
                && commands.contains(&cmd)
            {
                out.extend(rest.split_whitespace().map(String::from));
            }
        }
    }
    out
}

/// Parse args twice when rc files apply: once to find the workspace, then with the
/// workspace's rc-lite flags PREPENDED (rc first ⇒ explicit CLI flags override).
fn parse_opts_with_rc(commands: &[&str], args: &[String]) -> Result<Opts, ExitCode> {
    let pre = parse_opts(args)?;
    let rc = rc_lite_flags(&pre.workspace, commands);
    if rc.is_empty() {
        return Ok(pre);
    }
    let merged: Vec<String> = rc.into_iter().chain(args.iter().cloned()).collect();
    parse_opts(&merged)
}

fn cmd_build(args: &[String]) -> ExitCode {
    let o = match parse_opts_with_rc(&["common", "build"], args) {
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

/// `razel run <target> [-- args…]` (S3b): build, then exec the target's runnable
/// output with the program args, propagating its exit status. Local path today; the
/// daemon path becomes the Command service's `run` method at S3c (client #1 holds —
/// this function IS the future service client's rendering half).
fn cmd_run(args: &[String]) -> ExitCode {
    let o = match parse_opts_with_rc(&["common", "build", "run"], args) {
        Ok(o) => o,
        Err(c) => return c,
    };
    let Some(target_arg) = o.positionals.first().cloned() else {
        eprintln!("razel run: expected <target> [-- args…]");
        return ExitCode::from(EX_USAGE);
    };
    let prog_args = &o.positionals[1..];

    let result = match local_build(&o, &target_arg) {
        Ok(r) => r,
        Err(c) => return c,
    };
    if matches!(result.status, BuildStatus::Failed) {
        print_build_result(&result);
        return ExitCode::FAILURE;
    }
    let Some(exe) = result.outputs.first() else {
        eprintln!("razel run: `{target_arg}` produced no runnable output");
        return ExitCode::FAILURE;
    };
    let exe_path = o.workspace.join(&exe.path);
    match std::process::Command::new(&exe_path)
        .args(prog_args)
        .current_dir(&o.workspace)
        .status()
    {
        Ok(st) => ExitCode::from(st.code().unwrap_or(1).clamp(0, 255) as u8),
        Err(e) => {
            eprintln!("razel run: cannot exec {}: {e}", exe_path.display());
            ExitCode::FAILURE
        }
    }
}

fn cmd_affected(args: &[String]) -> ExitCode {
    let o = match parse_opts(args) {
        Ok(o) => o,
        Err(c) => return c,
    };
    if o.positionals.is_empty() {
        eprintln!("razel affected: expected one or more <file>");
        return ExitCode::from(EX_USAGE);
    }
    let files = o.positionals.clone();

    let impact = if o.daemon {
        let socket = o
            .socket
            .clone()
            .unwrap_or_else(|| default_socket(&o.workspace));
        match daemon_call(&socket, &rpc::req_affected(&files)) {
            Ok(p) => ImpactSet::from_cbor(&p),
            Err(c) => return c,
        }
    } else {
        match rpc::impact(&o.workspace, &files) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("razel affected: {e}");
                return ExitCode::FAILURE;
            }
        }
    };

    if o.cbor {
        println!("{}", hex(&encode(&impact.to_cbor())));
    } else {
        print_impact(&impact);
    }
    ExitCode::SUCCESS
}

fn print_impact(i: &ImpactSet) {
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

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn cmd_subscribe(args: &[String]) -> ExitCode {
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

fn print_build_state(s: &BuildState) {
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
///
/// A `//pkg:name` label builds through the **multi-package workspace** loader
/// (cross-package deps load on demand); a bare `name`/`:name` builds the workspace's
/// own `BUILD` single-package. Both honor the global cc flags (`-c`/`--copt`/…).
fn local_build(o: &Opts, target_arg: &str) -> Result<BuildResult, ExitCode> {
    // §1b: a local in-process build is a workspace WRITER too — same lock, same
    // fail-loud as the daemons (released on return via Drop).
    let _writer = razel_daemon::outlock::acquire(&o.workspace, "razel-local", "")
        .map_err(|e| {
            eprintln!("razel: {e}");
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

    let report = if target_arg.starts_with("//") {
        // Workspace label → load packages on demand from the workspace root.
        build_workspace_with(&o.workspace, target_arg, &cache, o.global_flags())
    } else {
        // Bare name / :name → single-package build from the workspace's BUILD.
        let name = target_arg.rsplit(':').next().unwrap_or(target_arg);
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
        build_bazel_with(&build_src, name, &o.workspace, &cache, o.global_flags())
    };

    Ok(match report {
        Ok(report) => BuildResult {
            target: target_arg.to_string(),
            // executed == 0 → fully served from cache.
            status: if report.executed == 0 {
                BuildStatus::Cached
            } else {
                BuildStatus::Built
            },
            recomputes: report.executed as i64,
            // The target's DefaultInfo, not every intermediate (bazel semantics —
            // `run` execs outputs[0]); empty default_info falls back to produced.
            outputs: if report.default_outputs.is_empty() {
                &report.produced
            } else {
                &report.default_outputs
            }
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
    })
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
    let verb = match r.status {
        BuildStatus::Cached => "cached",
        _ => "built",
    };
    println!(
        "razel: {verb} {} ({n} output{}, {} recomputed)",
        r.target,
        if n == 1 { "" } else { "s" },
        r.recomputes
    );
    for o in &r.outputs {
        let h = hex(&o.digest);
        let short = &h[..h.len().min(12)];
        println!("  {short}  {}", o.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(a: &[&str]) -> Opts {
        parse_opts(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    fn rc_ws(tag: &str, bazelrc: &str, razelrc: &str) -> std::path::PathBuf {
        let ws = std::env::temp_dir().join(format!("razel-rc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(&ws).unwrap();
        if !bazelrc.is_empty() {
            std::fs::write(ws.join(".bazelrc"), bazelrc).unwrap();
        }
        if !razelrc.is_empty() {
            std::fs::write(ws.join(".razelrc"), razelrc).unwrap();
        }
        ws
    }

    #[test]
    fn rc_lite_scopes_inherits_and_layers() {
        // Command scoping + `common` + comments; `.razelrc` flags come AFTER
        // `.bazelrc` (the delta layer), CLI args after both (tested via merge order).
        let ws = rc_ws(
            "scope",
            "# comment\ncommon --a\nbuild --b\ntest --never\n",
            "build --c\n",
        );
        assert_eq!(rc_lite_flags(&ws, &["common", "build"]), vec!["--a", "--b", "--c"]);
        // run inherits build (+common); test-scoped lines stay out.
        assert_eq!(
            rc_lite_flags(&ws, &["common", "build", "run"]),
            vec!["--a", "--b", "--c"]
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn rc_lite_flags_reach_parse_and_cli_wins() {
        // .razelrc sets a disk cache; the parsed Opts carry it…
        let ws = rc_ws("parse", "", "build --disk_cache /tmp/rc-cache\n");
        let args: Vec<String> =
            vec!["t".into(), "-C".into(), ws.display().to_string()];
        let o = parse_opts_with_rc(&["common", "build"], &args).unwrap();
        assert_eq!(o.cache.as_deref(), Some(std::path::Path::new("/tmp/rc-cache")));
        // …and an explicit CLI flag OVERRIDES the rc layer.
        let args: Vec<String> = vec![
            "t".into(),
            "-C".into(),
            ws.display().to_string(),
            "--disk_cache".into(),
            "/tmp/cli-cache".into(),
        ];
        let o = parse_opts_with_rc(&["common", "build"], &args).unwrap();
        assert_eq!(o.cache.as_deref(), Some(std::path::Path::new("/tmp/cli-cache")));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn rc_lite_absent_files_are_silent() {
        let ws = rc_ws("none", "", "");
        assert!(rc_lite_flags(&ws, &["common", "build"]).is_empty());
        let _ = std::fs::remove_dir_all(&ws);
    }
    fn err(a: &[&str]) -> bool {
        parse_opts(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>()).is_err()
    }

    #[test]
    fn razel_extensions_and_targets() {
        let o = p(&["-C", "/ws", "--disk_cache=/c", "--daemon", "//a:b", "//c:d"]);
        assert_eq!(o.workspace, PathBuf::from("/ws"));
        assert_eq!(o.cache, Some(PathBuf::from("/c")));
        assert!(o.daemon);
        assert_eq!(o.positionals, vec!["//a:b", "//c:d"]);
    }

    #[test]
    fn cache_is_a_deprecated_alias() {
        assert_eq!(p(&["--cache", "/x"]).cache, Some(PathBuf::from("/x")));
    }

    #[test]
    fn value_flags_consume_their_value_not_the_target() {
        // --copt -O2 //t : -O2 is copt's value, //t is the only target.
        assert_eq!(p(&["--copt", "-O2", "//t"]).positionals, vec!["//t"]);
        assert_eq!(p(&["--copt=-O2", "//t"]).positionals, vec!["//t"]);
        assert_eq!(p(&["-c", "opt", "//t"]).positionals, vec!["//t"]);
    }

    #[test]
    fn boolean_flags_and_negation_dont_eat_the_target() {
        assert_eq!(p(&["--keep_going", "//t"]).positionals, vec!["//t"]);
        assert_eq!(p(&["--nokeep_going", "//t"]).positionals, vec!["//t"]);
    }

    #[test]
    fn double_dash_makes_the_rest_targets() {
        // After --, a leading-dash token is a target, not a flag.
        assert_eq!(
            p(&["--", "--copt", "//t"]).positionals,
            vec!["--copt", "//t"]
        );
    }

    #[test]
    fn unknown_non_bazel_flag_errors() {
        assert!(err(&["--frobnicate"]));
        assert!(err(&["-Z"]));
    }

    #[test]
    fn recognized_but_unsupported_bazel_flag_parses() {
        // --platforms is real Bazel; razel recognizes + diagnoses it, still parses.
        let o = p(&["--platforms=//p:x", "//t"]);
        assert_eq!(o.positionals, vec!["//t"]);
    }
}

#[cfg(test)]
mod flag_mapping_tests {
    use super::*;

    fn p(a: &[&str]) -> Opts {
        parse_opts(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn compilation_mode_and_copts_map_to_global_flags() {
        // -c opt expands; --copt/--cxxopt/--define accumulate into compile flags.
        let g = p(&[
            "-c",
            "opt",
            "--copt=-Wall",
            "--cxxopt=-std=c++20",
            "--define=FOO=1",
        ])
        .global_flags();
        assert!(g.copts.contains(&"-O2".to_string()));
        assert!(g.copts.contains(&"-DNDEBUG".to_string()));
        assert!(g.copts.contains(&"-Wall".to_string()));
        assert!(g.copts.contains(&"-std=c++20".to_string()));
        assert!(g.copts.contains(&"-DFOO=1".to_string()));
        // --linkopt rides the link, not the compile.
        assert_eq!(p(&["--linkopt=-s"]).global_flags().linkopts, vec!["-s"]);
        // fastbuild (default) adds no optimization flags.
        assert!(p(&["-c", "fastbuild"]).global_flags().copts.is_empty());
    }
}
