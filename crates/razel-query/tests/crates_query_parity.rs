//! P5.3b (§13, q4): the `@crates` query golden battery — `razel query` over razel's OWN `@crates`
//! closure vs `bazel query`. The committed `crate_blake3/query_goldens.txt` was captured from real
//! `bazel query --noimplicit_deps` (`cargo xtask capture-crates-query-goldens`); this driver runs the
//! SAME exprs through `razel query` at the repo root and asserts set-equality after normalizing
//! bazel's canonical repo display to razel's apparent form.
//!
//! IGNORED by default — like the `blake3_closure` analysis drivers it materializes razel's `@crates`
//! closure from the repo root (lock-seeded). The golden FILE is the committed q4 sentinel. Run:
//!   cargo test -p razel-query --test crates_query_parity -- --ignored --nocapture

use razel_query::{GlobalFlags, Output, run};
use std::path::Path;

/// Normalize a label's REPO so the two engines compare equal. razel emits CANONICAL
/// `@@rules_rust++crate+…` for materialized crates and APPARENT `@platforms`/`@rules_rust//rust/platform`
/// for the vendored slice; bazel emits canonical `@@…` for crates + `@@platforms` but apparent
/// `@rules_rust//rust/platform`. Map `@@rules_rust++crate+` → `@` (crate_universe canonical → apparent),
/// then any remaining `@@` → `@` (`@@platforms` → `@platforms`). Idempotent on already-apparent labels.
fn norm(label: &str) -> String {
    label.replace("@@rules_rust++crate+", "@").replace("@@", "@")
}

fn normset(lines: &str) -> Vec<String> {
    let mut v: Vec<String> = lines.lines().filter(|l| !l.is_empty()).map(norm).collect();
    v.sort();
    v.dedup();
    v
}

#[test]
#[ignore = "P5.3b dev gate: @crates query parity (materializes razel's @crates closure from the root)"]
fn razel_query_matches_the_crates_query_goldens() {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root");
    let goldens =
        std::fs::read_to_string(root.join("parity/corpus/rust/crate_blake3/query_goldens.txt"))
            .expect("crate_blake3/query_goldens.txt (run `cargo xtask capture-crates-query-goldens`)");

    let mut cases = 0;
    let mut failures = Vec::new();
    for block in goldens.split("@@ ").skip(1) {
        let (expr, body) = block.split_once('\n').expect("an expr line then its labels");
        let expr = expr.trim();
        let want = normset(body);

        let out = run(&root, GlobalFlags::default(), expr, Output::Label, false)
            .unwrap_or_else(|e| panic!("razel query `{expr}`: {e}"));
        let got = normset(&out);

        if got != want {
            let bazel_only: Vec<_> = want.iter().filter(|l| !got.contains(l)).collect();
            let razel_only: Vec<_> = got.iter().filter(|l| !want.contains(l)).collect();
            failures
                .push(format!("`{expr}`\n  bazel-only: {bazel_only:?}\n  razel-only: {razel_only:?}"));
        }
        cases += 1;
    }
    assert_eq!(cases, 4, "expected the full @crates battery (4 exprs; compile_data deferred — Phase 6)");
    assert!(failures.is_empty(), "@crates query parity mismatches:\n\n{}", failures.join("\n\n"));
}
