//! Incremental engine (Phase 6) — Skyframe-lite / salsa-style demand-driven recompute
//! with **early cutoff**.
//!
//! Each node has `value`, `verified_at` (last revision confirmed valid), and `changed_at`
//! (revision the value last actually changed). A request validates a node by checking
//! whether any dependency *changed* since the node was last verified; if not, it backdates
//! (no recompute). If a recompute produces an unchanged value, `changed_at` is **not**
//! bumped — so dependents are not recomputed (the firewall). Recomputes are counted, so
//! tests assert O(affected) and the equivalence property (incremental == from-scratch).
//!
//! C1 (FixMissingServerImplementationPlan §4.1): a node value is a [`NodeValue`] — a single
//! content digest (pure node) OR an output [`Manifest`] (an *action* node, one (path,digest)
//! per declared output). A [`ComputeFn`] receives **named, owned** [`DepValue`]s (so a
//! consumer can pick one output of a multi-output upstream action by path — §4.1 named-deps)
//! and is **effectful + fallible** (an action runs inside it). Compute runs with the engine's
//! borrow RELEASED (the `Rc` clone below), so a long-running or re-entrant action cannot
//! deadlock the `RefCell` (§4.1b).

use razel_core::Digest;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

type Key = String;
type Rev = u64;

/// A canonical output manifest: `(path, digest)` entries, sorted by `path`, unique
/// (RULE 22). A directory output's digest is the deterministic tree-hash (== `digest_input`'s
/// input-dir hash); an absent declared output is omitted (C2 §4.2).
pub type Manifest = Vec<(String, Digest)>;

/// One compute error at the engine seam. String-based (matches `request`'s `Result` and the
/// `IncrementalBuilder` error sink); a richer `razel_core` error would be a deliberate
/// C1 re-freeze.
pub type ComputeError = String;

/// A node's computed value: a single content digest (pure node) OR an output manifest
/// (action node).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeValue {
    Digest(Digest),
    Manifest(Manifest),
}

impl NodeValue {
    /// Early-cutoff equality: structural (same variant + equal contents). Manifests are
    /// canonical, so the derived `Vec` compare is total.
    pub fn digest_equal(&self, other: &NodeValue) -> bool {
        self == other
    }
}

/// A dependency value handed to a [`ComputeFn`]: the dependency's KEY plus its (owned,
/// cloned) value. Owned — NOT a borrow of engine-internal state — so the closure can run
/// arbitrarily long / re-enter the engine without holding a `RefCell` borrow (§4.1b), and so
/// a consumer can reconstruct the exact `path → digest` map (e.g. select one output of a
/// multi-output upstream action) without re-reading the filesystem (§4.1 named-deps). A
/// `ComputeFn` MUST NOT retain a `DepValue` past its return.
#[derive(Debug, Clone)]
pub struct DepValue {
    pub key: Key,
    pub value: NodeValue,
}

/// Effectful + fallible compute over named dep values. `Arc` (not `Rc`) + `Send + Sync` so a
/// recompute can clone it and drop the engine borrow before invoking (re-entrancy / long-action
/// safety) AND so the parallel evaluator can move it to a worker thread (the graph stays
/// single-threaded on the coordinator; only the compute runs on the pool).
type ComputeFn = Arc<dyn Fn(&[DepValue]) -> Result<NodeValue, ComputeError> + Send + Sync>;

enum Kind {
    Input,
    Derived { deps: Vec<Key>, compute: ComputeFn },
}

struct Node {
    kind: Kind,
    value: Option<NodeValue>,
    verified_at: Rev,
    changed_at: Rev,
}

/// A demand-driven incremental computation graph.
#[derive(Default)]
pub struct Engine {
    nodes: RefCell<HashMap<Key, Node>>,
    revision: Cell<Rev>,
    recomputes: Cell<usize>,
    in_progress: RefCell<HashSet<Key>>,
    /// Cooperative cancellation flag, checked between node validations (bazel cancel-and-restart,
    /// R-9.5). Shared (`Arc`) with the daemon's watcher thread; `None` = never cancels.
    cancel: RefCell<Option<Arc<AtomicBool>>>,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Define a leaf input with an initial value.
    pub fn add_input(&self, key: &str, value: NodeValue) {
        let rev = self.revision.get();
        self.nodes.borrow_mut().insert(
            key.to_string(),
            Node {
                kind: Kind::Input,
                value: Some(value),
                verified_at: rev,
                changed_at: rev,
            },
        );
    }

    /// Define a PURE derived node: a deterministic function of its dep values that cannot
    /// fail (a programming bug if it does). Used for aggregation nodes (e.g. combine over
    /// action manifests) — NOT for actions.
    pub fn add_derived(
        &self,
        key: &str,
        deps: &[&str],
        compute: impl Fn(&[DepValue]) -> NodeValue + Send + Sync + 'static,
    ) {
        self.insert_derived(key, deps, Arc::new(move |d| Ok(compute(d))));
    }

