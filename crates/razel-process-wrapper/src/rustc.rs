//! crate-universe P3.9 (§6.1/§4.1): the `rustc` subcommand — the build-script flags-file READER.
//! Reads a §6.1 flags file, maps each `kind` to its rustc flag(s) (the normative table below),
//! injects `rustc-env` records + `--env-file`/`--env` as the rustc process env, and runs the real
//! rustc with the original args + the appended flags. An empty/absent flags file is a no-op
//! passthrough. The CARGO env CONTENT is policy razel-loading passes (P3.10 wires the edge); this
//! subcommand is the Cargo-agnostic mechanism.
//!
//! Explicit-argv shape (the executor emits this, §4.1):
//!   `razel-process-wrapper rustc --rustc=<path> [--flags-file=<f>] [--env-file=<f>]… [--env=K=V]…
//!    -- <rustc args…>`

use std::io;
use std::path::PathBuf;
use std::process::Command;

use crate::flags::{self, FlagsRecord};

/// Parsed `rustc` subcommand options. Flags are `=`-joined (`--flags-file=PATH`); everything after
/// `--` is the real rustc argv.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RustcOpts {
    pub rustc: String,
    pub flags_file: Option<PathBuf>,
    /// P4.5 (§5.5/§6): TRANSITIVE build-script flags-files contributing native-LINK directives only
    /// (a `rust_binary`'s final link inherits the `-l`/`-L`/`-Clink-arg` of every build script in its
    /// closure). Distinct from `flags_file` (the OWN intra-target edge, which also applies cfg/env).
    pub link_flags_files: Vec<PathBuf>,
    pub env_files: Vec<PathBuf>,
    pub env: Vec<(String, String)>,
    pub rustc_args: Vec<String>,
}

impl RustcOpts {
    pub fn from_args(args: &[String]) -> Result<RustcOpts, String> {
        let mut o = RustcOpts::default();
        let mut i = 0;
        while i < args.len() {
            if args[i] == "--" {
                o.rustc_args = args[(i + 1)..].to_vec();
                if o.rustc.is_empty() {
                    return Err("rustc: --rustc=<path> is required".to_string());
                }
                return Ok(o);
            }
            let (flag, val) = args[i]
                .split_once('=')
                .ok_or_else(|| format!("rustc: expected --flag=value, got `{}`", args[i]))?;
            match flag {
                "--rustc" => o.rustc = val.to_string(),
                "--flags-file" => o.flags_file = Some(PathBuf::from(val)),
                "--link-flags-file" => o.link_flags_files.push(PathBuf::from(val)),
                "--env-file" => o.env_files.push(PathBuf::from(val)),
                "--env" => {
                    let (k, v) = val
                        .split_once('=')
                        .ok_or_else(|| format!("rustc: --env expects K=V, got `{val}`"))?;
                    o.env.push((k.to_string(), v.to_string()));
                }
                other => return Err(format!("rustc: unknown flag `{other}`")),
            }
            i += 1;
        }
        Err("rustc: missing `--` separator before the rustc args".to_string())
    }
}

/// The normative §6.1 `kind`→rustc mapping: returns the extra rustc ARGS to append + the `rustc-env`
/// records to inject as process env. Pure. `args` are already tokenized by the parser (P3.7), so the
/// reader never re-tokenizes or shell-quotes.
pub fn apply_flags(flags: &[FlagsRecord]) -> (Vec<String>, Vec<(String, String)>) {
    let mut args = Vec::new();
    let mut env = Vec::new();
    for f in flags {
        match f.kind.as_str() {
            "rustc-cfg" => {
                for a in &f.args {
                    args.push("--cfg".into());
                    args.push(a.clone());
                }
            }
            "rustc-link-lib" => {
                for a in &f.args {
                    args.push("-l".into());
                    args.push(a.clone());
                }
            }
            "rustc-link-search" => {
                for a in &f.args {
                    args.push("-L".into());
                    args.push(a.clone());
                }
            }
            // `rustc-cdylib-link-arg` is cdylib-only in cargo; slice-1 maps it like `rustc-link-arg`
            // (`-C link-arg`) — refined if a parity golden distinguishes the crate type.
            "rustc-link-arg" | "rustc-cdylib-link-arg" => {
                for a in &f.args {
                    args.push("-C".into());
                    args.push(format!("link-arg={a}"));
                }
            }
            "rustc-flags" => args.extend(f.args.iter().cloned()), // already tokenized (§6.1)
            "rustc-env" => {
                for a in &f.args {
                    if let Some((k, v)) = a.split_once('=') {
                        env.push((k.to_string(), v.to_string()));
                    }
                }
            }
            _ => {} // the parser only emits the kinds above; ignore any other defensively
        }
    }
    (args, env)
}

