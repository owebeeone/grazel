//! Razel's command-line argument parser — the SINGLE source of truth shared by the CLI
//! (`--batch` in-process) and the daemon (which parses the client-forwarded args server-side,
//! C3). Moved here from `razel-cli` so daemon == local == bazel parse identically (the parity
//! property G2 depends on). Returns `Result<_, String>` (no `process::ExitCode` in a library);
//! `razel-cli` maps the error string to an exit code. Diagnostics for deprecated/unsupported
//! flags go to stderr (best-effort; they do not affect the parsed result).
//!
//! Driven entirely by the flag tables ([`crate::bazel_flags::BAZEL_FLAGS`] + [`RAZEL_FLAGS`]),
//! so adding a row — not editing parse logic — is how a flag becomes supported.

use crate::GlobalFlags;
use crate::bazel_flags::{BAZEL_FLAGS, FlagSpec};
use std::path::{Path, PathBuf};

/// Parsed flags shared across subcommands.
#[derive(Default)]
pub struct Opts {
    pub workspace: PathBuf,
    pub cache: Option<PathBuf>,
    pub socket: Option<PathBuf>,
    pub daemon: bool,
    pub cbor: bool,
    /// `-c` / `--compilation_mode` (fastbuild|dbg|opt).
    pub compilation_mode: Option<String>,
    /// Global cc flags: `--copt`/`--cxxopt`/`--conlyopt`, `--define` (as `-D`).
    pub copts: Vec<String>,
    pub cxxopts: Vec<String>,
    pub conlyopts: Vec<String>,
    pub defines: Vec<String>,
    /// `--linkopt`.
    pub linkopts: Vec<String>,
    /// `--jobs`/`-j`: parallel-executor concurrency (0 ⇒ serial default).
    pub jobs: usize,
    /// `clean --expunge` (Bazel): the more-thorough clean.
    pub expunge: bool,
    /// `--bazel_build_compat` (razel-only): write outputs to Bazel's `bazel-out/` tree.
    /// Also set by `RAZEL_BAZEL_BUILD_COMPAT` (`1`/`T`), merged in [`Opts::global_flags`].
    pub bazel_build_compat: bool,
    pub positionals: Vec<String>,
}

impl Opts {
    /// Collapse the parsed cc flags into engine [`GlobalFlags`]: compilation mode expands to
    /// compile flags, then copts/cxxopts/conlyopts and `-D`efines ride every compile; linkopts
    /// ride every link. `bin_tree_layout` is always true — outputs land under the output tree
    /// (`razel-out/`, or `bazel-out/` under compat), never in-tree — so both the CLI AND the
    /// daemon keep the source tree clean (this is what fixes the daemon's `GlobalFlags::default()`
    /// pollution).
    pub fn global_flags(&self) -> GlobalFlags {
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
            compilation_mode: self.compilation_mode.clone().unwrap_or_default(),
            defines: self
                .defines
                .iter()
                .filter_map(|d| d.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
                .collect(),
            jobs: self.jobs,
            bazel_build_compat: self.bazel_build_compat || bazel_build_compat_env(),
            bin_tree_layout: true,
            ..Default::default()
        }
    }
}

/// `RAZEL_BAZEL_BUILD_COMPAT` truthy? Accepts `1`/`t`/`true` (case-insensitive).
pub fn bazel_build_compat_env() -> bool {
    std::env::var("RAZEL_BAZEL_BUILD_COMPAT")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "t" | "true"))
        .unwrap_or(false)
}

/// A flag razel acts on: parses the (optional) value and updates [`Opts`].
type Handler = fn(&mut Opts, Option<String>);

/// razel's own flags — recognized in addition to (and ahead of) Bazel's.
static RAZEL_FLAGS: &[FlagSpec] = &[
    FlagSpec { name: "workspace", abbrev: Some('C'), takes_value: true, allow_multiple: false, silent: false },
    FlagSpec { name: "socket", abbrev: None, takes_value: true, allow_multiple: false, silent: false },
    FlagSpec { name: "daemon", abbrev: None, takes_value: false, allow_multiple: false, silent: false },
    FlagSpec { name: "cbor", abbrev: None, takes_value: false, allow_multiple: false, silent: false },
    FlagSpec { name: "bazel_build_compat", abbrev: None, takes_value: false, allow_multiple: false, silent: false },
    FlagSpec { name: "cache", abbrev: None, takes_value: true, allow_multiple: false, silent: false },
];

/// The flags razel actually honors → their effect. **This map is the definition of
/// "supported".** A recognized Bazel flag with no row + not `silent` self-diagnoses as
/// unsupported.
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
    ("compilation_mode", |o, v| o.compilation_mode = v),
    ("copt", |o, v| o.copts.extend(v)),
    ("cxxopt", |o, v| o.cxxopts.extend(v)),
    ("conlyopt", |o, v| o.conlyopts.extend(v)),
    ("linkopt", |o, v| o.linkopts.extend(v)),
    ("define", |o, v| o.defines.extend(v)),
    ("jobs", |o, v| {
        if let Some(n) = v.and_then(|v| v.parse::<usize>().ok()) {
            o.jobs = n;
        }
    }),
    ("expunge", |o, v| o.expunge = v.as_deref() != Some("false")),
    ("bazel_build_compat", |o, v| o.bazel_build_compat = v.as_deref() != Some("false")),
];