    /// Define an EFFECTFUL action node: runs the action (spawn + capture + store) inside
    /// `execute` and returns its output [`Manifest`] (so output-level early cutoff has teeth)
    /// or an error.
    pub fn add_action(
        &self,
        key: &str,
        deps: &[&str],
        execute: impl Fn(&[DepValue]) -> Result<NodeValue, ComputeError> + Send + Sync + 'static,
    ) {
        self.insert_derived(key, deps, Arc::new(execute));
    }

    fn insert_derived(&self, key: &str, deps: &[&str], compute: ComputeFn) {
        self.nodes.borrow_mut().insert(
            key.to_string(),
            Node {
                kind: Kind::Derived {
                    deps: deps.iter().map(|d| d.to_string()).collect(),
                    compute,
                },
                value: None, // forces compute on first request
                verified_at: 0,
                changed_at: 0,
            },
        );
    }

    /// Change an input. Bumps the revision; `changed_at` advances only if the value differs
    /// (input-level early cutoff). Panics on an unknown key — callers (the daemon actor)
    /// MUST establish leaf inputs via analysis/projection before `set_input` (§3.5/§4.1).
    pub fn set_input(&self, key: &str, value: NodeValue) {
        let rev = self.revision.get() + 1;
        self.revision.set(rev);
        let mut nodes = self.nodes.borrow_mut();
        let n = nodes.get_mut(key).expect("unknown input");
        if n.value.as_ref() != Some(&value) {
            n.value = Some(value);
            n.changed_at = rev;
        }
        n.verified_at = rev;
    }

    pub fn recomputes(&self) -> usize {
        self.recomputes.get()
    }
    pub fn reset_recomputes(&self) {
        self.recomputes.set(0);
    }

    /// Install (or clear with `None`) a cancellation flag, checked between node validations. When
    /// it reads true, an in-flight [`request`](Self::request) aborts with `Err("cancelled")` at
    /// the next action boundary (an in-flight action is atomic — never interrupted mid-spawn). The
    /// daemon actor sets it when a watcher event arrives during a build, then drains the
    /// invalidations and re-`request`s — bazel cancel-and-restart (R-9.5). A cancelled-then-
    /// restarted build re-validates from the current revision, so warm == cold still holds.
    pub fn set_cancel(&self, flag: Option<Arc<AtomicBool>>) {
        *self.cancel.borrow_mut() = flag;
    }
    fn cancelled(&self) -> bool {
        self.cancel
            .borrow()
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    }

    /// Demand the (validated, up-to-date) value of `key`.
    pub fn request(&self, key: &str) -> Result<NodeValue, ComputeError> {
        if self.in_progress.borrow().contains(key) {
            return Err(format!("dependency cycle at `{key}`"));
        }
        self.in_progress.borrow_mut().insert(key.to_string());
        let r = self.request_inner(key);
        self.in_progress.borrow_mut().remove(key);
        r
    }

