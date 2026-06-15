//! P1.3: the query graph — the engine's OWN forward/reverse adjacency over the §11
//! `LoadedTarget` edges (NOT `razel_ir::Graph`; design §13). `deps`/`rdeps` are BFS over it;
//! pattern matching filters the loaded labels; each label carries a kind string for `kind()` /
//! `--output=label_kind` (P1.4).

use razel_loading::{EdgeKind, LoadedTarget};
use std::collections::{BTreeMap, BTreeSet};

/// A label set — the value every query expression evaluates to. Sorted (BTree) for deterministic,
/// lexicographic output (§13).
pub type LabelSet = BTreeSet<String>;

pub struct QueryGraph {
    /// Rule targets by canonical label (these carry edges; files/leaves do not).
    targets: BTreeMap<String, LoadedTarget>,
    /// Reverse adjacency `to → {from}` for `rdeps`.
    rev: BTreeMap<String, BTreeSet<String>>,
    /// Every label that appears (target or edge target) → its kind string.
    kinds: BTreeMap<String, String>,
}

impl QueryGraph {
    /// Build the forward/reverse adjacency + kind map from the loaded graph.
    pub fn new(targets: BTreeMap<String, LoadedTarget>) -> Self {
        let mut rev: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut kinds: BTreeMap<String, String> = BTreeMap::new();
        for (label, t) in &targets {
            kinds.insert(label.clone(), format!("{} rule", t.rule_class));
            for e in &t.edges {
                rev.entry(e.to.clone()).or_default().insert(label.clone());
                // A non-target edge target is a file leaf — classify it from the edge kind.
                if !targets.contains_key(&e.to) {
                    let k = match e.kind {
                        EdgeKind::SourceFile => "source file",
                        EdgeKind::GeneratedFile => "generated file",
                        // Rule/Alias/ConfigSetting/Implicit point at targets (or are gated
                        // elsewhere); only files are non-target leaves to name here.
                        _ => continue,
                    };
                    kinds.entry(e.to.clone()).or_insert_with(|| k.to_string());
                }
            }
        }
        QueryGraph { targets, rev, kinds }
    }

    /// Every loaded rule-target label.
    pub fn all_targets(&self) -> LabelSet {
        self.targets.keys().cloned().collect()
    }

    /// The kind string of a label (`"<rule_class> rule"` / `"source file"` / `"generated file"`),
    /// or `None` if the label is unknown to the graph.
    pub fn kind(&self, label: &str) -> Option<&str> {
        self.kinds.get(label).map(String::as_str)
    }

    /// The `LoadedTarget` for a label, if it is a rule target (for `attr`/`labels`, P1.4).
    pub fn target(&self, label: &str) -> Option<&LoadedTarget> {
        self.targets.get(label)
    }

    /// Forward successors of a label: its edges' targets. Only rule targets have edges; files are
    /// leaves. `implicit` includes `Implicit` edges (`--implicit_deps`).
    fn successors(&self, label: &str, implicit: bool) -> Vec<String> {
        match self.targets.get(label) {
            Some(t) => t
                .edges
                .iter()
                .filter(|e| implicit || e.kind != EdgeKind::Implicit)
                .map(|e| e.to.clone())
                .collect(),
            None => Vec::new(),
        }
    }

    /// `deps`: forward transitive closure from `roots` (roots included), depth-bounded
    /// (`depth` counts traversed edges; roots are depth 0). Cycle-safe.
    pub fn deps(&self, roots: &LabelSet, depth: Option<u32>, implicit: bool) -> LabelSet {
        let mut seen: LabelSet = roots.clone();
        let mut frontier: Vec<String> = roots.iter().cloned().collect();
        let mut d = 0u32;
        while !frontier.is_empty() {
            if depth.is_some_and(|max| d >= max) {
                break;
            }
            let mut next = Vec::new();
            for label in &frontier {
                for s in self.successors(label, implicit) {
                    if seen.insert(s.clone()) {
                        next.push(s);
                    }
                }
            }
            frontier = next;
            d += 1;
        }
        seen
    }

