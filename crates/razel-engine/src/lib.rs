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
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};

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

    /// Parallel demand evaluation — the SAME validated values + the SAME recompute set as
    /// [`request`](Self::request), but independent action recomputes run concurrently on a pool of
    /// `jobs` workers. The GRAPH stays single-threaded: this coordinator thread owns it (the
    /// `RefCell`); only the compute closures move to workers — they run with no engine borrow held
    /// (§4.1b) and MUST NOT re-enter the engine (razel's actions don't). Early cutoff is preserved
    /// DYNAMICALLY: a node recomputes only once all its deps are validated this revision AND one
    /// actually changed since the node was last verified. `jobs <= 1` ⇒ the serial [`request`].
    /// Cancellation (set via [`set_cancel`](Self::set_cancel)) stops dispatch at an action boundary,
    /// clears the queue, and returns `Err("cancelled")` — in-flight actions finish (atomic). A
    /// panicking action is caught and surfaced as an `Err` (never a hang).
    ///
    /// Recompute-COUNT equivalence with serial holds for SUCCESSFUL (and cancelled/incremental)
    /// builds. On a FAILING build it may exceed serial's: serial stops at the first erroring dep,
    /// while the parallel evaluator can dispatch a sibling before observing that error. The final
    /// graph state and the returned `Err` are identical either way (a restart re-validates from the
    /// current revision), so warm == cold is preserved; only the (observability) count can differ.
    pub fn request_parallel(&self, key: &str, jobs: usize) -> Result<NodeValue, ComputeError> {
        let jobs = jobs.max(1);
        if jobs == 1 {
            return self.request(key);
        }
        let cur = self.revision.get();

        // 1. Collect the reachable sub-DAG (deps/rdeps/pending) + detect cycles + unknown nodes.
        let mut deps: HashMap<String, Vec<String>> = HashMap::new();
        let mut rdeps: HashMap<String, Vec<String>> = HashMap::new();
        let mut pending: HashMap<String, usize> = HashMap::new();
        {
            let nodes = self.nodes.borrow();
            #[derive(Clone, Copy)]
            enum Mark {
                Doing,
                Done,
            }
            let mut mark: HashMap<String, Mark> = HashMap::new();
            // iterative DFS, idx = next dep to descend; on-stack node = `Doing` ⇒ cycle.
            let mut stack: Vec<(String, usize)> = vec![(key.to_string(), 0)];
            while let Some((k, idx)) = stack.last().cloned() {
                let kdeps = match nodes.get(&k) {
                    Some(n) => match &n.kind {
                        Kind::Input => Vec::new(),
                        Kind::Derived { deps, .. } => deps.clone(),
                    },
                    None => return Err(format!("unknown node `{k}`")),
                };
                if idx == 0 {
                    mark.insert(k.clone(), Mark::Doing);
                    deps.entry(k.clone()).or_insert_with(|| kdeps.clone());
                    pending.entry(k.clone()).or_insert(kdeps.len());
                    rdeps.entry(k.clone()).or_default();
                }
                if idx < kdeps.len() {
                    stack.last_mut().unwrap().1 += 1;
                    let d = kdeps[idx].clone();
                    rdeps.entry(d.clone()).or_default().push(k.clone());
                    match mark.get(&d) {
                        Some(Mark::Doing) => return Err(format!("dependency cycle at `{d}`")),
                        Some(Mark::Done) => {}
                        None => stack.push((d, 0)),
                    }
                } else {
                    mark.insert(k.clone(), Mark::Done);
                    stack.pop();
                }
            }
        }

        // 2. Coordinator loop + worker pool. Ready = deps all validated this run.
        let mut ready: VecDeque<String> =
            pending.iter().filter(|(_, p)| **p == 0).map(|(k, _)| k.clone()).collect();
        let mut done: HashSet<String> = HashSet::new();
        let mut in_flight = 0usize;
        let mut err: Option<ComputeError> = None;

        type Task = (String, ComputeFn, Vec<DepValue>);
        let shared: Arc<(Mutex<(VecDeque<Task>, bool)>, Condvar)> =
            Arc::new((Mutex::new((VecDeque::new(), false)), Condvar::new()));
        let (res_tx, res_rx) = mpsc::channel::<(String, Result<NodeValue, ComputeError>)>();

        std::thread::scope(|scope| {
            for _ in 0..jobs {
                let shared = shared.clone();
                let res_tx = res_tx.clone();
                scope.spawn(move || {
                    loop {
                        let task = {
                            let (lock, cv) = &*shared;
                            let mut g = lock.lock().unwrap();
                            loop {
                                if let Some(t) = g.0.pop_front() {
                                    break Some(t);
                                }
                                if g.1 {
                                    break None;
                                }
                                g = cv.wait(g).unwrap();
                            }
                        };
                        match task {
                            Some((k, f, dvs)) => {
                                // Catch a panicking action: convert it to an Err result instead of
                                // letting the worker die — otherwise the coordinator would block on
                                // recv() forever with in_flight > 0 (a hung build). The build then
                                // fails cleanly with the panic message.
                                let outcome = std::panic::catch_unwind(
                                    std::panic::AssertUnwindSafe(|| f(&dvs)),
                                )
                                .unwrap_or_else(|p| {
                                    let msg = p
                                        .downcast_ref::<&str>()
                                        .map(|s| s.to_string())
                                        .or_else(|| p.downcast_ref::<String>().cloned())
                                        .unwrap_or_else(|| "panicked".into());
                                    Err(format!("action `{k}` panicked: {msg}"))
                                });
                                if res_tx.send((k, outcome)).is_err() {
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                });
            }
            // Drop the coordinator's own sender clone: now the only live senders are the workers',
            // so a `recv()` returning Err means every worker has exited (a real failure), not a hang.
            drop(res_tx);

            loop {
                if done.contains(key) || err.is_some() {
                    break;
                }
                if self.cancelled() {
                    err = Some("cancelled".into());
                    break;
                }
                // Dispatch ready nodes; cutoffs/inputs resolve inline (cascading) without a worker.
                while in_flight < jobs {
                    let Some(k) = ready.pop_front() else { break };
                    if done.contains(&k) {
                        continue;
                    }
                    let kdeps = deps[&k].clone();
                    let (is_input, has_value, verified_at, max_dep_changed, compute, dep_values) = {
                        let nodes = self.nodes.borrow();
                        let n = &nodes[&k];
                        let max_dep_changed =
                            kdeps.iter().map(|d| nodes[d].changed_at).max().unwrap_or(0);
                        let compute = match &n.kind {
                            Kind::Derived { compute, .. } => Some(compute.clone()),
                            Kind::Input => None,
                        };
                        let dep_values: Vec<DepValue> = kdeps
                            .iter()
                            .map(|d| DepValue {
                                key: d.clone(),
                                value: nodes[d].value.clone().expect("validated dep has a value"),
                            })
                            .collect();
                        (
                            matches!(n.kind, Kind::Input),
                            n.value.is_some(),
                            n.verified_at,
                            max_dep_changed,
                            compute,
                            dep_values,
                        )
                    };
                    if is_input || (has_value && (verified_at == cur || max_dep_changed <= verified_at))
                    {
                        // Input (authoritative), already-validated, or early cutoff: backdate, no run.
                        self.nodes.borrow_mut().get_mut(&k).unwrap().verified_at = cur;
                        relax_dependents(&k, &rdeps, &mut pending, &mut ready, &mut done);
                    } else {
                        self.recomputes.set(self.recomputes.get() + 1);
                        let (lock, cv) = &*shared;
                        lock.lock().unwrap().0.push_back((k, compute.unwrap(), dep_values));
                        cv.notify_one();
                        in_flight += 1;
                    }
                }
                if in_flight == 0 {
                    if !done.contains(key) {
                        err = Some(format!("dependency cycle at `{key}`"));
                    }
                    break;
                }
                match res_rx.recv() {
                    Ok((k, Ok(v))) => {
                        in_flight -= 1;
                        {
                            let mut nodes = self.nodes.borrow_mut();
                            let n = nodes.get_mut(&k).unwrap();
                            if n.value.as_ref() != Some(&v) {
                                n.changed_at = cur; // value changed → propagate to dependents
                            }
                            n.value = Some(v);
                            n.verified_at = cur;
                        }
                        relax_dependents(&k, &rdeps, &mut pending, &mut ready, &mut done);
                    }
                    Ok((_, Err(e))) => {
                        in_flight -= 1;
                        err = Some(e);
                        break;
                    }
                    // All workers gone with work still in flight — fail loud (never return a
                    // stale/unvalidated target value).
                    Err(_) => {
                        err = Some("razel engine: parallel workers exited unexpectedly".into());
                        break;
                    }
                }
            }

            // Stop the pool: clear queued work + signal; in-flight actions finish (atomic), scope joins.
            {
                let (lock, cv) = &*shared;
                let mut g = lock.lock().unwrap();
                g.0.clear();
                g.1 = true;
                cv.notify_all();
            }

            if let Some(e) = err {
                return Err(e);
            }
            Ok(self.nodes.borrow()[key].value.clone().expect("target validated"))
        })
    }
}

/// Mark `k` validated and relax its dependents: each dependent's unvalidated-dep count drops by one,
/// and a dependent that hits zero becomes ready. (Parallel-evaluator bookkeeping for
/// [`Engine::request_parallel`].)
fn relax_dependents(
    k: &str,
    rdeps: &HashMap<String, Vec<String>>,
    pending: &mut HashMap<String, usize>,
    ready: &mut VecDeque<String>,
    done: &mut HashSet<String>,
) {
    done.insert(k.to_string());
    if let Some(rs) = rdeps.get(k) {
        for r in rs {
            if let Some(p) = pending.get_mut(r) {
                *p -= 1;
                if *p == 0 {
                    ready.push_back(r.clone());
                }
            }
        }
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

    // ---- parallel evaluator (request_parallel) — must match serial exactly ----

    #[test]
    fn parallel_matches_serial_value_and_recompute_count() {
        // Same validated value AND same recompute set as serial, on a full build + an incremental edit.
        let par = graph();
        let v_par = par.request_parallel("D", 4).unwrap();
        let par_full = par.recomputes();
        let ser = graph();
        let v_ser = ser.request("D").unwrap();
        assert_eq!(v_par, v_ser, "parallel full-build value == serial");
        assert_eq!(par_full, ser.recomputes(), "full-build recompute count == serial (C + D)");

        par.reset_recomputes();
        par.set_input("A", nv("a1"));
        let v_par2 = par.request_parallel("D", 4).unwrap();
        let par_inc = par.recomputes();
        ser.reset_recomputes();
        ser.set_input("A", nv("a1"));
        let v_ser2 = ser.request("D").unwrap();
        assert_eq!(v_par2, v_ser2, "parallel incremental value == serial");
        assert_eq!(par_inc, ser.recomputes(), "incremental recompute count == serial");
    }

    #[test]
    fn parallel_preserves_early_cutoff_firewall() {
        let e = Engine::new();
        e.add_input("A", nv("a0"));
        e.add_derived("C", &["A"], |_| nv("CONST")); // C ignores A's content
        e.add_derived("D", &["C"], concat);
        e.request_parallel("D", 4).unwrap();
        e.reset_recomputes();
        e.set_input("A", nv("a1"));
        e.request_parallel("D", 4).unwrap();
        assert_eq!(e.recomputes(), 1, "C re-ran (A changed) but its value held → D firewalled");
    }

    #[test]
    fn parallel_detects_cycles() {
        let e = Engine::new();
        e.add_derived("X", &["Y"], concat);
        e.add_derived("Y", &["X"], concat);
        assert!(e.request_parallel("X", 4).is_err());
    }

    #[test]
    fn parallel_action_error_propagates() {
        let e = Engine::new();
        e.add_input("src", nv("s0"));
        e.add_action("boom", &["src"], |_| Err("action failed: rc=1".into()));
        let r = e.request_parallel("boom", 4);
        assert!(r.is_err() && r.unwrap_err().contains("rc=1"));
    }

    #[test]
    fn parallel_action_panic_becomes_error_not_hang() {
        // A panicking action MUST fail the build cleanly — without catch_unwind in the worker the
        // coordinator would block on recv() forever (the critical hang the review caught).
        let e = Engine::new();
        e.add_input("src", nv("s0"));
        e.add_action("boom", &["src"], |_| panic!("kaboom"));
        let r = e.request_parallel("boom", 4);
        assert!(r.is_err(), "a panicking action fails the build, never hangs");
        assert!(r.unwrap_err().contains("panicked"), "the panic surfaces as the error");
    }

    #[test]
    fn parallel_equals_from_scratch_with_manifests() {
        let mk = |seed: &str| {
            let e = Engine::new();
            e.add_input("src", nv(seed));
            e.add_action("act", &["src"], |deps| {
                let h = digest_of(&deps[0].value);
                Ok(NodeValue::Manifest(vec![("o.rlib".into(), Digest::of(h.as_bytes()))]))
            });
            e.add_derived("top", &["act"], concat);
            e
        };
        let warm = mk("v0");
        warm.request_parallel("top", 4).unwrap();
        warm.set_input("src", nv("v9"));
        let incremental = warm.request_parallel("top", 4).unwrap();
        let cold = mk("v9");
        assert_eq!(incremental, cold.request("top").unwrap());
    }

    #[test]
    fn parallel_aborts_when_cancelled_then_restarts() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let e = graph();
        let cancel = Arc::new(AtomicBool::new(false));
        e.set_cancel(Some(cancel.clone()));
        cancel.store(true, Ordering::Relaxed);
        let r = e.request_parallel("D", 4);
        assert!(
            r.is_err() && r.unwrap_err().contains("cancelled"),
            "a set cancel flag aborts the parallel build"
        );
        // Clear + restart → completes, equal to a fresh serial build (warm == cold).
        cancel.store(false, Ordering::Relaxed);
        let restarted = e.request_parallel("D", 4).expect("restart succeeds");
        assert_eq!(restarted, graph().request("D").unwrap());
    }

    #[test]
    fn parallel_runs_a_wide_independent_fan() {
        // 32 independent action nodes under one root → all run (concurrently), count == serial.
        let e = Engine::new();
        e.add_input("seed", nv("s"));
        let tops: Vec<String> = (0..32).map(|i| format!("a{i}")).collect();
        for a in &tops {
            e.add_action(a, &["seed"], |_| Ok(NodeValue::Digest(Digest::of(b"x"))));
        }
        let refs: Vec<&str> = tops.iter().map(String::as_str).collect();
        e.add_derived("root", &refs, concat);
        e.request_parallel("root", 8).unwrap();
        assert_eq!(e.recomputes(), 33, "32 actions + root, each computed once");
        // Warm no-op: nothing changed → zero recompute.
        e.reset_recomputes();
        e.request_parallel("root", 8).unwrap();
        assert_eq!(e.recomputes(), 0, "warm no-op via the parallel path is zero work");
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
