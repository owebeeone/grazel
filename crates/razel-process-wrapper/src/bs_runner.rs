//! crate-universe P3.8 (§5.2): the `build-script` subcommand — run a compiled cargo build script
//! with a default-deny env, capture its stdout, parse the §6.1 directives, and write the JSONL
//! flags file + create OUT_DIR. The Cargo env CONTENT (`CARGO_PKG_*`, `CARGO_FEATURE_*`, …) is
//! POLICY razel-loading passes as `--env`/`--env-file` (P3.8b); this wrapper is the Cargo-agnostic
//! MECHANISM (assemble env, run, capture, parse, write).
//!
//! Cross-platform: writes via `std::fs` (no `/bin/sh`), captures via `Command::output()`, and on
//! Windows seeds a small system-env baseline so a default-deny child can still start.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::flags;

/// Parsed `build-script` subcommand options.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RunOpts {
    pub flags_out: PathBuf,
    pub out_dir: PathBuf,
    pub rundir: Option<PathBuf>,
    /// Explicit Cargo env (`--env KEY=VALUE`) — highest precedence (literal `rustc_env`/`version`/
    /// `pkg_name` override env-file entries, §6.2).
    pub env: Vec<(String, String)>,
    /// `--env-file` paths (KEY=VALUE lines); later files win (§6.2).
    pub env_files: Vec<PathBuf>,
    pub program: String,
    pub args: Vec<String>,
}

impl RunOpts {
    /// Parse `--flags-out F --out-dir D [--env K=V]… [--env-file P]… [--rundir R] -- PROG [ARGS…]`.
    pub fn from_args(args: &[String]) -> Result<RunOpts, String> {
        let mut o = RunOpts::default();
        let val = |idx: usize| {
            args.get(idx + 1).cloned().ok_or_else(|| format!("{} needs a value", args[idx]))
        };
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--" => {
                    o.program = args
                        .get(i + 1)
                        .cloned()
                        .ok_or_else(|| "missing program after `--`".to_string())?;
                    o.args = args[(i + 2).min(args.len())..].to_vec();
                    return Ok(o);
                }
                "--flags-out" => {
                    o.flags_out = PathBuf::from(val(i)?);
                    i += 2;
                }
                "--out-dir" => {
                    o.out_dir = PathBuf::from(val(i)?);
                    i += 2;
                }
                "--rundir" => {
                    o.rundir = Some(PathBuf::from(val(i)?));
                    i += 2;
                }
                "--env" => {
                    let kv = val(i)?;
                    let (k, v) = kv
                        .split_once('=')
                        .ok_or_else(|| format!("--env expects KEY=VALUE, got `{kv}`"))?;
                    o.env.push((k.to_string(), v.to_string()));
                    i += 2;
                }
                "--env-file" => {
                    o.env_files.push(PathBuf::from(val(i)?));
                    i += 2;
                }
                other => return Err(format!("unknown build-script flag `{other}`")),
            }
        }
        Err("missing `--` separator before the build-script program".to_string())
    }
}

/// Assemble the build script's child env (§5.2 default-deny), lowest→highest precedence: the
/// platform `baseline`, then each `env_files` entry (later FILES win, §6.2), then the explicit
/// `env` entries (override env-file), then `OUT_DIR` (wrapper-authoritative, last).
pub fn assemble_child_env(
    out_dir: &Path,
    env_files: &[PathBuf],
    env: &[(String, String)],
    baseline: &BTreeMap<String, String>,
) -> io::Result<BTreeMap<String, String>> {
    let mut out = crate::env::base_env(baseline, env_files, env)?;
    out.insert("OUT_DIR".to_string(), out_dir.to_string_lossy().into_owned());
    Ok(out)
}

/// The result of turning a build script's captured stdout into flags-file + side channels.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Processed {
    pub flags_jsonl: String,
    pub warnings: Vec<String>,
    pub deviations: Vec<String>,
    pub error: Option<String>,
}

/// Parse captured stdout (§6.1) → the JSONL flags-file body + side channels. Pure.
pub fn process_script_output(stdout: &[u8]) -> Processed {
    let parsed = flags::parse_build_script_output(&String::from_utf8_lossy(stdout));
    Processed {
        flags_jsonl: flags::flags_file_jsonl(&parsed.flags),
        warnings: parsed.warnings,
        deviations: parsed.deviations,
        error: parsed.error,
    }
}

