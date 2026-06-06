//! Incremental engine (Phase 6) — Skyframe-lite / salsa-style demand-driven recompute
//! with **early cutoff**.
//!
//! Each node has `value`, `verified_at` (last revision confirmed valid), and `changed_at`
//! (revision the value last actually changed). A request validates a node by checking
//! whether any dependency *changed* since the node was last verified; if not, it backdates
//! (no recompute). If a recompute produces an unchanged value, `changed_at` is **not**
//! bumped — so dependents are not recomputed (the firewall). Recomputes are counted, so
//! tests assert O(affected) and the equivalence property (incremental == from-scratch).

use razel_core::Digest;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

type Key = String;
type Rev = u64;
type ComputeFn = Box<dyn Fn(&[Digest]) -> Digest>;

enum Kind {
    Input,
    Derived { deps: Vec<Key>, compute: ComputeFn },
}

struct Node {
    kind: Kind,
    value: Option<Digest>,
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
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Define a leaf input with an initial value.
    pub fn add_input(&self, key: &str, value: Digest) {
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

    /// Define a derived node computed from `deps` (values passed in `deps` order).
    pub fn add_derived(
        &self,
        key: &str,
        deps: &[&str],
        compute: impl Fn(&[Digest]) -> Digest + 'static,
    ) {
        self.nodes.borrow_mut().insert(
            key.to_string(),
            Node {
                kind: Kind::Derived {
                    deps: deps.iter().map(|d| d.to_string()).collect(),
                    compute: Box::new(compute),
                },
                value: None, // forces compute on first request
                verified_at: 0,
                changed_at: 0,
            },
        );
    }

    /// Change an input. Bumps the revision; `changed_at` advances only if the value differs
    /// (input-level early cutoff).
    pub fn set_input(&self, key: &str, value: Digest) {
        let rev = self.revision.get() + 1;
        self.revision.set(rev);
        let mut nodes = self.nodes.borrow_mut();
        let n = nodes.get_mut(key).expect("unknown input");
        if n.value != Some(value) {
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

    /// Demand the (validated, up-to-date) value of `key`.
    pub fn request(&self, key: &str) -> Result<Digest, String> {
        if self.in_progress.borrow().contains(key) {
            return Err(format!("dependency cycle at `{key}`"));
        }
        self.in_progress.borrow_mut().insert(key.to_string());
        let r = self.request_inner(key);
        self.in_progress.borrow_mut().remove(key);
        r
    }

    fn request_inner(&self, key: &str) -> Result<Digest, String> {
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
            (n.verified_at, n.value, deps)
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

        // Validate/compute dependencies first.
        let mut dep_values = Vec::with_capacity(deps.len());
        let mut max_dep_changed = 0;
        for d in &deps {
            dep_values.push(self.request(d)?);
            max_dep_changed = max_dep_changed.max(self.nodes.borrow()[d].changed_at);
        }

        // Early cutoff: already computed and no dep changed since last verify → backdate.
        if let Some(v) = value
            && max_dep_changed <= verified_at
        {
            self.nodes.borrow_mut().get_mut(key).unwrap().verified_at = cur;
            return Ok(v);
        }

        // Recompute.
        self.recomputes.set(self.recomputes.get() + 1);
        let new = {
            let nodes = self.nodes.borrow();
            match &nodes[key].kind {
                Kind::Derived { compute, .. } => compute(&dep_values),
                Kind::Input => unreachable!(),
            }
        };
        {
            let mut nodes = self.nodes.borrow_mut();
            let n = nodes.get_mut(key).unwrap();
            if n.value != Some(new) {
                n.changed_at = cur; // value actually changed → propagate
            }
            n.value = Some(new);
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
    // concat the dep digests' hex → a content-derived value
    fn concat(parts: &[Digest]) -> Digest {
        let s: String = parts.iter().map(|p| p.to_hex()).collect();
        Digest::of(s.as_bytes())
    }

    /// A: input, B: input, C = f(A), D = f(C, B).
    fn graph() -> Engine {
        let e = Engine::new();
        e.add_input("A", d("a0"));
        e.add_input("B", d("b0"));
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
        e.set_input("B", d("b1")); // only D depends on B (not C)
        e.request("D").unwrap();
        assert_eq!(e.recomputes(), 1); // only D; C reused (early cutoff)
    }

    #[test]
    fn early_cutoff_stops_propagation_when_value_unchanged() {
        let e = Engine::new();
        e.add_input("A", d("a0"));
        e.add_derived("C", &["A"], |_| d("CONST")); // C ignores A
        e.add_derived("D", &["C"], concat);
        e.request("D").unwrap();
        e.reset_recomputes();

        e.set_input("A", d("a1")); // A changed → C revalidates...
        e.request("D").unwrap();
        // C recomputes (A changed) but its value is unchanged → D is NOT recomputed.
        assert_eq!(e.recomputes(), 1);
    }

    #[test]
    fn incremental_equals_from_scratch() {
        let e = graph();
        e.request("D").unwrap();
        e.set_input("A", d("a9"));
        e.set_input("B", d("b9"));
        let incremental = e.request("D").unwrap();

        let fresh = Engine::new();
        fresh.add_input("A", d("a9"));
        fresh.add_input("B", d("b9"));
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
            e.add_input("n0", d("seed"));
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
            e.set_input("n0", d("seed2"));
            e.request(&format!("n{n}")).unwrap();
            assert_eq!(e.recomputes(), n, "leaf edit propagates linearly for {n}");
        }
    }
}
