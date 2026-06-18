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

use razel_build::{
    GlobalFlags, build_bazel_with, build_workspace_with, config_segment, convenience_symlinks,
    resolve_build_file,
};
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
        Some("test") => cmd_test(&args[1..]),
        Some("clean") => cmd_clean(&args[1..]),
        Some("affected") => cmd_affected(&args[1..]),
        Some("query") => cmd_query(&args[1..]),
        Some("subscribe") => cmd_subscribe(&args[1..]),
        Some("version") | Some("-V") | Some("--version") => cmd_version(&args[1..]),
        Some("daemon") => cmd_daemon(&args[1..]),
        Some("help") => {
            cmd_help(&args[1..]);
            ExitCode::SUCCESS
        }
        Some("-h") | Some("--help") | None => {
            cmd_help(&[]);
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("razel: unknown command {other:?}\n");
            cmd_help(&[]);
            ExitCode::from(EX_USAGE)
        }
    }
}

/// A flag razel ACTUALLY honors — the help surface. Kept ⊆ [`HANDLERS`] (the support
/// definition) by `help_documents_only_supported_flags`, so help never advertises a
/// recognized-but-ignored Bazel flag.
struct FlagHelp {
    name: &'static str,
    abbrev: Option<char>,
    arg: Option<&'static str>,
    desc: &'static str,
}

/// Shown under every command.
static COMMON_FLAGS: &[&str] = &["workspace", "disk_cache"];

static FLAG_HELP: &[FlagHelp] = &[
    FlagHelp { name: "workspace", abbrev: Some('C'), arg: Some("<dir>"), desc: "Workspace dir with the BUILD/MODULE files (default: .)." },
    FlagHelp { name: "disk_cache", abbrev: None, arg: Some("<dir>"), desc: "Content-addressed output cache (default: <ws>/.razel-cache)." },
    FlagHelp { name: "compilation_mode", abbrev: Some('c'), arg: Some("<mode>"), desc: "Compilation mode: fastbuild | dbg | opt." },
    FlagHelp { name: "copt", abbrev: None, arg: Some("<opt>"), desc: "Add an option to every C/C++ compile (repeatable)." },
    FlagHelp { name: "cxxopt", abbrev: None, arg: Some("<opt>"), desc: "Add an option to every C++ compile (repeatable)." },
    FlagHelp { name: "conlyopt", abbrev: None, arg: Some("<opt>"), desc: "Add an option to every C compile (repeatable)." },
    FlagHelp { name: "linkopt", abbrev: None, arg: Some("<opt>"), desc: "Add an option to every link (repeatable)." },
    FlagHelp { name: "define", abbrev: None, arg: Some("<k=v>"), desc: "Set a build variable / config value (repeatable)." },
    FlagHelp { name: "jobs", abbrev: Some('j'), arg: Some("<n>"), desc: "Run up to <n> targets/tests concurrently (default: serial)." },
    FlagHelp { name: "bazel_build_compat", abbrev: None, arg: None, desc: "Write outputs to Bazel's bazel-out/ tree (also env RAZEL_BAZEL_BUILD_COMPAT=1)." },
    FlagHelp { name: "expunge", abbrev: None, arg: None, desc: "Remove more thorough state (with clean)." },
    FlagHelp { name: "daemon", abbrev: None, arg: None, desc: "Route the request to a running `razel daemon` over a socket." },
    FlagHelp { name: "socket", abbrev: None, arg: Some("<path>"), desc: "Daemon socket path (default: <ws>/.razel-daemon.sock)." },
    FlagHelp { name: "cbor", abbrev: None, arg: None, desc: "Print the result as taut-wire CBOR (hex) instead of text." },
];

/// One CLI command: its argument shape, one-line summary, and the supported flags it honors.
struct CmdHelp {
    name: &'static str,
    args: &'static str,
    summary: &'static str,
    flags: &'static [&'static str],
}

