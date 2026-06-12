//! The `grazel` bin — rust/OS mechanics ONLY (§1d thin-bin rule): argv intake,
//! exit code. Everything else is grazel-cli-lib.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    grazel_cli_lib::run(&args)
}
