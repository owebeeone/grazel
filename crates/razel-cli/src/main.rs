//! The `razel` bin — THIN by rule (PublicSurfaces §1d): OS mechanics only; every verb,
//! flag, and rendering decision lives in the razel-cli LIBRARY, which grazel links to
//! get this exact CLI surface (S0: the seam).

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    razel_cli::run(&args)
}
