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
    GlobalFlags, build_workspace_with, config_segment, convenience_symlinks, resolve_build_file,
};
use razel_core::Digest;
use razel_daemon::rpc::{self, Server};
use razel_exec::Cache;
use razel_wire::{
    BuildResult, BuildState, BuildStatus, Hello, ImpactSet, InvocationEvent, OutputArtifact,
    VersionInfo, encode,
};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

// C3: ONE command-line parser, shared by the CLI + daemon (razel_loading::args, re-exported via
// razel-build). The CLI only wraps it to map the library's String parse error onto an ExitCode
// (parse_opts/parse_opts_with_rc below); Opts + its fields + global_flags + default_socket + rc-lite
// all come from the shared module, so the CLI and the daemon parse identically.
use razel_build::args::{self, Opts, bazel_build_compat_env, default_socket};


use crate::*;

/// Wire protocol revision reported by `version` (bumped on breaking IR changes).
pub(crate) const PROTOCOL: i64 = 1;
pub(crate) fn parse_opts_with_rc(commands: &[&str], args: &[String]) -> Result<Opts, ExitCode> {
    args::parse_opts_with_rc(commands, args).map_err(usage_err)
}

pub(crate) fn cmd_version(args: &[String]) -> ExitCode {
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

/// A Bazel target PATTERN (expands to many targets) vs a concrete label.
pub(crate) fn is_pattern(t: &str) -> bool {
    t.contains("...") || t.ends_with(":all")
}

pub(crate) fn cmd_build(args: &[String]) -> ExitCode {
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

    // A build isn't silent: announce the target up front (so even a fully-cached build shows it's
    // working before the elapsed/result lines); per-action progress streams during execution.
    if !o.cbor {
        eprintln!("Building {target_arg} …");
    }
    let t0 = std::time::Instant::now();
    // WS-E (bazel model): default to the WARM per-workspace daemon — auto-spawn one if needed —
    // so a no-op rebuild is ~instant instead of re-hashing every input (~34s). `--batch` (or
    // `RAZEL_BATCH=1`) forces in-process; `--daemon` REQUIRES the daemon (no fallback); the default
    // tries the daemon and falls back to in-process if none can be reached, so builds never break.
    let socket = o
        .socket
        .clone()
        .unwrap_or_else(|| default_socket(&o.workspace));
    let force_batch = o.batch || std::env::var_os("RAZEL_BATCH").is_some();
    let result = if force_batch {
        match local_build(&o, &target_arg) {
            Ok(r) => r,
            Err(c) => return c,
        }
    } else if ensure_daemon(&o, &socket) {
        // C3: forward the raw build args + cwd; the daemon parses them server-side. Stream per-action
        // progress back so a daemon build isn't silent (WS-E.2).
        let cwd = std::env::current_dir().unwrap_or_else(|_| o.workspace.clone());
        match daemon_build_streamed(&socket, args, &cwd.to_string_lossy(), &target_arg, o.cbor) {
            Ok(r) => r,
            // The daemon died mid-stream. "Builds never break": fall back to in-process unless the
            // user explicitly required the daemon with --daemon.
            Err(()) if o.daemon => {
                eprintln!("razel: daemon became unreachable mid-build ({})", socket.display());
                return ExitCode::FAILURE;
            }
            Err(()) => {
                eprintln!("razel: daemon unavailable, building in-process");
                match local_build(&o, &target_arg) {
                    Ok(r) => r,
                    Err(c) => return c,
                }
            }
        }
    } else if o.daemon {
        // Explicit --daemon: don't silently fall back to a cold in-process build.
        eprintln!("razel: --daemon set but no daemon could be reached at {}", socket.display());
        return ExitCode::FAILURE;
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

pub(crate) fn local_build(o: &Opts, target_arg: &str) -> Result<BuildResult, ExitCode> {
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

/// One request/response to the daemon; unwraps the payload or prints the error.
/// Ensure a warm daemon is reachable at `socket`: reuse a running one, else AUTO-SPAWN a detached
/// `razel daemon` for this workspace and wait briefly for it to bind. Returns false if none could
/// be reached — the caller then falls back to an in-process build, so builds never break (WS-E,
/// bazel model: a per-workspace server is the default, with `RAZEL_BATCH=1` to force in-process).
pub(crate) fn ensure_daemon(o: &Opts, socket: &Path) -> bool {
    use std::process::{Command, Stdio};
    use std::time::Duration;
    // Probe with `hello`, NOT bare `version`: it validates that the daemon at this socket serves
    // THIS workspace + a matching protocol (do_hello) AND reports its binary identity (`build_id`),
    // so we reuse only a daemon that is the SAME build as this CLI. A daemon running a different (or
    // too-old) build of our workspace is auto-restarted — the warm server can never serve results
    // from code the CLI no longer runs.
    let hello = Hello {
        protocol: PROTOCOL,
        build_version: env!("CARGO_PKG_VERSION").to_string(),
        workspace_root: o.workspace.to_string_lossy().to_string(),
    };
    // What build this CLI is: `<ver>+<exe build_id>`, compared against the daemon's hello reply.
    let my_build = format!("{}+{}", env!("CARGO_PKG_VERSION"), razel_daemon::rpc::exe_build_id());
    enum Probe {
        Mine,    // ours + same build → reuse
        Skew,    // ours (workspace ok) but a DIFFERENT build → restart
        Foreign, // rejected us (different workspace, or a daemon too old to speak `hello`)
        Down,    // nothing bound
    }
    let probe = |sock: &Path| -> Probe {
        match rpc::call(sock, &rpc::req_hello(&hello)) {
            Ok(resp) => match rpc::payload(&resp) {
                Ok(p) => {
                    if razel_wire::VersionInfo::from_cbor(&p).version == my_build {
                        Probe::Mine
                    } else {
                        Probe::Skew
                    }
                }
                Err(_) => Probe::Foreign,
            },
            Err(_) => Probe::Down,
        }
    };
    let explicit_socket = o.socket.is_some();
    match probe(socket) {
        Probe::Mine => return true,
        Probe::Skew => {
            // Same workspace, different build than this CLI → restart so the server matches the CLI.
            eprintln!("razel: daemon at {} is a different build; restarting it", socket.display());
            if matches!(stop_daemon(socket, &o.workspace), StopOutcome::Failed) {
                eprintln!("razel: could not stop the old daemon; building in-process");
                return false;
            }
        }
        Probe::Foreign if explicit_socket => {
            // A user-named `--socket` that rejected us is a DIFFERENT workspace's daemon — leave it
            // alone and build in-process (we can't bind over its socket).
            eprintln!(
                "razel: existing daemon at {} rejected this workspace; building in-process",
                socket.display()
            );
            return false;
        }
        Probe::Foreign => {
            // Our OWN per-workspace socket, but the daemon there can't speak our `hello` — a stale
            // build that predates the handshake (exactly the skew this guards against). Replace it.
            eprintln!("razel: stale daemon at {}; restarting it", socket.display());
            if matches!(stop_daemon(socket, &o.workspace), StopOutcome::Failed) {
                eprintln!("razel: could not stop the stale daemon; building in-process");
                return false;
            }
        }
        Probe::Down => {} // nothing bound → spawn one
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let cache = o
        .cache
        .clone()
        .unwrap_or_else(|| o.workspace.join(".razel-cache"));
    // Daemon stdout/stderr → a workspace log file (never the client's tty); the daemon outlives
    // this client process. Two append handles to the same file (try_clone-free).
    let logpath = o.workspace.join(".razel-daemon.log");
    let open_log = || {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&logpath)
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null())
    };
    let spawned = Command::new(exe)
        .arg("daemon")
        .arg("-C")
        .arg(&o.workspace)
        .arg("--socket")
        .arg(socket)
        .arg("--disk_cache")
        .arg(&cache)
        .stdin(Stdio::null())
        .stdout(open_log())
        .stderr(open_log())
        .spawn();
    if spawned.is_err() {
        return false;
    }
    // Poll for OUR daemon to come up + accept this workspace as the matching build (~5s budget).
    for _ in 0..200 {
        std::thread::sleep(Duration::from_millis(25));
        if matches!(probe(socket), Probe::Mine) {
            return true;
        }
    }
    false
}

pub(crate) fn daemon_call(socket: &Path, req: &razel_wire::Cbor) -> Result<razel_wire::Cbor, ExitCode> {
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

pub(crate) fn hex(bytes: &[u8]) -> String {
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
pub(crate) fn ensure_convenience_symlinks(workspace: &std::path::Path, flags: &GlobalFlags) {
    let mut links = convenience_symlinks(flags);
    // When the build ran in the exec-root forest (`.razel-exec`, used when `@crates` is materialized),
    // outputs land under `.razel-exec/<out>`, not the workspace — so the reported `…-out/…` paths and
    // the `…-bin` symlink would dangle. Surface the output tree at the workspace `<out>` (like Bazel's
    // `bazel-out` → the output base) so both resolve.
    if workspace.join(".razel-crates").is_dir() {
        let out = if flags.bazel_build_compat { "bazel-out" } else { "razel-out" };
        links.push((out.to_string(), format!(".razel-exec/{out}")));
    }
    for (link, target) in links {
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
    fn clean_removes_real_exec_root_outputs_keeps_crates_until_expunge() {
        let ws = std::env::temp_dir().join(format!("razel-clean-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        // The real outputs live in the exec-root forest; razel-out is a convenience symlink into it.
        std::fs::create_dir_all(ws.join(".razel-exec/razel-out")).unwrap();
        std::fs::write(ws.join(".razel-exec/razel-out/bin"), "x").unwrap();
        std::fs::create_dir_all(ws.join(".razel-crates/somecrate")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(".razel-exec/razel-out", ws.join("razel-out")).unwrap();
        let arg = ws.to_string_lossy().to_string();

        // Plain clean: removes the REAL exec-root outputs + the convenience symlink; KEEPS the
        // expensive fetched external deps.
        cmd_clean(&["-C".into(), arg.clone()]);
        assert!(!ws.join(".razel-exec").exists(), "clean removed the real exec-root storage");
        assert!(
            std::fs::symlink_metadata(ws.join("razel-out")).is_err(),
            "clean removed the convenience symlink"
        );
        assert!(ws.join(".razel-crates").exists(), "a plain clean keeps fetched external deps");

        // --expunge additionally drops the fetched external deps (bazel parity).
        cmd_clean(&["--expunge".into(), "-C".into(), arg]);
        assert!(!ws.join(".razel-crates").exists(), "--expunge removes the external deps");

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn rc_lite_scopes_inherits_and_layers() {
        // Through the SHARED parser (parse_opts_with_rc): command scoping + `common` + comments;
        // `.razelrc` layers AFTER `.bazelrc`; a `test`-scoped line stays out of a `build`.
        let ws = rc_ws(
            "scope",
            "# comment\ncommon --jobs=3\nbuild --copt=-Wall\ntest --copt=-WTEST\n",
            "build --copt=-Wextra\n",
        );
        let args = vec!["t".into(), "-C".into(), ws.display().to_string()];
        let o = parse_opts_with_rc(&["common", "build"], &args).unwrap();
        assert_eq!(o.jobs, 3, "`common` scope applies");
        assert!(o.copts.contains(&"-Wall".to_string()), "`build` scope (.bazelrc)");
        assert!(
            o.copts.contains(&"-Wextra".to_string()),
            ".razelrc layered after .bazelrc"
        );
        assert!(
            !o.copts.contains(&"-WTEST".to_string()),
            "`test`-scoped line excluded from a build"
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
        // No rc files: parsing succeeds and applies no extra flags (defaults intact).
        let ws = rc_ws("none", "", "");
        let args = vec!["t".into(), "-C".into(), ws.display().to_string()];
        let o = parse_opts_with_rc(&["common", "build"], &args).unwrap();
        assert_eq!(o.jobs, 0);
        assert!(o.copts.is_empty());
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

