//! grazel-cli-lib — ALL grazel business logic (the thin-bin rule, §1d).
//!
//! The verb dispatch below is the S0 STUB (GrazelCrates.md): a minimal arg walk
//! that dies the day razel-cli's `[lib]` split arrives on razelv3 — grazel's CLI
//! surface then becomes razel's verbatim (same parser) plus grazel-namespaced
//! verbs. It gains no features in the meantime.

pub mod daemon;
pub mod paths;
pub mod scope;
pub mod wstest;

use paths::ScopePaths;
use std::path::PathBuf;

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

/// CLI entry: `args` excludes argv0. Returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    let mut flag_scope: Option<String> = None;
    let mut flag_workspace: Option<PathBuf> = None;
    let mut flag_stage: Option<String> = None;
    let mut verb: Vec<&str> = vec![];
    for a in args {
        if let Some(v) = a.strip_prefix("--scope=") {
            flag_scope = Some(v.to_string());
        } else if let Some(v) = a.strip_prefix("--workspace=") {
            flag_workspace = Some(PathBuf::from(v));
        } else if let Some(v) = a.strip_prefix("--stage=") {
            flag_stage = Some(v.to_string());
        } else {
            verb.push(a.as_str());
        }
    }
    let workspace = flag_workspace
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let result: Result<i32, String> = match verb.as_slice() {
        ["version"] => {
            println!(
                "grazel {} (wire protocol {})",
                daemon::BUILD_VERSION,
                razel_daemon::rpc::PROTOCOL
            );
            Ok(0)
        }
        // Debug/plumbing view of §1e resolution; key=value, consumed by ws-test.
        ["scope"] => resolve_paths(flag_scope.as_deref(), &workspace).map(|p| {
            println!("scope={}", p.scope);
            println!("socket={}", p.socket.display());
            println!("state={}", p.state_dir.display());
            0
        }),
        ["daemon", "run"] => resolve_paths(flag_scope.as_deref(), &workspace)
            .and_then(|p| daemon::run(&p, &workspace))
            .map(|()| 0),
        ["ws", "test"] => std::env::current_exe()
            .map_err(|e| format!("current_exe: {e}"))
            .map(|bin| {
                let tmp = std::env::temp_dir().join(format!("grazel-ws-test-{}", std::process::id()));
                let ctx = wstest::StageCtx { grazel_bin: bin, tmp };
                let ok = wstest::run_stages(&ctx, flag_stage.as_deref(), &mut std::io::stdout());
                if ok { 0 } else { 1 }
            }),
        _ => {
            eprintln!("usage: grazel [--scope=S] [--workspace=DIR] <version | scope | daemon run | ws test [--stage=NAME]>");
            Ok(2)
        }
    };
    result.unwrap_or_else(|e| {
        eprintln!("grazel: {e}");
        1
    })
}
