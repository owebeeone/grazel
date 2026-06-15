//! `razel-process-wrapper` CLI dispatch. Usage:
//!   razel-process-wrapper build-script --flags-out F --out-dir D [--env K=V]… [--env-file P]… \
//!       [--rundir R] -- PROG [ARGS…]
//!   razel-process-wrapper rustc --rustc=PATH [--flags-file=F] [--env-file=F]… [--env=K=V]… \
//!       -- <rustc args…>

use std::process::ExitCode;

use razel_process_wrapper::bs_runner::{run_build_script, RunOpts};
use razel_process_wrapper::rustc::{run_rustc, RustcOpts};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match dispatch(&args) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(e) => {
            eprintln!("razel-process-wrapper: {e}");
            ExitCode::from(2)
        }
    }
}

fn dispatch(args: &[String]) -> Result<i32, String> {
    let (sub, rest) = args
        .split_first()
        .ok_or("usage: razel-process-wrapper <build-script|rustc> …")?;
    match sub.as_str() {
        "build-script" => {
            let opts = RunOpts::from_args(rest)?;
            run_build_script(&opts).map_err(|e| e.to_string())
        }
        "rustc" => {
            let opts = RustcOpts::from_args(rest)?;
            run_rustc(&opts).map_err(|e| e.to_string())
        }
        other => Err(format!("unknown subcommand `{other}` (expected `build-script` or `rustc`)")),
    }
}
