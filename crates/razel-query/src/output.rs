//! P1.4: output formatting — `--output=label` (sorted labels) and `label_kind`
//! (`"<kind> <label>"`). Results are lexicographically sorted because the `LabelSet` is a
//! `BTreeSet` (the `--order_output=auto` analogue, §13).

use crate::graph::{LabelSet, QueryGraph};

/// The v1 output modes (§12). `--cbor`/`build`/`graph` are deferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Output {
    #[default]
    Label,
    LabelKind,
}

impl Output {
    /// Parse an `--output=` value, or a named error.
    pub fn parse(s: &str) -> Result<Output, String> {
        match s {
            "label" => Ok(Output::Label),
            "label_kind" => Ok(Output::LabelKind),
            "build" | "package" | "graph" | "proto" | "xml" | "cbor" => {
                Err(format!("--output={s} is deferred in query v1 (§12)"))
            }
            other => Err(format!("unknown --output={other}")),
        }
    }
}

/// Render a result set per the output mode — one label per line, lexicographically sorted. For
/// `label_kind`, a label with no known kind prints bare (defensive; shouldn't happen for results
/// drawn from the graph).
pub fn format(graph: &QueryGraph, set: &LabelSet, mode: Output) -> String {
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
        }
        out.push('\n');
    }
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
    fn output_parse_accepts_v1_and_defers_the_rest() {
        assert_eq!(Output::parse("label").unwrap(), Output::Label);
        assert_eq!(Output::parse("label_kind").unwrap(), Output::LabelKind);
        assert!(Output::parse("build").unwrap_err().contains("deferred"));
        assert!(Output::parse("nonsense").unwrap_err().contains("unknown"));
    }
}
