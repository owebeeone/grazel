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
    GlobalFlags, build_workspace_with, config_segment, resolve_build_file,
};
use razel_core::Digest;
use razel_exec::Cache;
use razel_wire::{
    BuildResult, BuildStatus, OutputArtifact,
};
use std::path::Path;
use std::process::ExitCode;

// C3: ONE command-line parser, shared by the CLI + daemon (razel_loading::args, re-exported via
// razel-build). The CLI only wraps it to map the library's String parse error onto an ExitCode
// (parse_opts/parse_opts_with_rc below); Opts + its fields + global_flags + default_socket + rc-lite
// all come from the shared module, so the CLI and the daemon parse identically.
use razel_build::args::Opts;



/// One test target's verdict.
pub(crate) enum TestOutcome {
    Passed(f64),
    Failed(f64, String),
    BuildError(String),
}

/// Build + exec one test target, capture `test.log`, return the verdict. A build failure or a
/// missing runnable output is a `BuildError` (exit 1, never the tests-failed code).
pub(crate) fn run_one_test(o: &Opts, target_arg: &str, cache: &Cache, flags: GlobalFlags) -> TestOutcome {
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

/// Build ONE target against an already-open `cache` with explicit `flags` — the CALLER holds
/// the workspace lock. Factored from [`local_build`] so the `test` batch holds the lock once
/// and builds many targets (each with `flags.jobs = 1`; cross-test parallelism is the batch
/// pool's job, not the per-build executor's).
pub(crate) fn build_one(
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
        // Bare name / :name → a target in the workspace's ROOT package. Confirm a root BUILD exists
        // (clear error otherwise), then route through the WORKSPACE path so cross-package aliases are
        // FOLLOWED (e.g. `//:razel` → `//crates/razel-cli:razel`) and dependency packages load on
        // demand. The old single-package `build_bazel_with` analyzed an alias to an empty,
        // action-less target — which `build_one` then reported as a vacuous "up-to-date". (RG 0011:
        // canonical BUILD.bazel-over-BUILD discovery.)
        let name = target_arg.rsplit(':').next().unwrap_or(target_arg);
        match resolve_build_file(&o.workspace, flags.strict_bazel) {
            Ok(Some(_)) => {}
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
        }
        build_workspace_with(&o.workspace, &format!("//:{name}"), cache, flags)
    };

    Ok(match report {
        Ok(report) => {
            // The target's DefaultInfo, not every intermediate (bazel semantics — `run` execs
            // outputs[0]); empty default_info falls back to produced.
            let effective: &[String] = if report.default_outputs.is_empty() {
                &report.produced
            } else {
                &report.default_outputs
            };
            if report.executed == 0 && effective.is_empty() {
                // 0 actions AND 0 outputs ⇒ nothing was built. Do NOT report "up-to-date": that is
                // a vacuous success (the silent lie). It happens for an action-less target — e.g. an
                // `alias` the bare-name path didn't follow across packages (see build_one's bare
                // branch), or a stub target. Fail loudly so `clean && build X` can't claim success
                // while producing nothing.
                BuildResult {
                    target: target_arg.to_string(),
                    status: BuildStatus::Failed,
                    recomputes: 0,
                    outputs: vec![],
                    message: Some(format!(
                        "`{target_arg}` ran no actions and produced no outputs — nothing was built \
                         (an alias or action-less target?), so it is not up-to-date"
                    )),
                }
            } else {
                BuildResult {
                    target: target_arg.to_string(),
                    // executed == 0 (with outputs present) → fully served from cache.
                    status: if report.executed == 0 {
                        BuildStatus::Cached
                    } else {
                        BuildStatus::Built
                    },
                    recomputes: report.executed as i64,
                    outputs: effective
                        .iter()
                        .map(|p| OutputArtifact {
                            path: p.clone(),
                            digest: digest_of(&o.workspace.join(p)),
                        })
                        .collect(),
                    message: None,
                }
            }
        }
        Err(e) => BuildResult {
            target: target_arg.to_string(),
            status: BuildStatus::Failed,
            recomputes: 0,
            outputs: vec![],
            message: Some(e),
        },
    })
}

pub(crate) fn digest_of(path: &Path) -> Vec<u8> {
    match std::fs::read(path) {
        Ok(bytes) => Digest::of(&bytes).as_bytes().to_vec(),
        Err(_) => vec![], // output not on disk (e.g. restored elsewhere) — empty digest
    }
}

