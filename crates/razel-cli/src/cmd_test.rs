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

use razel_build::GlobalFlags;
use razel_daemon::rpc::{self, Server};
use razel_wire::{
    BuildStatus, ImpactSet, encode,
};
use std::process::ExitCode;

// C3: ONE command-line parser, shared by the CLI + daemon (razel_loading::args, re-exported via
// razel-build). The CLI only wraps it to map the library's String parse error onto an ExitCode
// (parse_opts/parse_opts_with_rc below); Opts + its fields + global_flags + default_socket + rc-lite
// all come from the shared module, so the CLI and the daemon parse identically.
use razel_build::args::{self, Opts, default_socket};


use crate::*;

/// sysexits EX_USAGE — bad invocation (vs. EX failure for a real build error).
pub(crate) const EX_USAGE: u8 = 64;

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
        Some("shutdown") => cmd_shutdown(&args[1..]),
        // The exec-time build-script / rustc wrapper, folded into the razel binary so razel
        // self-invokes it (`razel process-wrapper rustc …`) via `current_exe()` — no separate
        // co-located tool. Internal: not user-facing, not in `cmd_help`.
        Some("process-wrapper") => cmd_process_wrapper(&args[1..]),
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

/// `razel process-wrapper <build-script|rustc> …` — the exec-time wrapper, folded into the razel
/// binary (was the standalone `razel-process-wrapper`). razel's own build actions spawn `razel
/// process-wrapper …` via `current_exe()`, so there is no separate co-located tool to resolve.
/// Internal/leaf: invoked by the executor, not by users.
pub(crate) fn cmd_process_wrapper(args: &[String]) -> ExitCode {
    match razel_process_wrapper::dispatch(args) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(e) => {
            eprintln!("razel process-wrapper: {e}");
            ExitCode::from(2)
        }
    }
}

/// Shown under every command.
pub(crate) static COMMON_FLAGS: &[&str] = &["workspace", "disk_cache"];

/// `razel help [<command>]` (Bazel `help`): no arg ⇒ the command index; a command ⇒ its usage
/// + the flags razel ACTUALLY honors for it. Recognized-but-ignored Bazel flags are NOT listed
/// (they self-diagnose if used) — the help surface is the supported surface.
pub(crate) fn cmd_help(args: &[String]) {
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

/// ExitCode-wrapping front-ends over the shared parser ([`razel_build::args`]) — the CLI's only
/// parser. They map the library's `String` parse error onto the CLI's `ExitCode`; `Opts`, its
/// fields, `global_flags`, `default_socket`, `bazel_build_compat_env`, and rc-lite all come from the
/// shared module, so the CLI and the daemon parse identically (the C3 single-parser goal).
pub(crate) fn parse_opts(args: &[String]) -> Result<Opts, ExitCode> {
    args::parse_opts(args).map_err(usage_err)
}

pub(crate) fn usage_err(e: String) -> ExitCode {
    eprintln!("razel: {e}");
    ExitCode::from(EX_USAGE)
}

/// `razel build //...` / multiple targets: expand patterns to concrete labels, build each
/// (one workspace lock + shared cache; the cache dedups shared deps), print a Bazel-style
/// per-target line + summary. Exit 1 if any target failed.
pub(crate) fn cmd_build_many(o: &Opts) -> ExitCode {
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
pub(crate) fn cmd_run(args: &[String]) -> ExitCode {
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
pub(crate) fn cmd_test(args: &[String]) -> ExitCode {
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

pub(crate) fn cmd_affected(args: &[String]) -> ExitCode {
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

pub(crate) fn cmd_query(args: &[String]) -> ExitCode {
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

pub(crate) fn cmd_daemon(args: &[String]) -> ExitCode {
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
    // The real daemon watches the workspace so source edits between builds reach the warm graph.
    match Server::new_watching(o.workspace, cache).serve(&socket) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("razel daemon: {e}");
            ExitCode::FAILURE
        }
    }
}

