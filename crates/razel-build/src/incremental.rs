//! Incremental execution — build through the [`razel_engine::Engine`] so a warm
//! rebuild recomputes only the **affected** action subgraph (D2/F12), not the
//! whole target.
//!
//! The build graph maps onto the engine directly:
//!   - each source file → an **input** node (value = `NodeValue::Digest`, its content digest);
//!   - each action → an **action** node (`add_action`, C1) whose deps are its inputs (a
//!     generated input depends on the *producing action's* node), whose compute runs the
//!     action via [`execute_action`] (C2) and returns its output `NodeValue::Manifest`;
//!   - each target → a **derived** node over its action nodes.
//!
//! On a file edit, [`IncrementalBuilder::sync_file`] re-digests it and feeds the engine; the
//! next [`build`](IncrementalBuilder::build) re-runs only the actions whose transitive inputs
//! changed (output-level early-cutoff stops propagation when an action's manifest comes out
//! identical). The action node's value being its *output manifest* is what makes that firewall
//! work.
//!
//! The engine memoizes the value; the **output files** are produced as a side effect of
//! `execute_action`. When the engine skips an action (inputs unchanged) the outputs are
//! assumed already present in the (warm) exec root — exactly the daemon's persistent-workspace
//! model.
//!
//! C1/C2 migration (WS-C): action input digests come from the **named, owned** [`DepValue`]s
//! (a leaf is a `Digest`; a generated input is selected BY PATH out of the producing action's
//! `Manifest`) — no filesystem re-read. An action's failure propagates as the engine's
//! `Err(ComputeError)`; no separate error sink.

use crate::{AnalyzedTarget, analyze_build};
use razel_actions::Action;
use razel_core::Digest;
use razel_engine::{DepValue, Engine, NodeValue};
use razel_exec::{Cache, Isolation, Materialize, Sandbox, digest_path, execute_action};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// One incremental build session over a fixed exec root + cache. Holds the warm
/// engine graph; not `Send` (the engine is single-threaded — serialize builds
/// behind a lock if shared).
pub struct IncrementalBuilder {
    engine: Engine,
    exec_root: PathBuf,
    cache: Rc<Cache>,
    /// Leaf (source) input node keys that exist on disk and can be `sync_file`d.
    leaf_inputs: HashSet<String>,
    /// How each action's sandbox materializes its inputs (symlink vs hardlink).
    materialize: Materialize,
    /// OS-level confinement for each action (e.g. macOS Seatbelt).
    isolation: Isolation,
}

fn file_key(path: &str) -> String {
    format!("file:{path}")
}
fn target_key(name: &str) -> String {
    format!("tgt:{name}")
}
fn action_key(target: &str, i: usize) -> String {
    format!("act:{target}#{i}")
}

/// A leaf source file's node value: its canonical content digest (C2 `digest_path`); a missing
/// file digests as empty (symmetric with the executor skipping absent inputs).
fn leaf_value(path: &Path) -> NodeValue {
    NodeValue::Digest(digest_path(path).unwrap_or_else(|| Digest::of(b"")))
}

/// Flatten a dep value to a stable byte string for the target-aggregation node.
fn dep_bytes(v: &NodeValue) -> String {
    match v {
        NodeValue::Digest(g) => g.to_hex(),
        NodeValue::Manifest(m) => m.iter().map(|(p, g)| format!("{p}={}", g.to_hex())).collect(),
    }
}

impl IncrementalBuilder {
    pub fn new(exec_root: impl Into<PathBuf>, cache: Cache) -> Self {
        Self {
            engine: Engine::new(),
            exec_root: exec_root.into(),
            cache: Rc::new(cache),
            leaf_inputs: HashSet::new(),
            materialize: Materialize::default(),
            isolation: Isolation::default(),
        }
    }

    /// Choose how sandboxes materialize inputs (symlink, the default, or hardlink).
    pub fn with_materialize(mut self, how: Materialize) -> Self {
        self.materialize = how;
        self
    }

    /// Apply OS-level confinement to every action (e.g. macOS Seatbelt).
    pub fn with_isolation(mut self, isolation: Isolation) -> Self {
        self.isolation = isolation;
        self
    }

