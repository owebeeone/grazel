//! The `grazel` bin — rust/OS mechanics ONLY (§1d thin-bin rule): argv intake,
//! exit code. Everything else is grazel-cli-lib.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(grazel_cli_lib::run(&args));
}