static COMMANDS: &[CmdHelp] = &[
    CmdHelp { name: "build", args: "<target>...", summary: "Build the specified targets.",
        flags: &["compilation_mode", "copt", "cxxopt", "conlyopt", "linkopt", "define", "jobs", "bazel_build_compat", "daemon", "socket", "cbor"] },
    CmdHelp { name: "run", args: "<target> [-- args…]", summary: "Build, then run a target's output.",
        flags: &["compilation_mode", "copt", "cxxopt", "conlyopt", "linkopt", "define", "bazel_build_compat"] },
    CmdHelp { name: "test", args: "<target>...", summary: "Build and run the specified test targets.",
        flags: &["jobs", "compilation_mode", "copt", "cxxopt", "conlyopt", "linkopt", "define", "bazel_build_compat"] },
    CmdHelp { name: "clean", args: "[--expunge]", summary: "Remove razel's outputs, cache, and convenience symlinks (razel-out, razel-bin, .razel-cache).",
        flags: &["expunge"] },
    CmdHelp { name: "affected", args: "<file>...", summary: "List the targets affected by changed files.",
        flags: &["daemon", "socket", "cbor"] },
    CmdHelp { name: "subscribe", args: "", summary: "Stream build events from a running daemon.",
        flags: &["socket", "cbor"] },
    CmdHelp { name: "version", args: "", summary: "Print version information.",
        flags: &["daemon", "socket", "cbor"] },
    CmdHelp { name: "daemon", args: "", summary: "Run the razel build daemon.",
        flags: &["socket", "disk_cache"] },
    CmdHelp { name: "help", args: "[<command>]", summary: "Print help for a command, or this index.",
        flags: &[] },
];

fn flag_help(name: &str) -> Option<&'static FlagHelp> {
    FLAG_HELP.iter().find(|f| f.name == name)
}

/// `  -c, --compilation_mode <mode>   Compilation mode: …`
fn flag_line(f: &FlagHelp) -> String {
    let long = match f.arg {
        Some(a) => format!("--{} {a}", f.name),
        None => format!("--{}", f.name),
    };
    let head = match f.abbrev {
        Some(c) => format!("-{c}, {long}"),
        None => format!("    {long}"),
    };
    format!("  {head:<36}{}", f.desc)
}

/// `razel help [<command>]` (Bazel `help`): no arg ⇒ the command index; a command ⇒ its usage
/// + the flags razel ACTUALLY honors for it. Recognized-but-ignored Bazel flags are NOT listed
/// (they self-diagnose if used) — the help surface is the supported surface.
fn cmd_help(args: &[String]) {
    if let Some(cmd) = args.first()
        && let Some(c) = COMMANDS.iter().find(|c| c.name == cmd.as_str())
    {
        println!("Usage: razel {} {}\n\n{}", c.name, c.args, c.summary);
        let names: Vec<&str> = COMMON_FLAGS.iter().chain(c.flags.iter()).copied().collect();
        if !names.is_empty() {
            println!("\nSupported options:");
            for n in names {
                if let Some(f) = flag_help(n) {
                    println!("{}", flag_line(f));
                }
            }
        }
        if c.args.contains("target") {
            println!("\n  <target>   //pkg:name (workspace) or name/:name (single-package BUILD/BUILD.bazel)");
        }
        return;
    }
    if let Some(cmd) = args.first() {
        eprintln!("razel: unknown command {cmd:?}\n");
    }
    let w = COMMANDS.iter().map(|c| c.name.len()).max().unwrap_or(0);
    println!("razel — a Bazel-subset build engine\n");
    println!("Usage: razel <command> <options> ...\n");
    println!("Available commands:");
    for c in COMMANDS {
        println!("  {:<w$}  {}", c.name, c.summary);
    }
    println!("\nGetting more help:");
    println!("  razel help <command>   Print help and the supported options for <command>.");
    println!("\nFlags shared by most commands: -C/--workspace, --disk_cache. Bazel flags not");
    println!("listed under a command are recognized but ignored (a one-line diagnostic prints).");
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
    /// `--jobs`/`-j`: parallel-executor concurrency (0 ⇒ serial default).
    jobs: usize,
    /// `clean --expunge` (Bazel): the more-thorough clean.
    expunge: bool,
    /// `--bazel_build_compat` (razel-only): write outputs to Bazel's `bazel-out/` tree.
    /// Also set by the `RAZEL_BAZEL_BUILD_COMPAT` env var (`1`/`T`), merged in `global_flags`.
    bazel_build_compat: bool,
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
            jobs: self.jobs,
            // CLI/.razelrc flag OR the env var (the env is the primary trigger per the spec).
            bazel_build_compat: self.bazel_build_compat || bazel_build_compat_env(),
            // The CLI always materializes under the output tree (razel-out/, or bazel-out/
            // under compat) — never in-tree — so a user's source tree stays clean.
            bin_tree_layout: true,
            ..Default::default()
        }
    }
}