    /// Wire a single BUILD's analyzed targets into the engine graph (once per BUILD).
    pub fn configure(&mut self, build_src: &str) -> Result<(), String> {
        self.configure_targets(analyze_build(build_src)?)
    }

    /// Wire PRE-ANALYZED targets into the engine graph — the WORKSPACE-LABEL path (WS-C item 3):
    /// the daemon's actor passes `analyze_workspace_resolved(...)`'s targets (`//:razel` plus its
    /// transitive deps) here, not a single BUILD. Each leaf input node is inserted once
    /// (`leaf_inputs` guard), so this is safe to call once per warm graph.
    pub fn configure_targets(&mut self, targets: Vec<AnalyzedTarget>) -> Result<(), String> {
        // Which action produces each generated file → that file depends on it.
        let mut producer: HashMap<String, String> = HashMap::new();
        for t in &targets {
            for (i, act) in t.actions.iter().enumerate() {
                for out in &act.outputs {
                    producer.insert(out.clone(), action_key(&t.name, i));
                }
            }
        }

        for t in &targets {
            let mut act_keys = Vec::new();
            for (i, act) in t.actions.iter().enumerate() {
                let akey = action_key(&t.name, i);
                // Deps (in act.inputs order, one per input): a generated input → its producing
                // action node; else a leaf file input node.
                let mut deps = Vec::new();
                for inp in &act.inputs {
                    if let Some(prod) = producer.get(inp) {
                        deps.push(prod.clone());
                    } else {
                        let fk = file_key(inp);
                        if self.leaf_inputs.insert(fk.clone()) {
                            self.engine
                                .add_input(&fk, leaf_value(&self.exec_root.join(inp)));
                        }
                        deps.push(fk);
                    }
                }

                // Each action gets a PERSISTENT sandbox, reused across rebuilds — only the
                // changed input links are fixed up, and a content-only change is zero churn.
                let sb_dir = self
                    .exec_root
                    .join(".razel-sandbox")
                    .join(akey.replace([':', '#'], "_"));
                let sandbox = Rc::new(RefCell::new(
                    Sandbox::persistent(sb_dir, self.materialize)
                        .map_err(|e| e.to_string())?
                        .with_isolation(self.isolation),
                ));

                // The action node: restore-or-run via execute_action (C2); value = its output
                // Manifest (so output-level early cutoff tracks the produced bytes). Input
                // digests come from the dep VALUES (named, C1) — never a filesystem re-read.
                let argv = act.argv.clone();
                let input_paths = act.inputs.clone();
                let outputs = act.outputs.clone();
                let cache = self.cache.clone();
                let exec_root = self.exec_root.clone();
                let dep_refs: Vec<&str> = deps.iter().map(String::as_str).collect();
                self.engine.add_action(&akey, &dep_refs, move |dep_values| {
                    run_action(
                        &argv,
                        &input_paths,
                        &outputs,
                        dep_values,
                        &cache,
                        &exec_root,
                        &sandbox,
                    )
                });
                act_keys.push(akey);
            }
            // Target node: a pure aggregation over its action manifests.
            let tkey = target_key(&t.name);
            let act_refs: Vec<&str> = act_keys.iter().map(String::as_str).collect();
            self.engine.add_derived(&tkey, &act_refs, |deps| {
                let s: String = deps.iter().map(|dv| dep_bytes(&dv.value)).collect();
                NodeValue::Digest(Digest::of(s.as_bytes()))
            });
        }
        Ok(())
    }

    /// An edited file changed on disk: re-digest it and feed the engine. The next `build`
    /// recomputes only what transitively depends on it. Guarded by `leaf_inputs` — a `set_input`
    /// on an unknown key panics (engine), so an edit to a non-leaf / new path is a no-op here
    /// (the daemon actor routes those through a Rescan instead, §3.5).
    pub fn sync_file(&self, path: &str) {
        let key = file_key(path);
        if self.leaf_inputs.contains(&key) {
            self.engine
                .set_input(&key, leaf_value(&self.exec_root.join(path)));
        }
    }

    /// Build `target`; returns how many engine nodes recomputed (the O(affected) metric). An
    /// action failure surfaces as the engine request's `Err`.
    pub fn build(&self, target: &str) -> Result<usize, String> {
        self.engine.reset_recomputes();
        self.engine.request(&target_key(target))?;
        Ok(self.engine.recomputes())
    }
}

