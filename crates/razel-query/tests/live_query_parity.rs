//! Full Phase-1 `qg`: LIVE `bazel query` parity. The committed `query_goldens.txt` was captured
//! from real `bazel query --noimplicit_deps <expr>` (`cargo xtask capture-query-goldens`); this test
//! runs the SAME expressions through `razel query` over the dual-queryable `corpus/rust/transitive`
//! package and asserts byte-identical results. Hermetic at test time (no bazel/network — it reads
//! the goldens + runs razel's load-only query engine). `--noimplicit_deps` is razel query's v1
//! default (implicit-deps parity is Phase 6); the goldens were captured the same way.

use razel_query::{GlobalFlags, Output, run};
use std::path::Path;

#[test]
fn razel_query_matches_the_bazel_query_goldens() {
    let parity = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../parity")
        .canonicalize()
        .expect("parity workspace");
    let goldens = std::fs::read_to_string(parity.join("corpus/rust/transitive/query_goldens.txt"))
        .expect("query_goldens.txt (run `cargo xtask capture-query-goldens`)");

    let mut cases = 0;
    for block in goldens.split("@@ ").skip(1) {
        let (expr, body) = block.split_once('\n').expect("an expr line then its labels");
        let expr = expr.trim();
        let mut want: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
        want.sort();

        // implicit=false → `--noimplicit_deps`, matching how the goldens were captured.
        let out = run(&parity, GlobalFlags::default(), expr, Output::Label, false)
            .unwrap_or_else(|e| panic!("razel query `{expr}`: {e}"));
        let mut got: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
        got.sort();

        assert_eq!(got, want, "razel query `{expr}` diverges from the bazel-query golden");
        cases += 1;
    }
    assert!(cases >= 7, "expected the full battery, captured {cases}");
}

/// P6.Q4: the `--output=package` renderer against LIVE `bazel query --output=package`. Same harness
/// as the label parity, over the committed `query_goldens_package.txt`: the main-repo package form
/// (no leading `//`, no repo) plus the cross-package union's deduped, sorted multi-package render.
#[test]
fn razel_query_package_output_matches_the_bazel_goldens() {
    let parity = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../parity")
        .canonicalize()
        .expect("parity workspace");
    let goldens =
        std::fs::read_to_string(parity.join("corpus/rust/transitive/query_goldens_package.txt"))
            .expect("query_goldens_package.txt (run `cargo xtask capture-query-goldens`)");

    let mut cases = 0;
    for block in goldens.split("@@ ").skip(1) {
        let (expr, body) = block.split_once('\n').expect("an expr line then its packages");
        let expr = expr.trim();
        let mut want: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
        want.sort();

        let out = run(&parity, GlobalFlags::default(), expr, Output::Package, false)
            .unwrap_or_else(|e| panic!("razel query --output=package `{expr}`: {e}"));
        let mut got: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
        got.sort();

        assert_eq!(
            got, want,
            "razel query --output=package `{expr}` diverges from the bazel-query golden"
        );
        cases += 1;
    }
    assert!(cases >= 3, "expected the full package battery, captured {cases}");
}

/// P6.Q5: the `tests()` verb against LIVE `bazel query` over the test-bearing `corpus/rust/tests`
/// package — a `test_suite` expanded to its members, a mixed `:all` (tests kept / suite expanded /
/// library dropped), and a non-test target → empty. Gates the verb on a REAL loaded graph (the
/// `test_suite` loader capture, P6.Q5), not just the synthetic eval unit test.
#[test]
fn razel_query_tests_verb_matches_the_bazel_goldens() {
    let parity = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../parity")
        .canonicalize()
        .expect("parity workspace");
    let goldens = std::fs::read_to_string(parity.join("corpus/rust/tests/query_goldens.txt"))
        .expect("corpus/rust/tests/query_goldens.txt (run `cargo xtask capture-query-goldens`)");

    let mut cases = 0;
    for block in goldens.split("@@ ").skip(1) {
        let (expr, body) = block.split_once('\n').expect("an expr line then its labels");
        let expr = expr.trim();
        let mut want: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
        want.sort();

        let out = run(&parity, GlobalFlags::default(), expr, Output::Label, false)
            .unwrap_or_else(|e| panic!("razel query `{expr}`: {e}"));
        let mut got: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
        got.sort();

        assert_eq!(got, want, "razel query `{expr}` diverges from the bazel-query golden");
        cases += 1;
    }
    assert!(cases >= 3, "expected the full tests() battery, captured {cases}");
}
