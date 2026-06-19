//! `razel-process-wrapper` — the exec-time process wrapper (rules_rust `process_wrapper`
//! analogue). The executor invokes it (by path, like `rustc`) to wrap subprocess actions whose
//! contract the bare command can't express.
//!
//! Subcommands:
//! - `build-script` (P3.8 §5.2): run a cargo build script with a default-deny env, capture its
//!   stdout, parse the §6.1 directives, write the JSONL flags file + OUT_DIR. The WRITER.
//! - `rustc` (P3.9, §6.1): apply a flags file to a rustc invocation (`--cfg`/`-l`/`-L`/`-C
//!   link-arg`/env). The READER.
//!
//! The shared [`flags`] module is the single definition of the flags-file schema both ends use;
//! the shared [`env`] module is the default-deny env assembly both subcommands layer onto.

pub mod bs_runner;
pub mod env;
pub mod flags;
pub mod rustc;

/// Dispatch a process-wrapper invocation (`<build-script|rustc> …`) to the matching subcommand.
/// Shared by the standalone `razel-process-wrapper` bin (a dev/test fixture) AND the `razel
/// process-wrapper` subcommand — the wrapper is folded INTO the razel binary so razel self-invokes
/// it via `current_exe()`, with no separate co-located tool to find on PATH / in runfiles.
pub fn dispatch(args: &[String]) -> Result<i32, String> {
    let (sub, rest) = args.split_first().ok_or("usage: <build-script|rustc> …")?;
    match sub.as_str() {
        "build-script" => {
            let opts = bs_runner::RunOpts::from_args(rest)?;
            bs_runner::run_build_script(&opts).map_err(|e| e.to_string())
        }
        "rustc" => {
            let opts = rustc::RustcOpts::from_args(rest)?;
            rustc::run_rustc(&opts).map_err(|e| e.to_string())
        }
        other => Err(format!("unknown subcommand `{other}` (expected `build-script` or `rustc`)")),
    }
}
