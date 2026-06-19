//! `razel-process-wrapper` — a thin standalone entry over [`razel_process_wrapper::dispatch`] (the
//! exec-time build-script / rustc wrapper). Kept as a dev/test fixture; the shipped + live path is
//! the `razel process-wrapper` subcommand (same `dispatch`), so razel self-invokes the wrapper via
//! `current_exe()` with no separate binary to co-locate. Usage:
//!   razel-process-wrapper build-script --flags-out F --out-dir D [--env K=V]… [--env-file P]… \
//!       [--rundir R] -- PROG [ARGS…]
//!   razel-process-wrapper rustc --rustc=PATH [--flags-file=F] [--env-file=F]… [--env=K=V]… \
//!       -- <rustc args…>

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match razel_process_wrapper::dispatch(&args) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(e) => {
            eprintln!("razel-process-wrapper: {e}");
            ExitCode::from(2)
        }
    }
}
