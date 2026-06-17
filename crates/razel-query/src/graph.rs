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
    /// leaves. `implicit` includes `Implicit` edges (`--implicit_deps`). `pub(crate)` so the
    /// `--output=graph` renderer can draw each node's induced-subgraph edges (P6.Q-graph).
    pub(crate) fn successors(&self, label: &str, implicit: bool) -> Vec<String> {
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

    /// `same_pkg_direct_rdeps(x)` — every target in the SAME package as a target in `x` that
    /// DIRECTLY depends on it (Bazel §12): the direct reverse-deps, restricted to the owning package.
    pub fn same_pkg_direct_rdeps(&self, roots: &LabelSet) -> LabelSet {
        fn pkg(l: &str) -> &str {
            l.rsplit_once(':').map(|(p, _)| p).unwrap_or(l)
        }
        let mut out = LabelSet::new();
        for t in roots {
            let tpkg = pkg(t);
            if let Some(preds) = self.rev.get(t) {
                for p in preds {
                    if pkg(p) == tpkg {
                        out.insert(p.clone());
                    }
                }
            }
        }
        out
    }

    /// `allpaths(from, to)` = the node set on SOME path `from`→`to` = the forward closure of
    /// `from` intersected with what can still reach `to` (`rdeps` within that closure). An
    /// order-independent set — a clean golden (§13).
    pub fn allpaths(&self, from: &LabelSet, to: &LabelSet, implicit: bool) -> LabelSet {
        let forward = self.deps(from, None, implicit);
        self.rdeps(&forward, to, None)
    }

    /// `somepath(from, to)` = the nodes on ONE shortest path from some `from` to some `to`
    /// (multi-source BFS; successors visited in sorted order for razel's own determinism). Bazel
    /// returns *a* shortest path, so a golden must accept any valid one (§13). Empty if no path.
    pub fn somepath(&self, from: &LabelSet, to: &LabelSet, implicit: bool) -> LabelSet {
        use std::collections::VecDeque;
        let mut parent: BTreeMap<String, Option<String>> = BTreeMap::new();
        let mut q: VecDeque<String> = VecDeque::new();
        for s in from {
            parent.insert(s.clone(), None);
            q.push_back(s.clone());
        }
        while let Some(cur) = q.pop_front() {
            if to.contains(&cur) {
                let mut path = LabelSet::new();
                let mut node = Some(cur);
                while let Some(n) = node {
                    node = parent.get(&n).cloned().flatten();
                    path.insert(n);
                }
                return path;
            }
            let mut succ = self.successors(&cur, implicit);
            succ.sort();
            for s in succ {
                if !parent.contains_key(&s) {
                    parent.insert(s.clone(), Some(cur.clone()));
                    q.push_back(s);
                }
            }
        }
        LabelSet::new()
    }

    /// Match a target pattern against the loaded graph → the label set. `//...`, `//pkg/...`,
    /// `//pkg:all` (RULE targets), `//pkg:*` / `//pkg:all-targets` (rules PLUS source/generated
    /// files AND the `BUILD` file — Bazel's `:*` semantics), and a concrete `//pkg:name` (exact).
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
        // `:*` / `:all-targets` — every target in the package: rule targets AND the source/generated
        // file targets (which live in `kinds`, not `targets`) AND the synthetic `BUILD` file (Bazel
        // lists it). `:all` (below) is the rule-only subset.
        if let Some(pkg) = body.strip_suffix(":*").or_else(|| body.strip_suffix(":all-targets")) {
            let mut s: LabelSet = self
                .kinds
                .keys()
                .filter(|l| {
                    l.strip_prefix("//").and_then(|b| b.split_once(':')).map(|(p, _)| p) == Some(pkg)
                })
                .cloned()
                .collect();
            s.insert(format!("//{pkg}:BUILD"));
            return s;
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
        // `:*` / `:all-targets` add the source files + the BUILD file (Bazel's all-targets).
        assert_eq!(
            g.match_pattern("//app:*"),
            set(&["//app:BUILD", "//app:base", "//app:bin", "//app:lib", "//app:lib.rs"])
        );
        assert_eq!(g.match_pattern("//app:all-targets"), g.match_pattern("//app:*"));
    }

    #[test]
    fn somepath_and_allpaths_over_a_diamond() {
        // bin → l1 → base ; bin → l2 → base
        let mut m = BTreeMap::new();
        m.insert("//d:base".into(), target("//d:base", "d", "rust_library", vec![]));
        m.insert("//d:l1".into(), target("//d:l1", "d", "rust_library", vec![("//d:base", EdgeKind::Rule)]));
        m.insert("//d:l2".into(), target("//d:l2", "d", "rust_library", vec![("//d:base", EdgeKind::Rule)]));
        m.insert(
            "//d:bin".into(),
            target("//d:bin", "d", "rust_binary", vec![("//d:l1", EdgeKind::Rule), ("//d:l2", EdgeKind::Rule)]),
        );
        let g = QueryGraph::new(m);
        let (from, to) = (set(&["//d:bin"]), set(&["//d:base"]));
        // allpaths = every node on any path (both branches).
        assert_eq!(g.allpaths(&from, &to, false), set(&["//d:base", "//d:bin", "//d:l1", "//d:l2"]));
        // somepath = ONE shortest path; sorted successors make it deterministic (l1 < l2).
        assert_eq!(g.somepath(&from, &to, false), set(&["//d:base", "//d:bin", "//d:l1"]));
        // no path → empty.
        assert!(g.somepath(&to, &from, false).is_empty());
    }
}
