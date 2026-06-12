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
    no_autostart: bool,
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
        } else if a == "--no_autostart" || a == "--no-autostart" {
            flags.no_autostart = true;
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
        Some("scope" | "ws" | "version" | "daemon") => grazel_verb(args),
        // GR3: build/affected go THROUGH the scope daemon (GrazelVerbs.md) —
        // §1e posture; flag semantics stay razel's verbatim.
        Some("build" | "affected") => routed_razel_verb(args),
        _ => razel_cli::run(args),
    }
}

/// Daemon-route a razel verb: peel ONLY `--scope` (grazel-namespaced), ensure
/// the scope daemon (hello → membership → output lock), delegate with
/// `--daemon --socket <scope socket>` injected. Explicit user routing
/// (`--daemon`/`--socket` present) wins: delegate VERBATIM, inject nothing.
fn routed_razel_verb(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "--daemon" || a == "--socket" || a.starts_with("--socket=")) {
        return razel_cli::run(args);
    }
    let mut flag_scope = None;
    let mut rest: Vec<String> = Vec::with_capacity(args.len());
    for a in args {
        match a.strip_prefix("--scope=") {
            Some(v) => flag_scope = Some(v.to_string()),
            None => rest.push(a.clone()),
        }
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
                "usage: grazel <scope | daemon run|ping|stop | ws test [--stage=N]> [--scope=S] [--workspace=DIR]\n       or any razel verb (build/affected/subscribe/…) — razel's surface verbatim"
            );
            Ok(ExitCode::from(64)) // EX_USAGE, razel's convention
        }
    };
    result.unwrap_or_else(|e| {
        eprintln!("grazel: {e}");
        ExitCode::FAILURE
    })
}