/// Look up a long flag name across razel's flags then Bazel's.
fn spec_long(name: &str) -> Option<&'static FlagSpec> {
    RAZEL_FLAGS.iter().chain(BAZEL_FLAGS).find(|f| f.name == name)
}

/// Look up a short (abbreviated) flag, razel's then Bazel's. (`-C` is razel's workspace; `-c`
/// is Bazel's compilation_mode — distinct by case.)
fn spec_short(c: char) -> Option<&'static FlagSpec> {
    RAZEL_FLAGS.iter().chain(BAZEL_FLAGS).find(|f| f.abbrev == Some(c))
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

/// Apply a recognized flag: a handler (supported) runs; otherwise it's silently ignored
/// (language flags razel will never need) or diagnosed (recognized, not yet implemented).
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

/// Parse a Bazel-syntax command line: `--flag`/`--flag=val`/`--flag val`, `--noflag`, short
/// `-x`/`-xval`/`-x val`, `--` (rest are targets), positionals. Driven entirely by the flag
/// tables — unknown (non-Bazel) flags are a usage error (`Err(message)`), like Bazel.
pub fn parse_opts(args: &[String]) -> Result<Opts, String> {
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
                None => return Err(format!("unrecognized option `--{name}` (not a Bazel flag)")),
            }
        } else {
            let c = arg[1..].chars().next().unwrap();
            let attached = arg[1 + c.len_utf8()..].to_string();
            match spec_short(c) {
                Some(s) => (s, false, (!attached.is_empty()).then_some(attached)),
                None => return Err(format!("unrecognized option `-{c}`")),
            }
        };

        if spec.takes_value {
            if value.is_none() && !negated {
                match args.get(i) {
                    Some(v) => {
                        value = Some(v.clone());
                        i += 1;
                    }
                    None => return Err(format!("`{}` requires a value", spec.name)),
                }
            }
        } else {
            value = Some(if negated { "false".into() } else { "true".into() });
        }

        dispatch(&mut o, spec, value);
    }
    // RG 0010: ABSOLUTIZE the workspace (the bare default `.` and any relative -C): actions
    // execute in sandbox dirs, where a relative exec_root breaks input staging on cold builds.
    // (Daemon note: the daemon resolves the client's cwd before calling this; see WS-D.)
    if o.workspace.is_relative() {
        o.workspace = std::fs::canonicalize(&o.workspace)
            .map_err(|e| format!("cannot resolve workspace {}: {e}", o.workspace.display()))?;
    }
    Ok(o)
}

/// The per-workspace daemon rendezvous socket path.
pub fn default_socket(workspace: &Path) -> PathBuf {
    workspace.join(".razel-daemon.sock")
}

/// rc-lite (S3d): the WORKSPACE layer only of `.bazelrc` then `.razelrc` — command-scoped lines
/// (`build --flag …`), comments/blanks skipped, bazel's command inheritance. `.razelrc` is the
/// razel-only DELTA, applied AFTER `.bazelrc`; CLI args follow all rc flags (the command line wins).
fn rc_lite_flags(workspace: &Path, commands: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for rc in [".bazelrc", ".razelrc"] {
        let Ok(src) = std::fs::read_to_string(workspace.join(rc)) else {
            continue;
        };
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

/// Parse args twice when rc files apply: once to find the workspace, then with the workspace's
/// rc-lite flags PREPENDED (rc first ⇒ explicit CLI flags override).
pub fn parse_opts_with_rc(commands: &[&str], args: &[String]) -> Result<Opts, String> {
    let pre = parse_opts(args)?;
    let rc = rc_lite_flags(&pre.workspace, commands);
    if rc.is_empty() {
        return Ok(pre);
    }
    let merged: Vec<String> = rc.into_iter().chain(args.iter().cloned()).collect();
    parse_opts(&merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_build_flags_and_positionals_into_global_flags() {
        let o = parse_opts(&[
            "-c".into(),
            "opt".into(),
            "--jobs=8".into(),
            "--copt".into(),
            "-Wall".into(),
            "//foo:bar".into(),
        ])
        .unwrap();
        assert_eq!(o.compilation_mode.as_deref(), Some("opt"));
        assert_eq!(o.jobs, 8);
        assert_eq!(o.positionals, vec!["//foo:bar".to_string()]);
        let gf = o.global_flags();
        assert!(gf.copts.contains(&"-O2".to_string()), "opt → -O2");
        assert!(gf.copts.contains(&"-Wall".to_string()), "--copt rides");
        assert_eq!(gf.jobs, 8);
        assert!(gf.bin_tree_layout, "always the output tree (no in-tree pollution)");
    }

    #[test]
    fn unrecognized_flag_is_a_usage_error() {
        assert!(parse_opts(&["--totally-not-a-bazel-flag".into()]).is_err());
    }

    #[test]
    fn double_dash_makes_the_rest_positional() {
        let o = parse_opts(&["--".into(), "--not-a-flag".into(), "//x".into()]).unwrap();
        assert_eq!(o.positionals, vec!["--not-a-flag".to_string(), "//x".to_string()]);
    }
}