/// Run one action (cache restore-or-run via [`execute_action`], C2) in its persistent
/// `sandbox`; the value is its output [`Manifest`]. Input digests come from the named dep
/// values (`dep_values` is in `input_paths` order): a leaf dep is a `Digest`; a generated input
/// is selected BY PATH from the producing action's `Manifest`. No filesystem re-read.
#[allow(clippy::too_many_arguments)]
fn run_action(
    argv: &[String],
    input_paths: &[String],
    outputs: &[String],
    dep_values: &[DepValue],
    cache: &Cache,
    exec_root: &Path,
    sandbox: &Rc<RefCell<Sandbox>>,
) -> Result<NodeValue, String> {
    let mut input_digests = BTreeMap::new();
    for (inp, dv) in input_paths.iter().zip(dep_values.iter()) {
        let dg = match &dv.value {
            NodeValue::Digest(g) => *g,
            NodeValue::Manifest(m) => m
                .iter()
                .find(|(p, _)| p == inp)
                .map(|(_, g)| *g)
                .ok_or_else(|| format!("input `{inp}` not produced by `{}`", dv.key))?,
        };
        input_digests.insert(inp.clone(), dg);
    }
    let action = Action {
        argv: argv.to_vec(),
        inputs: input_digests,
        env: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
        tools: BTreeMap::new(),
        platform: "host".into(),
        outputs: outputs.to_vec(),
    };
    let mut sb = sandbox.borrow_mut();
    match execute_action(&action, cache, exec_root, &mut sb) {
        ExecOutcome::Cached(m) | ExecOutcome::Executed(m) => Ok(NodeValue::Manifest(m)),
        ExecOutcome::Failed(msg) => Err(msg),
    }
}

// Bring the C2 outcome enum into scope for `run_action`'s match.
use razel_exec::ExecOutcome;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // A rule emitting one action per src: each copies src -> src.out (cc-independent).
    const BUILD: &str = r#"
def _impl(ctx):
    outs = []
    for s in ctx.attr.srcs:
        o = s + ".out"
        ctx.actions.run(executable = "/bin/sh", outputs = [o], inputs = [s],
                        arguments = ["-c", "cat " + s + " > " + o])
        outs.append(o)
    return [DefaultInfo(files = outs)]