/// P4.5 (§5.5/§6): the native-LINK subset of [`apply_flags`] — `rustc-link-lib`/`-search`/`-arg`
/// only. A TRANSITIVE build-script flags-file (a dep's, via `--link-flags-file`) contributes its
/// link directives to a consuming FINAL link, but its `rustc-cfg`/`rustc-env` are intra-target (they
/// configured the PRODUCING crate's own compile) and MUST NOT leak into the consumer. (The OWN
/// build-script edge still rides `--flags-file`, which applies cfg+env+link to the same crate.)
pub fn link_args_only(flags: &[FlagsRecord]) -> Vec<String> {
    let mut args = Vec::new();
    for f in flags {
        match f.kind.as_str() {
            "rustc-link-lib" => {
                for a in &f.args {
                    args.push("-l".into());
                    args.push(a.clone());
                }
            }
            "rustc-link-search" => {
                for a in &f.args {
                    args.push("-L".into());
                    args.push(a.clone());
                }
            }
            "rustc-link-arg" | "rustc-cdylib-link-arg" => {
                for a in &f.args {
                    args.push("-C".into());
                    args.push(format!("link-arg={a}"));
                }
            }
            _ => {} // cfg/env/flags are intra-target — never propagated to a consumer's link
        }
    }
    args
}