/// `RAZEL_BAZEL_BUILD_COMPAT` truthy? Accepts `1`/`t`/`true` (case-insensitive).
fn bazel_build_compat_env() -> bool {
    std::env::var("RAZEL_BAZEL_BUILD_COMPAT")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "t" | "true"))
        .unwrap_or(false)
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
    // Bazel build-dir compat: write outputs to bazel-out/ (also via RAZEL_BAZEL_BUILD_COMPAT).
    FlagSpec {
        name: "bazel_build_compat",
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
    // S5x: --jobs/-j now takes effect (was recognized-but-ignored) → the parallel executor.
    ("jobs", |o, v| {
        if let Some(n) = v.and_then(|v| v.parse::<usize>().ok()) {
            o.jobs = n;
        }
    }),
    // `clean --expunge` (handled so it doesn't self-diagnose as unsupported).
    ("expunge", |o, v| o.expunge = v.as_deref() != Some("false")),
    ("bazel_build_compat", |o, v| {
        o.bazel_build_compat = v.as_deref() != Some("false")
    }),
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
    // RG 0010: ABSOLUTIZE the workspace (the bare default `.` and any relative -C):
    // actions execute in sandbox dirs, where a relative exec_root breaks input
    // staging on cold builds — a warm hit masks it, which is how it escaped CI.
    if o.workspace.is_relative() {
        o.workspace = std::fs::canonicalize(&o.workspace).map_err(|e| {
            eprintln!("razel: cannot resolve workspace {}: {e}", o.workspace.display());
            ExitCode::FAILURE
        })?;
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

/// A Bazel target PATTERN (expands to many targets) vs a concrete label.
fn is_pattern(t: &str) -> bool {
    t.contains("...") || t.ends_with(":all")
}

fn cmd_build(args: &[String]) -> ExitCode {
    let o = match parse_opts_with_rc(&["common", "build"], args) {
        Ok(o) => o,
        Err(c) => return c,
    };
    if o.positionals.is_empty() {
        // Bazel does NOT error on a bare `build` — it builds the empty target set and exits
        // 0 ("requested an empty set of targets. Nothing will be built."). Match that.
        eprintln!(
            "WARNING: Your request is correct, but requested an empty set of targets. \
             Nothing will be built."
        );
        eprintln!("INFO: Build completed successfully.");
        return ExitCode::SUCCESS;
    }
    // Patterns (`//...`, `//pkg:all`) or several targets → expand + multi-build. A single
    // concrete label keeps the existing daemon/cbor-capable path (output unchanged).
    if o.positionals.len() > 1 || o.positionals.iter().any(|t| is_pattern(t)) {
        return cmd_build_many(&o);
    }
    let target_arg = o.positionals[0].clone();

    let t0 = std::time::Instant::now();
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
        if !matches!(result.status, BuildStatus::Failed) {
            ensure_convenience_symlinks(&o.workspace, &o.global_flags());
            eprintln!("INFO: Elapsed time: {:.3}s", t0.elapsed().as_secs_f64());
            eprintln!("INFO: Build completed successfully.");
        }
    }
    match result.status {
        BuildStatus::Failed => ExitCode::FAILURE,
        _ => ExitCode::SUCCESS,
    }
}

/// `razel build //...` / multiple targets: expand patterns to concrete labels, build each
/// (one workspace lock + shared cache; the cache dedups shared deps), print a Bazel-style
/// per-target line + summary. Exit 1 if any target failed.
fn cmd_build_many(o: &Opts) -> ExitCode {
    let mut labels: Vec<String> = Vec::new();
    for p in &o.positionals {
        match razel_build::expand_pattern(&o.workspace, p, o.global_flags()) {
            Ok(ls) => labels.extend(ls),
            Err(e) => {
                eprintln!("razel build: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    labels.sort();
    labels.dedup();
    if labels.is_empty() {
        eprintln!("razel build: no targets matched");
        return ExitCode::from(EX_USAGE);
    }
    let _writer = match razel_daemon::outlock::acquire(&o.workspace, "razel-local", "") {
        Ok(w) => w,
        Err(e) => {
            eprintln!("razel: {e}");
            return ExitCode::FAILURE;
        }
    };
    let cache = match open_cache(o) {
        Ok(c) => c,
        Err(c) => return c,
    };
    let t0 = std::time::Instant::now();
    let (mut built, mut cached, mut failed) = (0usize, 0usize, 0usize);
    for label in &labels {
        match build_one(o, label, &cache, o.global_flags()) {
            Ok(r) => match r.status {
                BuildStatus::Built => {
                    built += 1;
                    eprintln!("  {label} built");
                }
                BuildStatus::Cached => {
                    cached += 1;
                    eprintln!("  {label} up-to-date");
                }
                BuildStatus::Failed => {
                    failed += 1;
                    eprintln!("  {label} FAILED");
                    if let Some(m) = &r.message {
                        eprintln!("    {m}");
                    }
                }
            },
            Err(_) => {
                failed += 1;
                eprintln!("  {label} FAILED");
            }
        }
    }
    if built + cached > 0 {
        ensure_convenience_symlinks(&o.workspace, &o.global_flags());
    }
    eprintln!("INFO: Elapsed time: {:.3}s", t0.elapsed().as_secs_f64());
    if failed > 0 {
        eprintln!(
            "ERROR: build did NOT complete — {} target(s): {built} built, {cached} up-to-date, {failed} FAILED.",
            labels.len()
        );
    } else {
        eprintln!(
            "INFO: Build completed successfully, {} target(s) ({built} built, {cached} up-to-date).",
            labels.len()
        );
    }
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
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

/// `razel test <target>` (S5): build → exec the test → bazel's test protocol.
/// Exit 0 = passed; exit 3 = build OK, test FAILED (bazel's code); exit 1 = the
/// build itself failed. stdout+stderr land in
/// `.razel-cache/testlogs/<pkg>/<name>/test.log`; one bazel-shaped summary line
/// (`//pkg:name PASSED in 0.3s`) per target.
/// `razel test <target>... [-j N]` (Bazel `test`): build each target, exec it, apply Bazel's
/// test protocol — exit 0 (all pass) / 3 (a test failed) / 1 (a build failed) — write a
/// per-target `testlogs/<pkg>/<name>/test.log`, print a PASSED/FAILED line per target plus a
/// summary. `-j N` runs up to N targets CONCURRENTLY (test-level parallelism; each target's
/// own build stays serial). Pattern targets (`//...`) aren't expanded yet — list explicitly.
fn cmd_test(args: &[String]) -> ExitCode {
    let o = match parse_opts_with_rc(&["common", "build", "test"], args) {
        Ok(o) => o,
        Err(c) => return c,
    };
    let targets = o.positionals.clone();
    if targets.is_empty() {
        eprintln!("razel test: expected <target>...");
        return ExitCode::from(EX_USAGE);
    }
    // §1b: ONE workspace-writer lock for the whole batch (the per-target builds run under it).
    let _writer = match razel_daemon::outlock::acquire(&o.workspace, "razel-local", "") {
        Ok(w) => w,
        Err(e) => {
            eprintln!("razel: {e}");
            return ExitCode::FAILURE;
        }
    };
    let cache = match open_cache(&o) {
        Ok(c) => c,
        Err(c) => return c,
    };
    // Per-target builds are SERIAL (jobs = 1); the -j pool parallelizes ACROSS tests.
    let mut build_flags = o.global_flags();
    build_flags.jobs = 1;
    let jobs = o.jobs.max(1);

    // Run each target's (build → exec → verdict) in a jobs-bounded pool, into index-keyed
    // slots so the printed order is the target order regardless of completion order.
    let slots: Vec<std::sync::Mutex<Option<TestOutcome>>> =
        (0..targets.len()).map(|_| std::sync::Mutex::new(None)).collect();
    let cursor = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| loop {
                let i = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(t) = targets.get(i) else { break };
                let outcome = run_one_test(&o, t, &cache, build_flags.clone());
                *slots[i].lock().expect("test slot") = Some(outcome);
            });
        }
    });

    let (mut passed, mut failed, mut build_err) = (0usize, 0usize, 0usize);
    for (i, slot) in slots.iter().enumerate() {
        match slot.lock().expect("test slot").take().expect("target ran") {
            TestOutcome::Passed(secs) => {
                passed += 1;
                eprintln!("{}  PASSED in {secs:.1}s", targets[i]);
            }
            TestOutcome::Failed(secs, log) => {
                failed += 1;
                eprintln!("{}  FAILED in {secs:.1}s\n  log: {log}", targets[i]);
            }
            TestOutcome::BuildError(msg) => {
                build_err += 1;
                eprintln!("{}  BUILD FAILED", targets[i]);
                if !msg.is_empty() {
                    eprintln!("  {msg}");
                }
            }
        }
    }
    let ran = passed + failed;
    if ran > 0 {
        ensure_convenience_symlinks(&o.workspace, &o.global_flags());
    }
    let tail = if build_err > 0 {
        format!(", {build_err} not built")
    } else {
        String::new()
    };
    eprintln!(
        "Executed {ran} out of {} tests: {passed} passing, {failed} failing{tail}.",
        targets.len()
    );
    // Bazel exit codes: 1 = build/analysis error, 3 = a test failed, 0 = all passed.
    if build_err > 0 {
        ExitCode::FAILURE
    } else if failed > 0 {
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    }
}

/// One test target's verdict.
enum TestOutcome {
    Passed(f64),
    Failed(f64, String),
    BuildError(String),
}

/// Build + exec one test target, capture `test.log`, return the verdict. A build failure or a
/// missing runnable output is a `BuildError` (exit 1, never the tests-failed code).
fn run_one_test(o: &Opts, target_arg: &str, cache: &Cache, flags: GlobalFlags) -> TestOutcome {
    let compat = flags.bazel_build_compat; // read before `flags` moves into build_one
    let cfg = config_segment(&flags.compilation_mode);
    let result = match build_one(o, target_arg, cache, flags) {
        Ok(r) => r,
        Err(_) => return TestOutcome::BuildError(String::new()),
    };
    if matches!(result.status, BuildStatus::Failed) {
        return TestOutcome::BuildError(result.message.unwrap_or_default());
    }
    let Some(exe) = result.outputs.first() else {
        return TestOutcome::BuildError(format!("`{target_arg}` produced no runnable test output"));
    };
    let t0 = std::time::Instant::now();
    let out = match std::process::Command::new(o.workspace.join(&exe.path))
        .current_dir(&o.workspace)
        .output()
    {
        Ok(out) => out,
        Err(e) => return TestOutcome::BuildError(format!("cannot exec {}: {e}", exe.path)),
    };
    let secs = t0.elapsed().as_secs_f64();
    // Bazel's testlogs shape: logs land under the output base's `testlogs/<pkg>/<name>/`
    // (`razel-out/<config>/testlogs`, or `bazel-out/<config>/testlogs` under compat), reachable
    // via the `razel-testlogs` / `bazel-testlogs` convenience symlink.
    let rest = target_arg.trim_start_matches('/');
    let (pkg, name) = rest.split_once(':').unwrap_or(("", rest));
    let out_root = if compat { "bazel-out" } else { "razel-out" };
    let log_dir = o.workspace.join(out_root).join(&cfg).join("testlogs").join(pkg).join(name);
    let _ = std::fs::create_dir_all(&log_dir);
    let mut log = out.stdout.clone();
    log.extend_from_slice(&out.stderr);
    let log_path = log_dir.join("test.log");
    let _ = std::fs::write(&log_path, &log);
    if out.status.success() {
        TestOutcome::Passed(secs)
    } else {
        TestOutcome::Failed(secs, log_path.display().to_string())
    }
}

/// `razel clean` (Bazel `clean`): remove razel's output/state for this workspace — the
/// content-addressed cache (`.razel-cache/`), the output tree (`razel-out/`), and the
/// Bazel-style convenience symlinks (`razel-bin`, `razel-testlogs`), just as `bazel clean`
/// wipes `bazel-out` + its `bazel-*` symlinks. Under `--bazel_build_compat` (or the env var)
/// razel wrote into Bazel's tree, so the `bazel-out` / `bazel-bin` / `bazel-testlogs` set is
/// removed too. Symlinks are unlinked (never followed). `--expunge`/`--async` are accepted
/// (Bazel-compat): razel keeps a single state dir, so `--expunge` is currently equivalent and
/// `--async` runs synchronously. Idempotent (nothing present = success), like Bazel; the
/// summary goes to stderr (Bazel stream discipline).
fn cmd_clean(args: &[String]) -> ExitCode {
    let o = match parse_opts(args) {
        Ok(o) => o,
        Err(c) => return c,
    };
    let compat = o.bazel_build_compat || bazel_build_compat_env();
    let mut names: Vec<&str> = vec![".razel-cache", "razel-out", "razel-bin", "razel-testlogs"];
    if compat {
        names.extend(["bazel-out", "bazel-bin", "bazel-testlogs"]);
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

fn cmd_query(args: &[String]) -> ExitCode {
    let mut output = razel_query::Output::Label;
    // §12 default is --implicit_deps; slice 1 emits the same explicit graph either way (no native
    // implicit labels yet), so the flag is plumbed but the result is unaffected today.
    let mut implicit = true;
    let mut workspace = std::path::PathBuf::from(".");
    let mut expr: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if let Some(v) = a.strip_prefix("--output=") {
            match razel_query::Output::parse(v) {
                Ok(m) => output = m,
                Err(e) => {
                    eprintln!("razel query: {e}");
                    return ExitCode::from(EX_USAGE);
                }
            }
        } else if a == "--noimplicit_deps" {
            implicit = false;
        } else if a == "--implicit_deps" {
            implicit = true;
        } else if a == "-C" || a == "--workspace" {
            i += 1;
            match args.get(i) {
                Some(w) => workspace = std::path::PathBuf::from(w),
                None => {
                    eprintln!("razel query: `{a}` needs a path");
                    return ExitCode::from(EX_USAGE);
                }
            }
        } else if let Some(v) = a.strip_prefix("--workspace=") {
            workspace = std::path::PathBuf::from(v);
        } else if a.starts_with("--") {
            eprintln!("razel query: unknown flag `{a}`");
            return ExitCode::from(EX_USAGE);
        } else if expr.is_none() {
            expr = Some(a.clone());
        } else {
            eprintln!("razel query: unexpected argument `{a}` (expected one expression)");
            return ExitCode::from(EX_USAGE);
        }
        i += 1;
    }
    let Some(expr) = expr else {
        eprintln!("razel query: missing query expression");
        return ExitCode::from(EX_USAGE);
    };
    if workspace.is_relative() {
        workspace = match std::fs::canonicalize(&workspace) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("razel query: cannot resolve workspace {}: {e}", workspace.display());
                return ExitCode::FAILURE;
            }
        };
    }
    // Results → stdout; diagnostics → stderr (the established stream discipline).
    match razel_query::run(&workspace, GlobalFlags::default(), &expr, output, implicit) {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("razel query: {e}");
            ExitCode::FAILURE
        }
    }
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
/// Open the workspace's content-addressed cache (`--disk_cache` or `<ws>/.razel-cache`).
fn open_cache(o: &Opts) -> Result<Cache, ExitCode> {
    let cache_path = o
        .cache
        .clone()
        .unwrap_or_else(|| o.workspace.join(".razel-cache"));
    Cache::new(&cache_path).map_err(|e| {
        eprintln!("razel build: cannot open cache {}: {e}", cache_path.display());
        ExitCode::FAILURE
    })
}

fn local_build(o: &Opts, target_arg: &str) -> Result<BuildResult, ExitCode> {
    // §1b: a local in-process build is a workspace WRITER too — same lock, same
    // fail-loud as the daemons (released on return via Drop).
    let _writer = razel_daemon::outlock::acquire(&o.workspace, "razel-local", "")
        .map_err(|e| {
            eprintln!("razel: {e}");
            ExitCode::FAILURE
        })?;
    let cache = open_cache(o)?;
    build_one(o, target_arg, &cache, o.global_flags())
}

/// Build ONE target against an already-open `cache` with explicit `flags` — the CALLER holds
/// the workspace lock. Factored from [`local_build`] so the `test` batch holds the lock once
/// and builds many targets (each with `flags.jobs = 1`; cross-test parallelism is the batch
/// pool's job, not the per-build executor's).
fn build_one(
    o: &Opts,
    target_arg: &str,
    cache: &Cache,
    flags: GlobalFlags,
) -> Result<BuildResult, ExitCode> {
    let report = if target_arg.starts_with("//")
        || (target_arg.starts_with('@') && target_arg.contains("//"))
    {
        // Workspace label (`//pkg:name`) OR an external-repo label (`@crates//:blake3`,
        // RazelRustParityPlan B1) → load packages on demand from the workspace root; the analysis
        // path seeds the `@crates` lock + follows the alias chain to the versioned crate repo.
        build_workspace_with(&o.workspace, target_arg, cache, flags)
    } else {
        // Bare name / :name → single-package build from the workspace's root package.
        // Route through the canonical resolver (BUILD.bazel over BUILD) so the bare-name
        // path matches package discovery, not a separate ad-hoc probe (RG 0011).
        let name = target_arg.rsplit(':').next().unwrap_or(target_arg);
        let build_path = match resolve_build_file(&o.workspace, flags.strict_bazel) {
            Ok(Some(p)) => p,
            Ok(None) => {
                eprintln!(
                    "razel build: no BUILD or BUILD.bazel in {}",
                    o.workspace.display()
                );
                return Err(ExitCode::FAILURE);
            }
            Err(e) => {
                eprintln!("razel build: {e}");
                return Err(ExitCode::FAILURE);
            }
        };
        let build_src = std::fs::read_to_string(&build_path).map_err(|e| {
            eprintln!("razel build: cannot read {}: {e}", build_path.display());
            ExitCode::FAILURE
        })?;
        build_bazel_with(&build_src, name, &o.workspace, cache, flags)
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

/// Mint Bazel-style convenience symlinks in the workspace after a build: `razel-bin` /
/// `razel-testlogs` → `razel-out/<config>/{bin,testlogs}` (or `bazel-*` under
/// `--bazel_build_compat`). Best-effort + idempotent; only ever replaces a symlink we own —
/// never clobbers a real file/dir a user placed at that name.
fn ensure_convenience_symlinks(workspace: &std::path::Path, flags: &GlobalFlags) {
    for (link, target) in convenience_symlinks(flags) {
        let link_path = workspace.join(&link);
        match std::fs::symlink_metadata(&link_path) {
            Ok(m) if m.file_type().is_symlink() => {
                let _ = std::fs::remove_file(&link_path);
            }
            Ok(_) => continue, // a real file/dir — leave it alone
            Err(_) => {}
        }
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink(&target, &link_path);
        #[cfg(windows)]
        let _ = std::os::windows::fs::symlink_dir(&target, &link_path);
    }
}

/// Bazel's build-result format (all on STDERR — stdout is for data): `Target <label>
/// up-to-date:` + each DefaultInfo output, or `ERROR: …` on failure. The completion/elapsed
/// `INFO:` lines are emitted by the caller (which has the timing).
fn print_build_result(r: &BuildResult) {
    if let BuildStatus::Failed = r.status {
        eprintln!("ERROR: {}: build failed.", r.target);
        if let Some(m) = &r.message {
            eprintln!("  {m}");
        }
        return;
    }
    eprintln!("Target {} up-to-date:", r.target);
    for o in &r.outputs {
        eprintln!("  {}", o.path);
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
    fn bazel_build_compat_from_cli_and_razelrc() {
        // CLI flag sets the Opts bool; absent = off (no rc files written here).
        let ws = rc_ws("bbc", "", "");
        let cli = |extra: &[&str]| {
            let mut v = vec!["t".to_string(), "-C".to_string(), ws.display().to_string()];
            v.extend(extra.iter().map(|s| s.to_string()));
            parse_opts_with_rc(&["common", "build"], &v).unwrap()
        };
        assert!(!cli(&[]).bazel_build_compat, "default off");
        assert!(cli(&["--bazel_build_compat"]).bazel_build_compat, "CLI flag on");
        let _ = std::fs::remove_dir_all(&ws);

        // …and it is read from `.razelrc` — the rc bonus AND a direct proof `.razelrc` is read.
        let ws2 = rc_ws("bbc-rc", "", "common --bazel_build_compat\n");
        let args = vec!["t".into(), "-C".into(), ws2.display().to_string()];
        let o = parse_opts_with_rc(&["common", "build"], &args).unwrap();
        assert!(o.bazel_build_compat, ".razelrc `common --bazel_build_compat` honored");
        let _ = std::fs::remove_dir_all(&ws2);
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

    #[test]
    fn help_documents_only_supported_flags() {
        // Every documented flag is ACTUALLY handled (so help never advertises a
        // recognized-but-ignored Bazel flag) — help surface ⊆ support surface.
        for f in FLAG_HELP {
            assert!(
                HANDLERS.iter().any(|(n, _)| *n == f.name),
                "help documents `--{}` but no HANDLER backs it (would advertise an ignored flag)",
                f.name
            );
        }
        // Every command's flag refs resolve to a documented flag.
        for c in COMMANDS {
            for n in COMMON_FLAGS.iter().chain(c.flags.iter()) {
                assert!(flag_help(n).is_some(), "command `{}` refs undocumented flag `{n}`", c.name);
            }
        }
        // Every dispatched verb appears in the help index.
        for v in ["build", "run", "test", "clean", "affected", "subscribe", "version", "daemon", "help"] {
            assert!(COMMANDS.iter().any(|c| c.name == v), "verb `{v}` missing from help index");
        }
    }

    #[test]
    fn jobs_flag_reaches_global_flags_in_every_form() {
        // S5x: --jobs N / --jobs=N / -j N all land in GlobalFlags.jobs (→ execute_jobs).
        assert_eq!(p(&["build", "--jobs", "4"]).global_flags().jobs, 4, "--jobs N");
        assert_eq!(p(&["build", "--jobs=2"]).global_flags().jobs, 2, "--jobs=N");
        assert_eq!(p(&["build", "-j", "6"]).global_flags().jobs, 6, "-j N");
        // Default = 0 ⇒ execute_jobs treats it as serial (no behaviour change).
        assert_eq!(p(&["build"]).global_flags().jobs, 0, "unset = serial");
        // A non-numeric value is ignored (stays serial), never a parse error.
        assert_eq!(p(&["build", "--jobs", "xyz"]).global_flags().jobs, 0, "bad value = serial");
    }
}
