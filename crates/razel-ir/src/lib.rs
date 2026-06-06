//! The dialect-agnostic build graph (§2.1) — the engine-as-product core (D6).
//!
//! File/Action/Target/(Result) nodes; forward `consumes` edges and **explicitly stored
//! reverse edges (rdeps)**, the single shared index that serves engine invalidation,
//! the agent impact query, and the client stream (D2/F12). Impact traversal is
//! O(affected + their edges) — never a full-graph scan (the no-accidental-O(n²) rule).

use razel_core::{ActionId, Digest, FileId, NodeRef, TargetId};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Source,
    Generated,
}

#[derive(Clone, Debug)]
pub struct FileNode {
    pub id: FileId,
    pub digest: Option<Digest>,
    pub exists: bool,
    pub kind: FileKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetKind {
    Library,
    Binary,
    Test,
}

#[derive(Clone, Debug)]
pub struct TargetNode {
    pub id: TargetId,
    pub kind: TargetKind,
}

#[derive(Clone, Debug)]
pub struct ActionNode {
    pub id: ActionId,
    pub content_key: Digest,
}

/// The build graph. Forward + reverse adjacency are maintained together so that
/// reverse reachability (rdeps / impact) is output-sensitive.
#[derive(Default)]
pub struct Graph {
    files: HashMap<FileId, FileNode>,
    actions: HashMap<ActionId, ActionNode>,
    targets: HashMap<TargetId, TargetNode>,
    /// node -> nodes it consumes / depends on.
    fwd: HashMap<NodeRef, HashSet<NodeRef>>,
    /// node -> its dependents (reverse deps). The keystone index.
    rev: HashMap<NodeRef, HashSet<NodeRef>>,
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_file(&mut self, n: FileNode) {
        self.files.insert(n.id.clone(), n);
    }
    pub fn add_action(&mut self, n: ActionNode) {
        self.actions.insert(n.id.clone(), n);
    }
    pub fn add_target(&mut self, n: TargetNode) {
        self.targets.insert(n.id.clone(), n);
    }

    pub fn file(&self, id: &FileId) -> Option<&FileNode> {
        self.files.get(id)
    }
    pub fn action(&self, id: &ActionId) -> Option<&ActionNode> {
        self.actions.get(id)
    }
    pub fn target(&self, id: &TargetId) -> Option<&TargetNode> {
        self.targets.get(id)
    }

    /// Record that `dependent` consumes/depends-on `dependency`, maintaining both
    /// the forward and the reverse (rdep) adjacency.
    pub fn add_dep(&mut self, dependent: NodeRef, dependency: NodeRef) {
        self.fwd
            .entry(dependent.clone())
            .or_default()
            .insert(dependency.clone());
        self.rev.entry(dependency).or_default().insert(dependent);
    }

    /// Every transitive dependent of `node` (its rdeps closure). BFS over the stored
    /// reverse edges: O(affected + their edges), independent of total graph size.
    pub fn affects(&self, node: &NodeRef) -> BTreeSet<NodeRef> {
        let mut seen = BTreeSet::new();
        let mut q = VecDeque::new();
        if let Some(deps) = self.rev.get(node) {
            for d in deps {
                if seen.insert(d.clone()) {
                    q.push_back(d.clone());
                }
            }
        }
        while let Some(n) = q.pop_front() {
            if let Some(deps) = self.rev.get(&n) {
                for d in deps {
                    if seen.insert(d.clone()) {
                        q.push_back(d.clone());
                    }
                }
            }
        }
        seen
    }

    /// Impact of editing `file`, partitioned into candidate-affected **tests** and
    /// **deliverables** (§2.1). "Candidate" = transitively depends on the file — the
    /// right set for test selection; not proven runtime coverage.
    pub fn impacted_targets(&self, file: &FileId) -> (BTreeSet<TargetId>, BTreeSet<TargetId>) {
        let mut tests = BTreeSet::new();
        let mut deliverables = BTreeSet::new();
        for nr in self.affects(&NodeRef::File(file.clone())) {
            if let NodeRef::Target(tid) = nr {
                match self.targets.get(&tid).map(|t| t.kind) {
                    Some(TargetKind::Test) => {
                        tests.insert(tid);
                    }
                    Some(_) => {
                        deliverables.insert(tid);
                    }
                    None => {}
                }
            }
        }
        (tests, deliverables)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fid(s: &str) -> FileId {
        FileId::new(s)
    }
    fn aid(s: &str) -> ActionId {
        ActionId::new(s)
    }
    fn tid(s: &str) -> TargetId {
        TargetId::new(s)
    }

    /// Build: file -> action -> target chain. `target` depends-on `action` depends-on `file`.
    fn chain(g: &mut Graph, f: &str, a: &str, t: &str, kind: TargetKind) {
        g.add_file(FileNode {
            id: fid(f),
            digest: Some(Digest::of(f.as_bytes())),
            exists: true,
            kind: FileKind::Source,
        });
        g.add_action(ActionNode {
            id: aid(a),
            content_key: Digest::of(a.as_bytes()),
        });
        g.add_target(TargetNode { id: tid(t), kind });
        g.add_dep(NodeRef::Action(aid(a)), NodeRef::File(fid(f)));
        g.add_dep(NodeRef::Target(tid(t)), NodeRef::Action(aid(a)));
    }

    #[test]
    fn nodes_round_trip() {
        let mut g = Graph::new();
        chain(&mut g, "f", "a", "t", TargetKind::Library);
        assert!(g.file(&fid("f")).is_some());
        assert_eq!(g.target(&tid("t")).unwrap().kind, TargetKind::Library);
    }

    #[test]
    fn affects_walks_rdeps_transitively() {
        let mut g = Graph::new();
        chain(&mut g, "f", "a", "t", TargetKind::Binary);
        let aff = g.affects(&NodeRef::File(fid("f")));
        assert!(aff.contains(&NodeRef::Action(aid("a"))));
        assert!(aff.contains(&NodeRef::Target(tid("t"))));
        assert_eq!(aff.len(), 2);
    }

    #[test]
    fn affects_is_output_sensitive_not_whole_graph() {
        // Two unrelated chains; editing f1 must NOT pull in the f2 subgraph.
        let mut g = Graph::new();
        chain(&mut g, "f1", "a1", "t1", TargetKind::Binary);
        chain(&mut g, "f2", "a2", "t2", TargetKind::Binary);
        let aff = g.affects(&NodeRef::File(fid("f1")));
        assert_eq!(aff.len(), 2); // a1, t1 only
        assert!(!aff.contains(&NodeRef::Target(tid("t2"))));
        assert!(!aff.contains(&NodeRef::Action(aid("a2"))));
    }

    #[test]
    fn impact_partitions_tests_from_deliverables() {
        // f -> a, and both a test target and a library target consume a.
        let mut g = Graph::new();
        chain(&mut g, "f", "a", "lib", TargetKind::Library);
        g.add_target(TargetNode {
            id: tid("tst"),
            kind: TargetKind::Test,
        });
        g.add_dep(NodeRef::Target(tid("tst")), NodeRef::Action(aid("a")));

        let (tests, deliverables) = g.impacted_targets(&fid("f"));
        assert_eq!(tests, BTreeSet::from([tid("tst")]));
        assert_eq!(deliverables, BTreeSet::from([tid("lib")]));
    }
}