    fn request_inner(&self, key: &str) -> Result<NodeValue, ComputeError> {
        // Cooperative cancellation: checked before each node's validation/recompute, so a build
        // aborts promptly at an action boundary (bazel cancel-and-restart, R-9.5).
        if self.cancelled() {
            return Err("cancelled".into());
        }
        let cur = self.revision.get();

        // Snapshot what we need without holding the borrow across recursion.
        let (verified_at, value, deps) = {
            let nodes = self.nodes.borrow();
            let n = nodes
                .get(key)
                .ok_or_else(|| format!("unknown node `{key}`"))?;
            let deps = match &n.kind {
                Kind::Input => None,
                Kind::Derived { deps, .. } => Some(deps.clone()),
            };
            (n.verified_at, n.value.clone(), deps)
        };

        // Already validated this revision.
        if verified_at == cur
            && let Some(v) = value
        {
            return Ok(v);
        }

        let Some(deps) = deps else {
            // Input: its value is authoritative; mark verified.
            self.nodes.borrow_mut().get_mut(key).unwrap().verified_at = cur;
            return Ok(value.expect("input has no value"));
        };

        // Validate/compute dependencies first, carrying each dep's KEY + value (named deps).
        let mut dep_values = Vec::with_capacity(deps.len());
        let mut max_dep_changed = 0;
        for d in &deps {
            let v = self.request(d)?;
            dep_values.push(DepValue {
                key: d.clone(),
                value: v,
            });
            max_dep_changed = max_dep_changed.max(self.nodes.borrow()[d].changed_at);
        }

        // Early cutoff: already computed and no dep changed since last verify → backdate.
        if let Some(v) = value
            && max_dep_changed <= verified_at
        {
            self.nodes.borrow_mut().get_mut(key).unwrap().verified_at = cur;
            return Ok(v);
        }

        // Recompute. Clone the compute `Rc` and DROP the borrow before invoking, so an
        // effectful/long/re-entrant action does not hold the engine's `RefCell` (§4.1b).
        let compute = {
            let nodes = self.nodes.borrow();
            match &nodes[key].kind {
                Kind::Derived { compute, .. } => compute.clone(),
                Kind::Input => unreachable!(),
            }
        };
        self.recomputes.set(self.recomputes.get() + 1);
        let new = compute(&dep_values)?; // borrow released; action error propagates
        {
            let mut nodes = self.nodes.borrow_mut();
            let n = nodes.get_mut(key).unwrap();
            if n.value.as_ref() != Some(&new) {
                n.changed_at = cur; // value actually changed → propagate
            }
            n.value = Some(new.clone());
            n.verified_at = cur;
        }
        Ok(new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Digest {
        Digest::of(s.as_bytes())
    }
    fn nv(s: &str) -> NodeValue {
        NodeValue::Digest(d(s))
    }
    fn digest_of(v: &NodeValue) -> String {
        match v {
            NodeValue::Digest(g) => g.to_hex(),
            NodeValue::Manifest(m) => m.iter().map(|(p, g)| format!("{p}={}", g.to_hex())).collect(),
        }
    }
    // concat the dep values' content → a content-derived value
    fn concat(deps: &[DepValue]) -> NodeValue {
        let s: String = deps.iter().map(|dp| digest_of(&dp.value)).collect();
        NodeValue::Digest(Digest::of(s.as_bytes()))
    }

    /// A: input, B: input, C = f(A), D = f(C, B).
    fn graph() -> Engine {
        let e = Engine::new();
        e.add_input("A", nv("a0"));
        e.add_input("B", nv("b0"));
        e.add_derived("C", &["A"], concat);
        e.add_derived("D", &["C", "B"], concat);
        e
    }

    #[test]
    fn first_build_computes_each_derived_once() {
        let e = graph();
        e.request("D").unwrap();
        assert_eq!(e.recomputes(), 2); // C and D
    }

    #[test]
    fn changing_one_input_recomputes_only_affected() {
        let e = graph();
        e.request("D").unwrap();
        e.reset_recomputes();
        e.set_input("B", nv("b1")); // only D depends on B (not C)
        e.request("D").unwrap();
        assert_eq!(e.recomputes(), 1); // only D; C reused (early cutoff)
    }

    #[test]
    fn early_cutoff_stops_propagation_when_value_unchanged() {
        let e = Engine::new();
        e.add_input("A", nv("a0"));
        e.add_derived("C", &["A"], |_| nv("CONST")); // C ignores A
        e.add_derived("D", &["C"], concat);
        e.request("D").unwrap();
        e.reset_recomputes();

        e.set_input("A", nv("a1")); // A changed → C revalidates...
        e.request("D").unwrap();
        // C recomputes (A changed) but its value is unchanged → D is NOT recomputed.
        assert_eq!(e.recomputes(), 1);
    }

    #[test]
    fn incremental_equals_from_scratch() {
        let e = graph();
        e.request("D").unwrap();
        e.set_input("A", nv("a9"));
        e.set_input("B", nv("b9"));
        let incremental = e.request("D").unwrap();

        let fresh = Engine::new();
        fresh.add_input("A", nv("a9"));
        fresh.add_input("B", nv("b9"));
        fresh.add_derived("C", &["A"], concat);
        fresh.add_derived("D", &["C", "B"], concat);
        let from_scratch = fresh.request("D").unwrap();

        assert_eq!(incremental, from_scratch);
    }

    #[test]
    fn detects_cycles() {
        let e = Engine::new();
        e.add_derived("X", &["Y"], concat);
        e.add_derived("Y", &["X"], concat);
        assert!(e.request("X").is_err());
    }

    #[test]
    fn scales_linearly_no_quadratic_blowup() {
        // A chain of N derived nodes; full build = N recomputes (linear, not N^2).
        for n in [16usize, 32, 64, 128] {
            let e = Engine::new();
            e.add_input("n0", nv("seed"));
            for i in 1..=n {
                let dep = format!("n{}", i - 1);
                e.add_derived(&format!("n{i}"), &[&dep], concat);
            }
            e.request(&format!("n{n}")).unwrap();
            assert_eq!(
                e.recomputes(),
                n,
                "full build of chain {n} must be exactly N"
            );

            // Editing the leaf and rebuilding recomputes the whole chain (all affected) —
            // still exactly N, never N^2.
            e.reset_recomputes();
            e.set_input("n0", nv("seed2"));
            e.request(&format!("n{n}")).unwrap();
            assert_eq!(e.recomputes(), n, "leaf edit propagates linearly for {n}");
        }
    }

    // ---- C1 additions: action nodes, manifests, named deps, error propagation ----

    fn manifest(pairs: &[(&str, &str)]) -> NodeValue {
        let mut m: Manifest = pairs.iter().map(|(p, c)| (p.to_string(), d(c))).collect();
        m.sort_by(|a, b| a.0.cmp(&b.0)); // canonical
        NodeValue::Manifest(m)
    }

    #[test]
    fn action_node_returns_manifest() {
        let e = Engine::new();
        e.add_input("src", nv("s0"));
        e.add_action("act", &["src"], |_| Ok(manifest(&[("out.rlib", "r0")])));
        let v = e.request("act").unwrap();
        assert_eq!(v, manifest(&[("out.rlib", "r0")]));
        assert_eq!(e.recomputes(), 1);
    }

    #[test]
    fn action_output_early_cutoff_firewalls_dependents() {
        // act ignores its input's CONTENT (always the same manifest) → a downstream consumer
        // is not recomputed when the input changes but the action output does not.
        let e = Engine::new();
        e.add_input("src", nv("s0"));
        e.add_action("act", &["src"], |_| Ok(manifest(&[("o", "CONST")])));
        e.add_derived("down", &["act"], concat);
        e.request("down").unwrap();
        e.reset_recomputes();
        e.set_input("src", nv("s1"));
        e.request("down").unwrap();
        assert_eq!(e.recomputes(), 1); // act re-ran; its manifest unchanged → down firewalled
    }

    /// G15 / c1_named_deps_not_lost: a producer emits TWO outputs; a downstream action
    /// consumes exactly ONE by PATH. Fails if `DepValue` ever loses the path (e.g. narrowed
    /// back to a flat `&[Digest]`).
    #[test]
    fn named_deps_select_one_output_of_a_multi_output_producer() {
        let e = Engine::new();
        e.add_input("src", nv("s0"));
        e.add_action("producer", &["src"], |_| {
            Ok(manifest(&[("a.rlib", "AAAA"), ("b.rmeta", "BBBB")]))
        });
        // downstream picks ONLY b.rmeta out of the producer's manifest, by path.
        e.add_action("consumer", &["producer"], |deps| {
            let prod = &deps
                .iter()
                .find(|dp| dp.key == "producer")
                .expect("producer dep present")
                .value;
            let NodeValue::Manifest(m) = prod else {
                return Err("producer must be a manifest".into());
            };
            let (_, g) = m
                .iter()
                .find(|(p, _)| p == "b.rmeta")
                .ok_or("b.rmeta not found — path was lost")?;
            Ok(NodeValue::Digest(*g))
        });
        let v = e.request("consumer").unwrap();
        assert_eq!(v, NodeValue::Digest(d("BBBB")));
    }

    #[test]
    fn action_error_propagates() {
        let e = Engine::new();
        e.add_input("src", nv("s0"));
        e.add_action("boom", &["src"], |_| Err("action failed: rc=1".into()));
        let r = e.request("boom");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("rc=1"));
    }

    #[test]
    fn incremental_equals_from_scratch_with_manifests() {
        let mk = |seed: &str| {
            let e = Engine::new();
            e.add_input("src", nv(seed));
            // an action whose manifest is content-derived from its input
            e.add_action("act", &["src"], |deps| {
                let h = digest_of(&deps[0].value);
                Ok(NodeValue::Manifest(vec![("o.rlib".into(), Digest::of(h.as_bytes()))]))
            });
            e.add_derived("top", &["act"], concat);
            e
        };
        let warm = mk("v0");
        warm.request("top").unwrap();
        warm.set_input("src", nv("v9"));
        let incremental = warm.request("top").unwrap();

        let cold = mk("v9");
        let from_scratch = cold.request("top").unwrap();
        assert_eq!(incremental, from_scratch);
    }

    #[test]
    fn request_aborts_when_cancel_flag_set_then_restarts() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let e = graph();
        let cancel = Arc::new(AtomicBool::new(false));
        e.set_cancel(Some(cancel.clone()));
        assert!(e.request("D").is_ok(), "uncancelled build succeeds");

        // A watcher event arrives mid-build: set the flag + invalidate → the next request aborts.
        cancel.store(true, Ordering::Relaxed);
        e.set_input("A", nv("a-changed"));
        let r = e.request("D");
        assert!(
            r.is_err() && r.unwrap_err().contains("cancelled"),
            "build cancels when the flag is set"
        );

        // Drain + clear + restart → completes, equal to a fresh build of the final state
        // (cancel-AND-restart preserves warm == cold).
        cancel.store(false, Ordering::Relaxed);
        let restarted = e.request("D").expect("restart succeeds");
        let fresh = graph();
        fresh.set_input("A", nv("a-changed"));
        assert_eq!(restarted, fresh.request("D").unwrap());
    }
}
