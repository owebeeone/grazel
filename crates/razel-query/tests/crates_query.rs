//! P5.3 (§13, q4) dev driver: `razel query` over razel's OWN `@crates` closure from the repo root.
//!
//! IGNORED by default — like the `blake3_closure` analysis drivers, it queries the real `@crates`
//! graph (seeds the root `MODULE.bazel.lock`, materializes `.razel-crates` there), so it is a fast
//! DRIVER for the q4 query roll, not a committed gate. The committed q4 gate is the `crates_query_parity`
//! golden (P5.3b: captured once from `bazel query`, checked in, verified hermetically). Run:
//!   cargo test -p razel-query --test crates_query -- --ignored --nocapture

use razel_query::{GlobalFlags, Output, run};
use std::path::Path;

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root")
}

/// `deps(@crates//:blake3)` ENTERS `@crates` (external query entry, P5.3a), MATERIALIZES the closure
/// from the lock, and TRAVERSES into both the materialized `@crates__*` crates and the vendored
/// `@platforms`/`@rules_rust//rust/platform` slice (P5.0/P5.2). Proves the whole q4 entry path runs.
#[test]
#[ignore = "P5.3 dev driver: queries razel's own @crates closure from the repo root"]
fn probe_blake3_deps_query_enters_and_traverses_crates() {
    let root = repo_root();
    let out = run(&root, GlobalFlags::default(), "deps(@crates//:blake3)", Output::LabelKind, false)
        .unwrap_or_else(|e| panic!("query deps(@crates//:blake3): {e}"));

    let lines: Vec<&str> = out.lines().collect();
    eprintln!("deps(@crates//:blake3) → {} nodes", lines.len());
    for l in &lines {
        eprintln!("  {l}");
    }

    // razel emits CANONICAL `@@rules_rust++crate+crates…` labels (the lock-keyed identity); the q4
    // golden comparator (P5.3b) normalizes them back to apparent `@crates…`. The driver asserts on
    // what razel actually produces.
    // Traversal reached the vendored platform slice (the `target_compatible_with` select default arm).
    assert!(
        out.contains("@platforms//:incompatible"),
        "deps() must reach the vendored @platforms//:incompatible node:\n{out}"
    );
    // Materialization + traversal reached blake3's own lib AND a real dep crate.
    assert!(
        out.contains("crates__blake3-1.8.2//:blake3"),
        "deps() must reach the materialized blake3 lib node:\n{out}"
    );
    assert!(
        lines.iter().filter(|l| l.contains("++crate+crates__")).count() >= 5,
        "deps() must reach several materialized dep crates:\n{out}"
    );
    // The P5.1 cargo_build_script macro children are traversed (deps() enters them).
    assert!(
        out.contains("crates__blake3-1.8.2//:_bs") && out.contains("crates__blake3-1.8.2//:build_script_build"),
        "deps() must traverse the build-script children (:_bs + :build_script_build alias):\n{out}"
    );
    // A real closure, not a single dangling node.
    assert!(lines.len() > 10, "expected a substantial closure, got {}:\n{out}", lines.len());
}