/// Run the build script (§5.2 action 2): create OUT_DIR, assemble the env, run the bin capturing
/// stdout, parse it, surface warnings/deviations to stderr, fail on `error=` or a non-zero script
/// exit, else write the flags file. Returns the exit code for the wrapper to propagate.
pub fn run_build_script(opts: &RunOpts) -> io::Result<i32> {
    // B4: cargo runs a build script with cwd = the crate manifest dir (so `cc::Build`'s relative
    // `build.file("c/x.c")` resolves) and ABSOLUTE OUT_DIR / program (cwd-independent). The wrapper's
    // own cwd is the exec sandbox; absolutize OUT_DIR + the bin against it BEFORE chdir-ing the child
    // to `--rundir`. The flags-file + `--env-file`s stay relative — the WRAPPER reads/writes those.
    let base = std::env::current_dir()?;
    let absify = |p: &Path| if p.is_absolute() { p.to_path_buf() } else { base.join(p) };
    let abs_out_dir = absify(&opts.out_dir);
    std::fs::create_dir_all(&abs_out_dir)?;
    let mut env =
        assemble_child_env(&abs_out_dir, &opts.env_files, &opts.env, &crate::env::platform_baseline())?;
    let mut cmd = Command::new(absify(Path::new(&opts.program)));
    cmd.args(&opts.args).env_clear();
    if let Some(dir) = &opts.rundir {
        let abs_rundir = absify(dir);
        // cargo sets CARGO_MANIFEST_DIR = the crate dir; cc-rs + `env!()` read it.
        env.insert("CARGO_MANIFEST_DIR".into(), abs_rundir.to_string_lossy().into_owned());
        cmd.current_dir(&abs_rundir);
    }
    cmd.envs(&env);
    let output = cmd.output()?; // captures stdout (and stderr)
    // B4: the script ran with an ABSOLUTE OUT_DIR (the per-action sandbox), so cc-rs etc. emit
    // `rustc-link-search`/`-L` paths under that EPHEMERAL sandbox dir — invalid for the downstream
    // crate compile (a different sandbox). Rewrite that absolute prefix back to the exec-root-relative
    // `--out-dir`, where the OUT_DIR tree is staged as a declared input of the consuming compile, so
    // the recorded flags resolve there. (Bazel's build-script link paths are likewise exec-relative.)
    let abs = abs_out_dir.to_string_lossy();
    let rel = opts.out_dir.to_string_lossy();
    let stdout = String::from_utf8_lossy(&output.stdout).replace(abs.as_ref(), rel.as_ref());
    let processed = process_script_output(stdout.as_bytes());
    for w in &processed.warnings {
        eprintln!("warning: {w}");
    }
    for d in &processed.deviations {
        eprintln!("{d}");
    }
    if !output.status.success() {
        // the script itself failed — surface its stderr + propagate its exit code.
        io::stderr().write_all(&output.stderr).ok();
        return Ok(output.status.code().unwrap_or(1));
    }
    if let Some(err) = &processed.error {
        eprintln!("error: build script reported: {err}");
        return Ok(1);
    }
    std::fs::write(&opts.flags_out, processed.flags_jsonl.as_bytes())?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("razel-pw-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn p38a_from_args_parses_flags_and_program() {
        let args: Vec<String> = [
            "--flags-out", "/o/flags.jsonl",
            "--out-dir", "/o/out",
            "--env", "CARGO_PKG_NAME=blake3",
            "--env-file", "/o/cargo_pkg.env",
            "--rundir", "/src/blake3",
            "--", "/o/_bs_", "--quiet",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let o = RunOpts::from_args(&args).unwrap();
        assert_eq!(o.flags_out, PathBuf::from("/o/flags.jsonl"));
        assert_eq!(o.out_dir, PathBuf::from("/o/out"));
        assert_eq!(o.rundir, Some(PathBuf::from("/src/blake3")));
        assert_eq!(o.env, [("CARGO_PKG_NAME".to_string(), "blake3".to_string())]);
        assert_eq!(o.env_files, [PathBuf::from("/o/cargo_pkg.env")]);
        assert_eq!(o.program, "/o/_bs_");
        assert_eq!(o.args, ["--quiet"]);
    }

    #[test]
    fn p38a_from_args_requires_the_separator() {
        let args: Vec<String> =
            ["--out-dir", "/o/out"].iter().map(|s| s.to_string()).collect();
        assert!(RunOpts::from_args(&args).unwrap_err().contains("`--` separator"));
    }

    #[test]
    fn p38a_env_precedence_baseline_then_env_file_then_explicit_then_out_dir() {
        let dir = tmp("env");
        // Two env-files: the LATER one wins among files (§6.2).
        let ef1 = dir.join("a.env");
        let ef2 = dir.join("b.env");
        std::fs::write(&ef1, "CARGO_PKG_NAME=old\nCARGO_PKG_VERSION=1.0.0\n# comment\n").unwrap();
        std::fs::write(&ef2, "CARGO_PKG_NAME=blake3\n").unwrap();
        let baseline: BTreeMap<String, String> =
            [("SystemRoot".to_string(), "C:\\Windows".to_string())].into_iter().collect();
        let explicit = [("CARGO_PKG_VERSION".to_string(), "1.8.2".to_string())]; // literal overrides file
        let env = assemble_child_env(
            &dir.join("OUT"),
            &[ef1, ef2],
            &explicit,
            &baseline,
        )
        .unwrap();
        assert_eq!(env.get("SystemRoot").map(String::as_str), Some("C:\\Windows"), "baseline kept");
        assert_eq!(env.get("CARGO_PKG_NAME").map(String::as_str), Some("blake3"), "later env-file wins");
        assert_eq!(env.get("CARGO_PKG_VERSION").map(String::as_str), Some("1.8.2"), "explicit --env overrides file");
        assert_eq!(
            env.get("OUT_DIR").map(String::as_str),
            Some(dir.join("OUT").to_string_lossy().as_ref()),
            "OUT_DIR is wrapper-authoritative"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn p38a_process_output_writes_flags_surfaces_warning_and_error() {
        let stdout = b"cargo::rustc-cfg=feature=\"simd\"\n\
cargo::warning=heads up\n\
cargo::rustc-link-lib=static=blake3\n" as &[u8];
        let p = process_script_output(stdout);
        // recognized directives → JSONL flags file (≥1 line per kind), in order.
        assert_eq!(p.flags_jsonl.lines().count(), 2, "{}", p.flags_jsonl);
        assert!(p.flags_jsonl.lines().next().unwrap().contains(r#""kind":"rustc-cfg""#));
        assert_eq!(p.warnings, ["heads up"], "warning routed to the side channel");
        assert!(p.error.is_none(), "no error here");

        // an `error=` directive populates the error channel (run_build_script fails on it).
        let p2 = process_script_output(b"cargo::error=no system lib\n");
        assert_eq!(p2.error.as_deref(), Some("no system lib"));
        assert!(p2.flags_jsonl.is_empty(), "error run produces no flags");
    }
}
