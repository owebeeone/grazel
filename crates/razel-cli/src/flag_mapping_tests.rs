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

/// A flag razel ACTUALLY honors — the help surface. Kept ⊆ [`HANDLERS`] (the support
/// definition) by `help_documents_only_supported_flags`, so help never advertises a
/// recognized-but-ignored Bazel flag.
pub(crate) struct FlagHelp {
    pub(crate) name: &'static str,
    pub(crate) abbrev: Option<char>,
    pub(crate) arg: Option<&'static str>,
    pub(crate) desc: &'static str,
}

pub(crate) static FLAG_HELP: &[FlagHelp] = &[
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
    FlagHelp { name: "daemon", abbrev: None, arg: None, desc: "Require the warm daemon (error if none); default auto-spawns one with in-process fallback." },
    FlagHelp { name: "batch", abbrev: None, arg: None, desc: "Build in-process, never the daemon (also env RAZEL_BATCH=1)." },
    FlagHelp { name: "socket", abbrev: None, arg: Some("<path>"), desc: "Daemon socket path (default: <ws>/.razel-daemon.sock)." },
    FlagHelp { name: "cbor", abbrev: None, arg: None, desc: "Print the result as taut-wire CBOR (hex) instead of text." },
];

/// One CLI command: its argument shape, one-line summary, and the supported flags it honors.
pub(crate) struct CmdHelp {
    pub(crate) name: &'static str,
    pub(crate) args: &'static str,
    pub(crate) summary: &'static str,
    pub(crate) flags: &'static [&'static str],
}

pub(crate) static COMMANDS: &[CmdHelp] = &[
    CmdHelp { name: "build", args: "<target>...", summary: "Build the specified targets.",
        flags: &["compilation_mode", "copt", "cxxopt", "conlyopt", "linkopt", "define", "jobs", "bazel_build_compat", "daemon", "batch", "socket", "cbor"] },
    CmdHelp { name: "run", args: "<target> [-- args…]", summary: "Build, then run a target's output.",
        flags: &["compilation_mode", "copt", "cxxopt", "conlyopt", "linkopt", "define", "bazel_build_compat"] },
    CmdHelp { name: "test", args: "<target>...", summary: "Build and run the specified test targets.",
        flags: &["jobs", "compilation_mode", "copt", "cxxopt", "conlyopt", "linkopt", "define", "bazel_build_compat"] },
    CmdHelp { name: "clean", args: "[--expunge]", summary: "Remove build outputs + convenience symlinks (razel-out, razel-bin); keeps the cache for fast rebuilds (--expunge also wipes .razel-cache + deps).",
        flags: &["expunge"] },
    CmdHelp { name: "affected", args: "<file>...", summary: "List the targets affected by changed files.",
        flags: &["daemon", "socket", "cbor"] },
    CmdHelp { name: "subscribe", args: "", summary: "Stream build events from a running daemon.",
        flags: &["socket", "cbor"] },
    CmdHelp { name: "version", args: "", summary: "Print version information.",
        flags: &["daemon", "socket", "cbor"] },
    CmdHelp { name: "daemon", args: "", summary: "Run the razel build daemon.",
        flags: &["socket", "disk_cache"] },
    CmdHelp { name: "shutdown", args: "", summary: "Shut down the running build daemon for this workspace.",
        flags: &["socket"] },
    CmdHelp { name: "help", args: "[<command>]", summary: "Print help for a command, or this index.",
        flags: &[] },
];

pub(crate) fn flag_help(name: &str) -> Option<&'static FlagHelp> {
    FLAG_HELP.iter().find(|f| f.name == name)
}

/// `  -c, --compilation_mode <mode>   Compilation mode: …`
pub(crate) fn flag_line(f: &FlagHelp) -> String {
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
        // (The "documented flag ⊆ handled flag" check moved with the parser: the handler table now
        // lives in razel_loading::args and isn't exposed cross-crate, so the shared parser's own
        // tests cover handler coverage. The CLI-local consistency checks remain.)
        // Every command's flag refs resolve to a documented flag.
        for c in COMMANDS {
            for n in COMMON_FLAGS.iter().chain(c.flags.iter()) {
                assert!(flag_help(n).is_some(), "command `{}` refs undocumented flag `{n}`", c.name);
            }
        }
        // Every dispatched verb appears in the help index.
        for v in [
            "build", "run", "test", "clean", "affected", "subscribe", "version", "daemon",
            "shutdown", "help",
        ] {
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
