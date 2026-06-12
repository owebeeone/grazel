//! grazel-cli-lib — ALL grazel business logic (the thin-bin rule, §1d).
//!
//! Verb surface = razel's VERBATIM (delegated to the razel-cli LIB, §1d: same
//! parser, a razel flag can never behave differently under grazel) PLUS the
//! grazel-namespaced verbs below. Two deliberate shadows: `daemon` means
//! grazeld (this binary in daemon mode — the one-binary rule), and `version`
//! identifies THIS distribution.

pub mod daemon;
pub mod dial;
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
        _ => razel_cli::run(args),
    }
}

fn grazel_verb(args: &[String]) -> ExitCode {
    let (flags, rest) = split_args(args);
    let workspace = flags
        .workspace
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

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
                    workspace: workspace.clone(),
                    // grazeld runs INDEFINITELY by default (Gianni, 2026-06-12):
                    // it's the long-lived scope service (razeld keeps Bazel's
                    // idle-out posture; that's razel's lane). Proper service
                    // management (launchd/systemd) is the eventual home — debt D8.
                    // --idle-timeout stays as an opt-in (ws-test exercises it).
                    idle_timeout: flags.idle_timeout.map(Duration::from_secs),
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
