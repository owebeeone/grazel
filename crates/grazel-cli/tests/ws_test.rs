//! The `grazel ws test` ladder, in-process under `cargo test` (GrazelWorkstream
//! §1: the SAME stages serve CI and the shipped verb). The binary under test is
//! the real one Cargo built for this crate.

use grazel_cli_lib::wstest::{StageCtx, run_stages};
use std::path::PathBuf;

#[test]
fn ws_test_ladder_green() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = StageCtx {
        grazel_bin: PathBuf::from(env!("CARGO_BIN_EXE_grazel")),
        tmp: tmp.path().to_path_buf(),
    };
    let mut out = Vec::new();
    let ok = run_stages(&ctx, None, &mut out);
    print!("{}", String::from_utf8_lossy(&out));
    assert!(ok, "ws-test ladder failed:\n{}", String::from_utf8_lossy(&out));
}

#[test]
fn unknown_stage_is_a_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = StageCtx {
        grazel_bin: PathBuf::from(env!("CARGO_BIN_EXE_grazel")),
        tmp: tmp.path().to_path_buf(),
    };
    assert!(!run_stages(&ctx, Some("no-such-stage"), &mut Vec::new()));
}