    /// `rdeps`: reverse closure from `roots`, restricted to `universe` (roots∩universe included),
    /// depth-bounded. Cycle-safe.
    pub fn rdeps(&self, universe: &LabelSet, roots: &LabelSet, depth: Option<u32>) -> LabelSet {
        let mut seen: LabelSet = roots.iter().filter(|r| universe.contains(*r)).cloned().collect();
        let mut frontier: Vec<String> = seen.iter().cloned().collect();
        let mut d = 0u32;
        while !frontier.is_empty() {
            if depth.is_some_and(|max| d >= max) {
                break;
            }
            let mut next = Vec::new();
            for label in &frontier {
                if let Some(preds) = self.rev.get(label) {
                    for p in preds {
                        if universe.contains(p) && seen.insert(p.clone()) {
                            next.push(p.clone());
                        }
                    }
                }
            }
            frontier = next;
            d += 1;
        }
        seen
    }

    /// Match a target pattern against the loaded rule targets → the label set. `//...`, `//pkg/...`,
    /// `//pkg:all`, and a concrete `//pkg:name` (exact). (Files aren't matched by patterns — Bazel
    /// patterns name targets; files enter the set via `deps`.)
    pub fn match_pattern(&self, pattern: &str) -> LabelSet {
        let body = pattern.strip_prefix("//").unwrap_or(pattern);
        if body == "..." || body == "...:all" {
            return self.all_targets();
        }
        if let Some(pfx) = body.strip_suffix("/...:all").or_else(|| body.strip_suffix("/...")) {
            return self
                .targets
                .values()
                .filter(|t| t.package == pfx || t.package.starts_with(&format!("{pfx}/")))
                .map(|t| t.label.clone())
                .collect();
        }
        if let Some(pkg) = body.strip_suffix(":all") {
            return self
                .targets
                .values()
                .filter(|t| t.package == pkg)
                .map(|t| t.label.clone())
                .collect();
        }
        // concrete label — exact match.
        self.targets.keys().filter(|l| l.as_str() == pattern).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use razel_loading::{Edge, TargetKind};

    fn target(label: &str, pkg: &str, rule: &str, edges: Vec<(&str, EdgeKind)>) -> LoadedTarget {
        LoadedTarget {
            label: label.into(),
            repo: String::new(),
            package: pkg.into(),
            rule_class: rule.into(),
            kind: TargetKind::Library,
            attrs: BTreeMap::new(),
            edges: edges
                .into_iter()
                .map(|(to, kind)| Edge { to: to.into(), kind, attr: "deps".into() })
                .collect(),
            raw_refs: vec![],
        }
    }

    /// app:bin → app:lib → app:base; app:lib also has a source file.
    fn fixture() -> QueryGraph {
        let mut m = BTreeMap::new();
        m.insert("//app:base".into(), target("//app:base", "app", "rust_library", vec![]));
        m.insert(
            "//app:lib".into(),
            target("//app:lib", "app", "rust_library", vec![
                ("//app:base", EdgeKind::Rule),
                ("//app:lib.rs", EdgeKind::SourceFile),
            ]),
        );
        m.insert(
            "//app:bin".into(),
            target("//app:bin", "app", "rust_binary", vec![("//app:lib", EdgeKind::Rule)]),
        );
        QueryGraph::new(m)
    }

    fn set(xs: &[&str]) -> LabelSet {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn deps_is_the_forward_closure_including_files() {
        let g = fixture();
        assert_eq!(
            g.deps(&set(&["//app:bin"]), None, false),
            set(&["//app:bin", "//app:lib", "//app:base", "//app:lib.rs"])
        );
        // depth 1 stops after one edge hop.
        assert_eq!(g.deps(&set(&["//app:bin"]), Some(1), false), set(&["//app:bin", "//app:lib"]));
    }

    #[test]
    fn rdeps_is_the_reverse_closure_within_universe() {
        let g = fixture();
        let universe = g.all_targets();
        assert_eq!(
            g.rdeps(&universe, &set(&["//app:base"]), None),
            set(&["//app:base", "//app:lib", "//app:bin"])
        );
    }

    #[test]
    fn kind_strings_cover_targets_and_files() {
        let g = fixture();
        assert_eq!(g.kind("//app:bin"), Some("rust_binary rule"));
        assert_eq!(g.kind("//app:lib.rs"), Some("source file"));
        assert_eq!(g.kind("//nope"), None);
    }

    #[test]
    fn patterns_match_all_pkg_and_concrete() {
        let g = fixture();
        assert_eq!(g.match_pattern("//..."), set(&["//app:base", "//app:bin", "//app:lib"]));
        assert_eq!(g.match_pattern("//app:all"), set(&["//app:base", "//app:bin", "//app:lib"]));
        assert_eq!(g.match_pattern("//app:lib"), set(&["//app:lib"]));
        assert_eq!(g.match_pattern("//app/..."), g.match_pattern("//app:all"));
    }
}
