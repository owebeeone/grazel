//! P1.4: output formatting — `--output=label` (sorted labels) and `label_kind`
//! (`"<kind> <label>"`). Results are lexicographically sorted because the `LabelSet` is a
//! `BTreeSet` (the `--order_output=auto` analogue, §13).

use crate::graph::{LabelSet, QueryGraph};

/// The v1 output modes (§12). `--cbor`/`build`/`proto`/`xml` are deferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Output {
    #[default]
    Label,
    LabelKind,
    /// `--output=package` — the deduped, sorted PACKAGES of the result targets.
    Package,
    /// `--output=graph` — a GraphViz digraph of the result set's induced subgraph.
    Graph,
}

impl Output {
    /// Parse an `--output=` value, or a named error.
    pub fn parse(s: &str) -> Result<Output, String> {
        match s {
            "label" => Ok(Output::Label),
            "label_kind" => Ok(Output::LabelKind),
            "package" => Ok(Output::Package),
            "graph" => Ok(Output::Graph),
            "build" | "proto" | "xml" | "cbor" => {
                Err(format!("--output={s} is deferred in query v1 (§12)"))
            }
            other => Err(format!("unknown --output={other}")),
        }
    }
}

/// The package of a label for `--output=package` — `//pkg:name` → `pkg` (Bazel's main-repo form);
/// an external `@repo//pkg:name` keeps its repo prefix (`@repo//pkg`).
fn package_of(label: &str) -> &str {
    let pkg = label.rsplit_once(':').map(|(p, _)| p).unwrap_or(label);
    pkg.strip_prefix("//").unwrap_or(pkg)
}

/// Render a result set per the output mode — one line per item, lexicographically sorted. `package`
/// emits the DEDUPED packages of the results; `label_kind` prefixes the kind (a label with no known
/// kind prints bare — defensive; shouldn't happen for results drawn from the graph). `graph` is
/// rendered by [`format_graph`] (it needs the `implicit` flag), dispatched in `run`.
pub fn format(graph: &QueryGraph, set: &LabelSet, mode: Output) -> String {
    if mode == Output::Package {
        let pkgs: std::collections::BTreeSet<&str> = set.iter().map(|l| package_of(l)).collect();
        return pkgs.into_iter().map(|p| format!("{p}\n")).collect();
    }
    let mut out = String::new();
    for label in set {
        match mode {
            Output::Label => out.push_str(label),
            Output::LabelKind => match graph.kind(label) {
                Some(k) => {
                    out.push_str(k);
                    out.push(' ');
                    out.push_str(label);
                }
                None => out.push_str(label),
            },
            Output::Package => unreachable!("handled above"),
            Output::Graph => unreachable!("rendered via format_graph (run dispatches)"),
        }
        out.push('\n');
    }
    out
}

/// `--output=graph` (§13): a GraphViz digraph of the result set's INDUCED subgraph — every result
/// node declared, plus `N -> S` for each direct successor S that is also in the set. razel emits the
/// UNFACTORED form (one node per target); bazel's default node-factoring is a display grouping of the
/// same logical graph, so the parity gate compares the node + edge SETS (factoring- and
/// order-insensitive). Nodes (BTree order) and each node's edges are sorted for determinism.
/// `implicit` mirrors `--implicit_deps` (drives whether `Implicit` edges are drawn).
pub fn format_graph(graph: &QueryGraph, set: &LabelSet, implicit: bool) -> String {
    let mut out = String::from("digraph mygraph {\n  node [shape=box];\n");
    for node in set {
        out.push_str(&format!("  \"{node}\"\n"));
        let mut succs: Vec<String> = graph
            .successors(node, implicit)
            .into_iter()
            .filter(|s| set.contains(s))
            .collect();
        succs.sort();
        succs.dedup();
        for s in succs {
            out.push_str(&format!("  \"{node}\" -> \"{s}\"\n"));
        }
    }
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use razel_loading::{Edge, EdgeKind, LoadedTarget, TargetKind};
    use std::collections::BTreeMap;

    fn graph() -> QueryGraph {
        let mut m = BTreeMap::new();
        m.insert(
            "//a:lib".into(),
            LoadedTarget {
                label: "//a:lib".into(),
                repo: String::new(),
                package: "a".into(),
                rule_class: "rust_library".into(),
                kind: TargetKind::Library,
                attrs: BTreeMap::new(),
                edges: vec![Edge { to: "//a:lib.rs".into(), kind: EdgeKind::SourceFile, attr: "srcs".into() }],
                raw_refs: vec![],
            },
        );
        QueryGraph::new(m)
    }

    #[test]
    fn output_modes_render_sorted_lines() {
        let g = graph();
        let set: LabelSet = ["//a:lib".to_string(), "//a:lib.rs".to_string()].into_iter().collect();
        assert_eq!(format(&g, &set, Output::Label), "//a:lib\n//a:lib.rs\n");
        assert_eq!(
            format(&g, &set, Output::LabelKind),
            "rust_library rule //a:lib\nsource file //a:lib.rs\n"
        );
    }

    #[test]
    fn output_package_dedupes_to_package_names() {
        let g = graph();
        let set: LabelSet =
            ["//a:lib".to_string(), "//a:lib.rs".to_string()].into_iter().collect();
        // both targets live in package `a` → one deduped line, `//` stripped (Bazel's form).
        assert_eq!(format(&g, &set, Output::Package), "a\n");
    }

    #[test]
    fn output_graph_renders_the_induced_subgraph() {
        let g = graph();
        let set: LabelSet =
            ["//a:lib".to_string(), "//a:lib.rs".to_string()].into_iter().collect();
        // lib edges to lib.rs (both in set) → one edge; both nodes declared; unfactored.
        assert_eq!(
            format_graph(&g, &set, false),
            "digraph mygraph {\n  node [shape=box];\n  \"//a:lib\"\n  \"//a:lib\" -> \"//a:lib.rs\"\n  \"//a:lib.rs\"\n}\n"
        );
    }

    #[test]
    fn output_parse_accepts_v1_and_defers_the_rest() {
        assert_eq!(Output::parse("label").unwrap(), Output::Label);
        assert_eq!(Output::parse("label_kind").unwrap(), Output::LabelKind);
        assert_eq!(Output::parse("package").unwrap(), Output::Package);
        assert_eq!(Output::parse("graph").unwrap(), Output::Graph);
        assert!(Output::parse("build").unwrap_err().contains("deferred"));
        assert!(Output::parse("nonsense").unwrap_err().contains("unknown"));
    }
}
