//! grazel-cli-lib — ALL grazel business logic (the thin-bin rule, §1d).
//!
//! Verb surface = razel's VERBATIM (delegated to the razel-cli LIB, §1d: same
//! parser, a razel flag can never behave differently under grazel) PLUS the
//! grazel-namespaced verbs below. Two deliberate shadows: `daemon` means
//! grazeld (this binary in daemon mode — the one-binary rule), and `version`
//! identifies THIS distribution.

pub mod daemon;
pub mod dial;
pub mod http;
pub mod paths;
pub mod scope;
pub mod views;
pub mod ws;
pub mod wstest;

use paths::ScopePaths;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Resolve scope + paths from flag/env/rc/cwd — the one dial-side entry every
/// verb shares (§1e: the scope only chooses WHICH daemon).
fn resolve_paths(flag_scope: Option<&str>, workspace: &std::path::Path) -> Result<ScopePaths, String> {
    let scope = scope::resolve(flag_scope, env_opt("GRAZEL_SCOPE").as_deref(), workspace)?;
    let home = paths::grazel_home(env_opt("GRAZEL_HOME").as_deref(), env_opt("HOME").as_deref())?;
    ScopePaths::new(&home, &scope)
}

/// Grazel-only argv, peeled before dispatch. Everything else passes through to
/// the razel parser untouched.
#[derive(Default)]
struct GrazelFlags {
    scope: Option<String>,
    workspace: Option<PathBuf>,
    stage: Option<String>,
    idle_timeout: Option<u64>,
    member_idle_timeout: Option<u64>,
    http_bind: Option<String>,
    view_buffer: Option<usize>,
    no_autostart: bool,
    all: bool,
    list: bool,
}

fn split_args(args: &[String]) -> (GrazelFlags, Vec<String>) {
    let mut flags = GrazelFlags::default();
    let mut rest = Vec::new();
    for a in args {
        if let Some(v) = a.strip_prefix("--scope=") {
            flags.scope = Some(v.to_string());
        } else if let Some(v) = a.strip_prefix("--workspace=") {
            flags.workspace = Some(PathBuf::from(v));
        } else if let Some(v) = a.strip_prefix("--stage=") {
            flags.stage = Some(v.to_string());
        } else if let Some(v) = a.strip_prefix("--idle-timeout=") {
            flags.idle_timeout = v.parse().ok();
        } else if let Some(v) = a.strip_prefix("--member-idle-timeout=") {
            flags.member_idle_timeout = v.parse().ok();
        } else if let Some(v) = a.strip_prefix("--http-bind=") {
            flags.http_bind = Some(v.to_string());
        } else if let Some(v) = a.strip_prefix("--view-buffer=") {
            flags.view_buffer = v.parse().ok();
        } else if a == "--no_autostart" || a == "--no-autostart" {
            flags.no_autostart = true;
        } else if a == "--all" {
            flags.all = true;
        } else if a == "--list" {
            flags.list = true;
        } else {
            rest.push(a.clone());
        }
    }
    (flags, rest)
}

/// CLI entry: `args` excludes argv0. Routing decides FIRST, peeling second:
/// grazel-only flags (`--scope`, `--workspace=`, …) are peeled only off grazel
/// verbs — razel verbs pass through VERBATIM, because razel has its own
/// `--workspace`/`-C` and §1d forbids a razel flag meaning anything different
/// under grazel. (Scope routing for razel verbs arrives with GR3.)
pub fn run(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        // The grazel namespace. `daemon` and `version` deliberately shadow
        // razel's: daemon = grazeld (one-binary rule), version = this distribution.
        Some("scope" | "ws" | "version" | "daemon" | "shutdown") => grazel_verb(args),
        // GR3: build/affected go THROUGH the scope daemon (GrazelVerbs.md) —
        // §1e posture; flag semantics stay razel's verbatim.
        Some("build" | "affected") => routed_razel_verb(args),
        // GR3b: run streams the invocation through grazeld (§4b client side).
        Some("run") => grazel_run(&args[1..]),
        // grazel's own help index; razel-surface verbs delegate to razel's help.
        Some("help") => grazel_help(&args[1..]),
        _ => razel_cli::run(args),
    }
}

