//! B2 dev-driver (RazelRustParityPlan): does razel analyze the REAL `@crates//:blake3` closure?
//!
//! IGNORED by default — it analyzes razel's OWN dogfood `@crates` lock from the repo root (and
//! materializes `.razel-crates` there), so it is a fast DRIVER for the B2 gap-closing roll, not a
//! committed gate (B3's `crate_blake3` parity golden is the gate). Faster feedback than the CLI
//! rebuild: only razel-loading recompiles. Run:
//!   cargo test -p razel-loading --test blake3_closure -- --ignored --nocapture

use razel_loading::{GlobalFlags, analyze_workspace_with};
use std::path::Path;

#[test]
#[ignore = "B2 dev driver: analyzes razel's own @crates closure from the repo root"]
fn probe_blake3_closure_analyzes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root");
    match analyze_workspace_with(&root, "@crates//:blake3", GlobalFlags::default()) {
        Ok(targets) => {
            eprintln!("OK: @crates//:blake3 closure analyzed — {} targets", targets.len());
            for t in targets.iter().take(60) {
                eprintln!("  {}", t.name);
            }
        }
        Err(e) => panic!("@crates//:blake3 closure did NOT analyze:\n{e}"),
    }
}