/// Run the real rustc through the wrapper: read+apply the flags file (if any), assemble the env
/// (baseline < `--env-file` < `rustc-env` from flags < explicit `--env`), append the mapped flags
/// to the original rustc argv, and exec. Empty/absent flags file → a pure passthrough.
pub fn run_rustc(opts: &RustcOpts) -> io::Result<i32> {
    let records = match &opts.flags_file {
        Some(p) if p.exists() => flags::read_flags_jsonl(&std::fs::read_to_string(p)?)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
        _ => Vec::new(),
    };
    let (mut extra_args, rustc_env) = apply_flags(&records);
    // P4.5 (§5.5/§6): TRANSITIVE build-script flags-files — link directives only (a consuming final
    // link inherits the closure's `-l`/`-L`/`-Clink-arg`; their cfg/env stay intra-target).
    for p in &opts.link_flags_files {
        if p.exists() {
            let recs = flags::read_flags_jsonl(&std::fs::read_to_string(p)?)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            extra_args.extend(link_args_only(&recs));
        }
    }
    // baseline < env-files < build-script `rustc-env` < explicit `--env` (literal `rustc_env` is the
    // highest authority, §6.2). `rustc-env` rides as the next-to-last layer via `base_env`.
    let mut env = crate::env::base_env(&crate::env::platform_baseline(), &opts.env_files, &rustc_env)?;
    for (k, v) in &opts.env {
        env.insert(k.clone(), v.clone());
    }
    // Cargo/Bazel set OUT_DIR + CARGO_MANIFEST_DIR ABSOLUTE; a crate may use them where relative
    // fails: `include!(concat!(env!("OUT_DIR"), …))` resolves against the INCLUDING SOURCE file's dir
    // (not CWD), and a proc-macro reads CARGO_MANIFEST_DIR to find a file (schemafy's schema). razel
    // passes them exec-root-relative, so absolutize against the runtime CWD (the exec-root). Runtime
    // env mechanism, not the action graph (argv/inputs) → parity-neutral.
    if let Ok(cwd) = std::env::current_dir() {
        for key in ["OUT_DIR", "CARGO_MANIFEST_DIR"] {
            if let Some(v) = env.get(key)
                && std::path::Path::new(v).is_relative()
            {
                env.insert(key.into(), cwd.join(v).to_string_lossy().into_owned());
            }
        }
    }
    // B4: razel compiles with the SYSTEM toolchain — it does NOT emit Bazel's `-Clinker=<abs cc>`
    // (a documented parity deviation), so when rustc LINKS (a `bin`, e.g. a build-script host bin) it
    // must resolve the linker `cc`/`ld` via PATH. The default-deny env carries no PATH on unix, so
    // pass the wrapper's own (the executor set it) unless `--env`/`--env-file` already pinned one.
    // Parity-neutral: env is runtime mechanism, not part of the action-graph (argv/inputs) compare.
    if !env.contains_key("PATH") {
        if let Ok(path) = std::env::var("PATH") {
            env.insert("PATH".into(), path);
        }
    }
    let status = Command::new(&opts.rustc)
        .args(&opts.rustc_args)
        .args(&extra_args)
        .env_clear()
        .envs(&env)
        .status()?;
    Ok(status.code().unwrap_or(-1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(kind: &str, args: &[&str]) -> FlagsRecord {
        FlagsRecord { kind: kind.into(), args: args.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn p39_from_args_parses_equals_joined_flags_and_rustc_argv() {
        let args: Vec<String> = [
            "--rustc=/t/rustc",
            "--flags-file=/o/_bs.out",
            "--env-file=/o/cargo_pkg.env",
            "--env=OUT_DIR=/o/_bs.out_dir",
            "--",
            "--edition",
            "2021",
            "--crate-name",
            "blake3",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let o = RustcOpts::from_args(&args).unwrap();
        assert_eq!(o.rustc, "/t/rustc");
        assert_eq!(o.flags_file, Some(PathBuf::from("/o/_bs.out")));
        assert_eq!(o.env_files, [PathBuf::from("/o/cargo_pkg.env")]);
        assert_eq!(o.env, [("OUT_DIR".to_string(), "/o/_bs.out_dir".to_string())]);
        assert_eq!(o.rustc_args, ["--edition", "2021", "--crate-name", "blake3"]);
    }

    #[test]
    fn p39_from_args_requires_rustc_and_separator() {
        assert!(RustcOpts::from_args(&["--flags-file=/x".to_string()]).unwrap_err().contains("`--` separator"));
        let no_rustc: Vec<String> = ["--flags-file=/x", "--"].iter().map(|s| s.to_string()).collect();
        assert!(RustcOpts::from_args(&no_rustc).unwrap_err().contains("--rustc"));
    }

    #[test]
    fn p39_apply_flags_maps_each_kind_per_the_normative_table() {
        let flags = vec![
            rec("rustc-cfg", &["feature=\"simd\""]),
            rec("rustc-link-lib", &["static=blake3"]),
            rec("rustc-link-search", &["native=/opt/lib"]),
            rec("rustc-link-arg", &["-Wl,-z,now"]),
            rec("rustc-cdylib-link-arg", &["-undefined"]),
            rec("rustc-flags", &["-L", "/extra", "--cfg", "tokio_unstable"]),
            rec("rustc-env", &["BUILD_ID=abc123"]),
        ];
        let (args, env) = apply_flags(&flags);
        assert_eq!(
            args,
            [
                "--cfg", "feature=\"simd\"",
                "-l", "static=blake3",
                "-L", "native=/opt/lib",
                "-C", "link-arg=-Wl,-z,now",
                "-C", "link-arg=-undefined",
                "-L", "/extra", "--cfg", "tokio_unstable", // rustc-flags appended verbatim
            ],
            "kind→rustc mapping: {args:?}"
        );
        // rustc-env → process env injection, NOT argv.
        assert_eq!(env, [("BUILD_ID".to_string(), "abc123".to_string())]);
        assert!(!args.iter().any(|a| a.contains("BUILD_ID")), "rustc-env is not an argv token");
    }

    #[test]
    fn p39_empty_flags_is_a_no_op_passthrough() {
        let (args, env) = apply_flags(&[]);
        assert!(args.is_empty() && env.is_empty(), "no flags → nothing appended");
    }

    #[test]
    fn p45_from_args_collects_repeated_link_flags_files() {
        // P4.5: `--link-flags-file` is repeatable (a final link inherits the WHOLE closure's build
        // scripts) and distinct from the single own `--flags-file`.
        let args: Vec<String> = [
            "--rustc=/t/rustc",
            "--flags-file=/o/_bs.out",
            "--link-flags-file=/o/dep1/_bs.out",
            "--link-flags-file=/o/dep2/_bs.out",
            "--",
            "--crate-name",
            "app",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let o = RustcOpts::from_args(&args).unwrap();
        assert_eq!(o.flags_file, Some(PathBuf::from("/o/_bs.out")), "own edge is the single --flags-file");
        assert_eq!(
            o.link_flags_files,
            [PathBuf::from("/o/dep1/_bs.out"), PathBuf::from("/o/dep2/_bs.out")],
            "transitive link-flags-files collected in order"
        );
    }

    #[test]
    fn p45_link_args_only_keeps_link_directives_and_drops_cfg_env() {
        // P4.5 (§5.5/§6): a TRANSITIVE build-script flags-file contributes ONLY native-link
        // directives to a consuming final link; its `rustc-cfg`/`rustc-env`/`rustc-flags` are
        // intra-target (they configured the PRODUCING crate) and must NOT leak into the consumer.
        let flags = vec![
            rec("rustc-cfg", &["have_libz"]),         // intra-target — dropped
            rec("rustc-env", &["DEP_Z_INCLUDE=/x"]),  // intra-target — dropped
            rec("rustc-flags", &["--cfg", "leak"]),   // intra-target — dropped
            rec("rustc-link-lib", &["static=z"]),
            rec("rustc-link-search", &["native=/opt/lib"]),
            rec("rustc-link-arg", &["-Wl,-rpath,/x"]),
            rec("rustc-cdylib-link-arg", &["-undefined"]),
        ];
        let args = link_args_only(&flags);
        assert_eq!(
            args,
            [
                "-l", "static=z",
                "-L", "native=/opt/lib",
                "-C", "link-arg=-Wl,-rpath,/x",
                "-C", "link-arg=-undefined",
            ],
            "link-only: {args:?}"
        );
        assert!(!args.iter().any(|a| a.contains("have_libz") || a.contains("DEP_Z") || a == "leak"),
            "cfg/env/flags must not leak: {args:?}");
    }
}