multi = rule(implementation = _impl, attrs = {"srcs": 1})
multi(name = "lib", srcs = ["x.txt", "y.txt"])
"#;

    #[test]
    fn rebuild_recomputes_only_the_affected_action() {
        let exec = tempfile::tempdir().unwrap();
        std::fs::write(exec.path().join("x.txt"), "hello").unwrap();
        std::fs::write(exec.path().join("y.txt"), "world").unwrap();
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        let mut b = IncrementalBuilder::new(exec.path(), cache);
        b.configure(BUILD).unwrap();

        // First build: both actions + the target node compute.
        let first = b.build("lib").unwrap();
        assert_eq!(first, 3, "cold build: act(x) + act(y) + tgt");
        assert_eq!(
            std::fs::read_to_string(exec.path().join("x.txt.out")).unwrap(),
            "hello"
        );
        assert_eq!(
            std::fs::read_to_string(exec.path().join("y.txt.out")).unwrap(),
            "world"
        );

        // Edit only x.txt → rebuild recomputes act(x) + tgt; act(y) is firewalled.
        std::fs::write(exec.path().join("x.txt"), "HELLO").unwrap();
        b.sync_file("x.txt");
        let second = b.build("lib").unwrap();
        assert_eq!(second, 2, "only act(x) + tgt recompute — act(y) skipped");
        assert_eq!(
            std::fs::read_to_string(exec.path().join("x.txt.out")).unwrap(),
            "HELLO"
        );

        // No change → a third build recomputes nothing.
        let third = b.build("lib").unwrap();
        assert_eq!(third, 0, "no input changed → zero recompute");
    }

    #[test]
    fn builds_correctly_with_hardlink_materialization() {
        let exec = tempfile::tempdir().unwrap();
        std::fs::write(exec.path().join("x.txt"), "hello").unwrap();
        std::fs::write(exec.path().join("y.txt"), "world").unwrap();
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // Zero-copy hardlink materialization (sandbox is under exec → same fs).
        let mut b =
            IncrementalBuilder::new(exec.path(), cache).with_materialize(Materialize::Hardlink);
        b.configure(BUILD).unwrap();

        assert_eq!(b.build("lib").unwrap(), 3);
        assert_eq!(
            std::fs::read_to_string(exec.path().join("x.txt.out")).unwrap(),
            "hello"
        );

        // In-place edit (truncate+write) → only the affected action recomputes,
        // and the hardlink reflects the new content.
        std::fs::write(exec.path().join("x.txt"), "HELLO").unwrap();
        b.sync_file("x.txt");
        assert_eq!(b.build("lib").unwrap(), 2, "only act(x) + tgt recompute");
        assert_eq!(
            std::fs::read_to_string(exec.path().join("x.txt.out")).unwrap(),
            "HELLO"
        );
    }

    #[test]
    fn incremental_matches_a_fresh_build_of_the_final_state() {
        if !Path::new("/bin/sh").exists() {
            return;
        }
        let run = |edits: &[(&str, &str)]| -> String {
            let exec = tempfile::tempdir().unwrap();
            std::fs::write(exec.path().join("x.txt"), "x0").unwrap();
            std::fs::write(exec.path().join("y.txt"), "y0").unwrap();
            let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();
            let mut b = IncrementalBuilder::new(exec.path(), cache);
            b.configure(BUILD).unwrap();
            b.build("lib").unwrap();
            for (f, v) in edits {
                std::fs::write(exec.path().join(f), v).unwrap();
                b.sync_file(f);
            }
            b.build("lib").unwrap();
            // The observable result: the produced output contents.
            format!(
                "{}|{}",
                std::fs::read_to_string(exec.path().join("x.txt.out")).unwrap(),
                std::fs::read_to_string(exec.path().join("y.txt.out")).unwrap()
            )
        };
        // Warm graph after an edit == fresh graph built straight to the final state.
        assert_eq!(run(&[("x.txt", "x1")]), "x1|y0");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn builds_under_seatbelt_isolation() {
        use razel_exec::seatbelt_available;
        if !seatbelt_available() {
            return;
        }
        let exec = tempfile::tempdir().unwrap();
        std::fs::write(exec.path().join("x.txt"), "hello").unwrap();
        std::fs::write(exec.path().join("y.txt"), "world").unwrap();
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // Each action runs write-confined + network-blocked; outputs land in the
        // sandbox (allowed) and razel captures them back to exec_root itself.
        let mut b = IncrementalBuilder::new(exec.path(), cache)
            .with_isolation(Isolation::Seatbelt { network: false });
        b.configure(BUILD).unwrap();
        assert_eq!(b.build("lib").unwrap(), 3);
        assert_eq!(
            std::fs::read_to_string(exec.path().join("x.txt.out")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn action_failure_surfaces_as_an_error() {
        let exec = tempfile::tempdir().unwrap();
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();
        let bad = r#"
def _impl(ctx):
    ctx.actions.run(executable = "/bin/sh", outputs = ["out"], inputs = [],
                    arguments = ["-c", "exit 3"])
    return [DefaultInfo(files = ["out"])]
boom = rule(implementation = _impl, attrs = {})
boom(name = "boom")
"#;
        let mut b = IncrementalBuilder::new(exec.path(), cache);
        b.configure(bad).unwrap();
        let err = b.build("boom").unwrap_err();
        assert!(err.contains("action failed"), "got: {err}");
    }

    #[test]
    fn configure_targets_accepts_pre_analyzed_workspace_targets() {
        // WS-C item 3: the actor passes analyze_workspace_resolved's targets directly (here we
        // simulate with analyze_build's output). The warm graph builds + firewalls a no-op.
        let exec = tempfile::tempdir().unwrap();
        std::fs::write(exec.path().join("x.txt"), "hello").unwrap();
        std::fs::write(exec.path().join("y.txt"), "world").unwrap();
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();
        let targets = crate::analyze_build(BUILD).unwrap();
        let mut b = IncrementalBuilder::new(exec.path(), cache);
        b.configure_targets(targets).unwrap();
        assert_eq!(b.build("lib").unwrap(), 3, "cold build via pre-analyzed targets");
        assert_eq!(b.build("lib").unwrap(), 0, "warm no-op = zero recompute");
    }
}