/// `grazel help [<command>]` (Bazel `help` shape) — grazel's verb index + per-command help.
/// razel-surface verbs (routed verbatim) delegate to razel's own supported-only help; the
/// grazel-flavored verbs are documented here. Like razel, only flags that take effect appear.
fn grazel_help(args: &[String]) -> ExitCode {
    // Routed verbatim to razel → razel's flag help applies unchanged.
    const RAZEL_SURFACE: &[&str] = &["build", "affected", "test", "clean", "subscribe"];
    // (name, args, summary, supported options) for the grazel-flavored verbs.
    const GRAZEL_VERBS: &[(&str, &str, &str, &str)] = &[
        ("run", "<target> [-- args…]", "Build & run a target, streamed through grazeld.", "--scope=<s>, -C/--workspace=<dir>, --no_daemon (razel-local)"),
        ("scope", "[--list]", "Resolve or list build scopes.", "--scope=<s>, -C/--workspace=<dir>, --list"),
        ("ws", "test [--stage=N|--list]", "Workstream acceptance instrument.", "--stage=<n>, --list, --scope=<s>"),
        ("daemon", "run|ping|stop|status", "Run/control grazeld (the one-binary node).", "--scope=<s>, -C/--workspace=<dir>"),
        ("shutdown", "[--all]", "Stop grazeld scope daemon(s).", "--all, --scope=<s>"),
        ("version", "", "Print grazel (this distribution) version.", ""),
        ("help", "[<command>]", "Print help for a command, or this index.", ""),
    ];
    if let Some(cmd) = args.first().map(String::as_str) {
        if RAZEL_SURFACE.contains(&cmd) {
            return razel_cli::run(&["help".to_string(), cmd.to_string()]);
        }
        if let Some((n, a, s, opts)) = GRAZEL_VERBS.iter().find(|(n, ..)| *n == cmd) {
            println!("Usage: grazel {n} {a}\n\n{s}");
            if !opts.is_empty() {
                println!("\nSupported options: {opts}");
            }
            return ExitCode::SUCCESS;
        }
        eprintln!("grazel: unknown command {cmd:?}\n");
    }
    println!("grazel — razel + iroh build node (one binary)\n");
    println!("Usage: grazel <command> <options> ...\n");
    println!("grazel commands:");
    for (n, _, s, _) in GRAZEL_VERBS {
        println!("  {n:<10} {s}");
    }
    println!("\nrazel verbs (routed through grazeld; razel's surface verbatim):");
    for v in RAZEL_SURFACE {
        println!("  {v:<10} (run `grazel help {v}`)");
    }
    println!("\nGetting more help:\n  grazel help <command>   Print help and the supported options for <command>.");
    ExitCode::SUCCESS
}

