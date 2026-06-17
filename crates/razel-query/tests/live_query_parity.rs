//! Full Phase-1 `qg`: LIVE `bazel query` parity. The committed goldens were captured from real
//! `bazel query --noimplicit_deps <expr>` (`cargo xtask capture-query-goldens`); these tests run the
//! SAME expressions through `razel query` and assert matching results. Hermetic at test time (no
//! bazel/network — they read the goldens + run razel's load-only query engine). `--noimplicit_deps`
//! is razel query's v1 default (implicit-deps parity is Phase 6); the goldens were captured the same
//! way.

use razel_query::{GlobalFlags, Output, run};
use std::path::{Path, PathBuf};

/// The canonicalized parity workspace root.
fn parity() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../parity")
        .canonicalize()
        .expect("parity workspace")
}

/// Label-set parity harness: for each `@@ <expr>` block in the golden at `golden_rel`, run `expr`
/// through razel with `output` and assert the SORTED result lines match the committed bazel golden.
/// Shared by every line-oriented mode (`label` / `package` / verb results); the graph mode is
/// structural, so it has its own (set-based) comparison.
fn assert_output_parity(golden_rel: &str, output: Output, min_cases: usize) {
    let parity = parity();
    let goldens = std::fs::read_to_string(parity.join(golden_rel))
        .unwrap_or_else(|e| panic!("{golden_rel} (run `cargo xtask capture-query-goldens`): {e}"));

    let mut cases = 0;
    for block in goldens.split("@@ ").skip(1) {
        let (expr, body) = block.split_once('\n').expect("an expr line then its result lines");
        let expr = expr.trim();
        let mut want: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
        want.sort();

        // implicit=false → `--noimplicit_deps`, matching how the goldens were captured.
        let out = run(&parity, GlobalFlags::default(), expr, output, false)
            .unwrap_or_else(|e| panic!("razel query `{expr}`: {e}"));
        let mut got: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
        got.sort();

        assert_eq!(got, want, "razel query `{expr}` ({output:?}) diverges from the bazel golden");
        cases += 1;
    }
    assert!(cases >= min_cases, "expected ≥{min_cases} cases in {golden_rel}, captured {cases}");
}

#[test]
fn razel_query_matches_the_bazel_query_goldens() {
    assert_output_parity("corpus/rust/transitive/query_goldens.txt", Output::Label, 7);
}

/// P6.Q4: `--output=package` — the main-repo package form (no leading `//`, no repo) + the
/// cross-package union's deduped, sorted multi-package render.
#[test]
fn razel_query_package_output_matches_the_bazel_goldens() {
    assert_output_parity("corpus/rust/transitive/query_goldens_package.txt", Output::Package, 3);
}

/// P6.Q5: `tests()` over the test-bearing `corpus/rust/tests` — a `test_suite` expanded to its
/// members, a mixed `:all` (tests kept / suite expanded / library dropped), and a non-test → empty.
#[test]
fn razel_query_tests_verb_matches_the_bazel_goldens() {
    assert_output_parity("corpus/rust/tests/query_goldens.txt", Output::Label, 3);
}

/// P6 `visible(predicate, x)` over the 3-package `corpus/rust/visibility` — public / `//b:__pkg__` /
/// `//b:__subpackages__` / default-private, from a package and from a subpackage.
#[test]
fn razel_query_visible_matches_the_bazel_goldens() {
    assert_output_parity("corpus/rust/visibility/query_goldens.txt", Output::Label, 3);
}

/// P6 `--output=graph`: the GraphViz renderer against LIVE `bazel query --output=graph
/// --nograph:factored`. razel emits the UNFACTORED digraph; the gate compares the NODE + EDGE sets
/// (factoring- and order-insensitive — see `format_graph`) over the committed graph goldens.
#[test]
fn razel_query_graph_output_matches_the_bazel_goldens() {
    let parity = parity();
    let goldens =
        std::fs::read_to_string(parity.join("corpus/rust/transitive/query_goldens_graph.txt"))
            .expect("query_goldens_graph.txt (run `cargo xtask capture-query-goldens`)");

    let mut cases = 0;
    for block in goldens.split("@@ ").skip(1) {
        let (expr, body) = block.split_once('\n').expect("an expr line then its graph");
        let expr = expr.trim();
        let want = graph_sets(body);

        let out = run(&parity, GlobalFlags::default(), expr, Output::Graph, false)
            .unwrap_or_else(|e| panic!("razel query --output=graph `{expr}`: {e}"));
        let got = graph_sets(&out);

        assert_eq!(
            got, want,
            "razel query --output=graph `{expr}` diverges from the bazel-query golden"
        );
        cases += 1;
    }
    assert!(cases >= 3, "expected the full graph battery, captured {cases}");
}

type GraphSets = (
    std::collections::BTreeSet<String>,
    std::collections::BTreeSet<(String, String)>,
);

/// Extract (nodes, edges) from GraphViz digraph text — order- and factoring-insensitive. A `->` line
/// is an edge `(from, to)`; any other line carrying a quoted label is a node; the `digraph … {` /
/// `node [shape=box];` / `}` lines carry no quoted label and are skipped. (Safe because the goldens
/// are `--nograph:factored`, so no node bundles multiple newline-joined labels.)
fn graph_sets(dot: &str) -> GraphSets {
    let mut nodes = std::collections::BTreeSet::new();
    let mut edges = std::collections::BTreeSet::new();
    for line in dot.lines() {
        if let Some((a, b)) = line.split_once("->") {
            if let (Some(f), Some(t)) = (unquote(a), unquote(b)) {
                edges.insert((f, t));
            }
        } else if let Some(n) = unquote(line) {
            nodes.insert(n);
        }
    }
    (nodes, edges)
}

/// The label inside the first pair of double quotes on a line, or `None`.
fn unquote(s: &str) -> Option<String> {
    let start = s.find('"')? + 1;
    let end = s[start..].find('"')? + start;
    Some(s[start..end].to_string())
}