/// `grazel run <target> [-- prog args…]` — the streamed dev-loop verb. Flag
/// surface is deliberately narrow (`--scope`, `-C`/`--workspace`) until razel-cli
/// itself grows a daemon-routed run to delegate to.
fn grazel_run(args: &[String]) -> ExitCode {
    // --no_daemon (survey P1): razel's own local run verb, verbatim.
    if args.iter().any(|a| a == "--no_daemon" || a == "--no-daemon") {
        let mut rest: Vec<String> = vec!["run".into()];
        rest.extend(
            args.iter()
                .filter(|a| {
                    a.as_str() != "--no_daemon"
                        && a.as_str() != "--no-daemon"
                        && !a.starts_with("--scope=")
                })
                .cloned(),
        );
        return razel_cli::run(&rest);
    }
    let mut flag_scope = None;
    let mut target = None;
    let mut prog_args: Vec<String> = vec![];
    let mut workspace = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--" {
            prog_args = it.cloned().collect();
            break;
        } else if let Some(v) = a.strip_prefix("--scope=") {
            flag_scope = Some(v.to_string());
        } else if a == "-C" || a == "--workspace" {
            workspace = it.next().map(PathBuf::from);
        } else if let Some(v) = a.strip_prefix("--workspace=") {
            workspace = Some(PathBuf::from(v));
        } else if target.is_none() && !a.starts_with('-') {
            target = Some(a.clone());
        } else {
            eprintln!("grazel run: unsupported argument {a:?} (target, --scope, -C, and `-- args` for now)");
            return ExitCode::from(64);
        }
    }
    let Some(target) = target else {
        eprintln!("grazel run: expected <target> [-- args…]");
        return ExitCode::from(64);
    };
    let workspace = workspace
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let workspace = workspace.canonicalize().unwrap_or(workspace);
    let outcome = std::env::current_exe()
        .map_err(|e| format!("current_exe: {e}"))
        .and_then(|bin| {
            let p = resolve_paths(flag_scope.as_deref(), &workspace)?;
            dial::run_streamed(&p, &workspace, &target, &prog_args, &bin)
        });
    match outcome {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("grazel: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Daemon-route a razel verb: peel ONLY the grazel-namespaced flags (`--scope`,
/// `--no_daemon`), ensure the scope daemon (hello → membership → output lock),
/// delegate with `--daemon --socket <scope socket>` injected. Explicit user
/// routing (`--daemon`/`--socket` present) wins: delegate VERBATIM, inject
/// nothing. `--no_daemon` (survey P1) is the escape hatch: razel-LOCAL
/// semantics, no daemon started, no injection — razel's own default behavior.
fn routed_razel_verb(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "--daemon" || a == "--socket" || a.starts_with("--socket=")) {
        return razel_cli::run(args);
    }
    let mut flag_scope = None;
    let mut no_daemon = false;
    let mut rest: Vec<String> = Vec::with_capacity(args.len());
    for a in args {
        if let Some(v) = a.strip_prefix("--scope=") {
            flag_scope = Some(v.to_string());
        } else if a == "--no_daemon" || a == "--no-daemon" {
            no_daemon = true;
        } else {
            rest.push(a.clone());
        }
    }
    if no_daemon {
        return razel_cli::run(&rest);
    }
    // Workspace for SCOPE RESOLUTION only — razel's -C/--workspace is scanned
    // non-destructively; the flag itself still reaches razel's parser.
    let mut workspace = None;
    let mut it = rest.iter().peekable();
    while let Some(a) = it.next() {
        if a == "-C" || a == "--workspace" {
            workspace = it.peek().map(|v| PathBuf::from(*v));
        } else if let Some(v) = a.strip_prefix("--workspace=") {
            workspace = Some(PathBuf::from(v));
        }
    }
    let workspace = workspace
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let workspace = workspace.canonicalize().unwrap_or(workspace);

    let routed = std::env::current_exe()
        .map_err(|e| format!("current_exe: {e}"))
        .and_then(|bin| {
            let p = resolve_paths(flag_scope.as_deref(), &workspace)?;
            dial::ensure(&p, &workspace, true, &bin)?;
            Ok(p.socket)
        });
    match routed {
        Ok(socket) => {
            rest.push("--daemon".into());
            rest.push("--socket".into());
            rest.push(socket.display().to_string());
            razel_cli::run(&rest)
        }
        Err(e) => {
            eprintln!("grazel: {e}");
            ExitCode::FAILURE
        }
    }
}

fn grazel_verb(args: &[String]) -> ExitCode {
    let (flags, rest) = split_args(args);
    let workspace = flags
        .workspace
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    // Canonical roots everywhere (member identity, locks): /var vs /private/var
    // on macOS must not look like two workspaces.
    let workspace = workspace.canonicalize().unwrap_or(workspace);

    let verbs: Vec<&str> = rest.iter().map(String::as_str).collect();
    let result: Result<ExitCode, String> = match verbs.as_slice() {
        ["version"] => {
            println!(
                "grazel {} (wire protocol {})",
                daemon::BUILD_VERSION,
                razel_daemon::rpc::PROTOCOL
            );
            Ok(ExitCode::SUCCESS)
        }
        // Survey P2: enumerate every scope under the grazel home, liveness-checked.
        ["scope"] if flags.list => {
            paths::grazel_home(env_opt("GRAZEL_HOME").as_deref(), env_opt("HOME").as_deref())
                .map(|home| {
                    let mut names = std::collections::BTreeSet::new();
                    for dir in [home.join(".uds"), home.join("scopes")] {
                        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                            names.insert(entry.file_name().to_string_lossy().to_string());
                        }
                    }
                    if names.is_empty() {
                        println!("no scopes under {}", home.display());
                    }
                    for name in names {
                        let alive = paths::ScopePaths::new(&home, &name)
                            .map(|p| {
                                daemon::status_lines(&p).iter().any(|l| l == "alive=true")
                            })
                            .unwrap_or(false);
                        println!("scope={name} alive={alive}");
                    }
                    ExitCode::SUCCESS
                })
        }
        // Survey P2: read-only status — works on a wedged daemon (no wire calls).
        ["daemon", "status"] => resolve_paths(flags.scope.as_deref(), &workspace).map(|p| {
            for line in daemon::status_lines(&p) {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }),
        // Debug/plumbing view of §1e resolution; key=value, consumed by ws-test.
        ["scope"] => resolve_paths(flags.scope.as_deref(), &workspace).map(|p| {
            println!("scope={}", p.scope);
            println!("socket={}", p.socket.display());
            println!("state={}", p.state_dir.display());
            ExitCode::SUCCESS
        }),
        ["daemon", "run"] => resolve_paths(flags.scope.as_deref(), &workspace)
            .and_then(|p| {
                daemon::run(daemon::ServeOpts {
                    paths: p,
                    // grazeld runs INDEFINITELY by default (Gianni, 2026-06-12):
                    // it's the long-lived scope service (razeld keeps Bazel's
                    // idle-out posture; that's razel's lane). Proper service
                    // management (launchd/systemd) is the eventual home — debt D8.
                    // --idle-timeout stays as an opt-in (ws-test exercises it).
                    idle_timeout: flags.idle_timeout.map(Duration::from_secs),
                    member_idle_timeout: Duration::from_secs(
                        flags.member_idle_timeout.unwrap_or(30 * 60),
                    ),
                    http_bind: flags.http_bind.clone(),
                    view_buffer: flags.view_buffer.unwrap_or(256),
                })
            })
            .map(|()| ExitCode::SUCCESS),
        ["daemon", "ping"] => std::env::current_exe()
            .map_err(|e| format!("current_exe: {e}"))
            .and_then(|bin| {
                let p = resolve_paths(flags.scope.as_deref(), &workspace)?;
                let out = dial::ensure(&p, &workspace, !flags.no_autostart, &bin)?;
                println!("scope={}", p.scope);
                println!("socket={}", p.socket.display());
                println!("pid={}", out.pid);
                println!("version={}", out.version.version);
                println!("protocol={}", out.version.protocol);
                println!("restarted={}", out.restarted);
                Ok(ExitCode::SUCCESS)
            }),
        ["daemon", "stop"] => resolve_paths(flags.scope.as_deref(), &workspace).and_then(|p| {
            let stopped = dial::stop(&p)?;
            println!(
                "scope={} {}",
                p.scope,
                if stopped { "stopped" } else { "no daemon running" }
            );
            Ok(ExitCode::SUCCESS)
        }),
        // The user-facing stop (Gianni, inbox 0007): no-idle-out makes lingering
        // daemons normal; this is the product answer to "kill it by pid".
        ["shutdown"] if flags.all => {
            if flags.scope.is_some() {
                Err("--all and --scope are mutually exclusive".into())
            } else {
                paths::grazel_home(env_opt("GRAZEL_HOME").as_deref(), env_opt("HOME").as_deref())
                    .map(|home| {
                        let uds = home.join(".uds");
                        let mut any = false;
                        for entry in std::fs::read_dir(&uds).into_iter().flatten().flatten() {
                            let scope = entry.file_name().to_string_lossy().to_string();
                            let Ok(p) = paths::ScopePaths::new(&home, &scope) else { continue };
                            any = true;
                            match dial::stop(&p) {
                                Ok(true) => println!("scope={scope} stopped"),
                                Ok(false) => println!("scope={scope} no daemon running"),
                                Err(e) => eprintln!("scope={scope} error: {e}"),
                            }
                        }
                        if !any {
                            println!("no scope daemons found under {}", uds.display());
                        }
                        ExitCode::SUCCESS
                    })
            }
        }
        ["shutdown"] => resolve_paths(flags.scope.as_deref(), &workspace).and_then(|p| {
            let stopped = dial::stop(&p)?;
            println!(
                "scope={} {}",
                p.scope,
                if stopped { "stopped" } else { "no daemon running" }
            );
            Ok(ExitCode::SUCCESS)
        }),
        ["ws", "test"] if flags.list => {
            for s in wstest::stages() {
                println!("{}", s.name);
            }
            Ok(ExitCode::SUCCESS)
        }
        ["ws", "test"] => std::env::current_exe()
            .map_err(|e| format!("current_exe: {e}"))
            .map(|bin| {
                let tmp = std::env::temp_dir().join(format!("gwt-{}", std::process::id()));
                let ctx = wstest::StageCtx { grazel_bin: bin, tmp };
                let ok = wstest::run_stages(&ctx, flags.stage.as_deref(), &mut std::io::stdout());
                if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
            }),
        _ => {
            eprintln!(
                "usage: grazel <scope [--list] | shutdown [--all] | daemon run|ping|stop|status | ws test [--stage=N|--list]> [--scope=S] [--workspace=DIR]\n       or any razel verb (build/affected/subscribe/…) — razel's surface verbatim; --no_daemon for razel-local semantics"
            );
            Ok(ExitCode::from(64)) // EX_USAGE, razel's convention
        }
    };
    result.unwrap_or_else(|e| {
        eprintln!("grazel: {e}");
        ExitCode::FAILURE
    })
}
