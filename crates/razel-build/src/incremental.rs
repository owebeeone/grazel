//! Incremental execution — build through the [`razel_engine::Engine`] so a warm
//! rebuild recomputes only the **affected** action subgraph (D2/F12), not the
//! whole target.
//!
//! The build graph maps onto the engine directly:
//!   - each source file        → an **input** node (value = its content digest);
//!   - each action             → a **derived** node whose deps are its inputs
//!     (a generated input depends on the *producing action's* node), and whose
//!     compute runs the action (cache restore-or-run) and returns the digest of
//!     its outputs;
//!   - each target             → a **derived** node over its action nodes.
//!
//! On a file edit, [`IncrementalBuilder::sync_file`] re-digests it and feeds the
//! engine; the next [`build`](IncrementalBuilder::build) re-runs only the actions
//! whose transitive inputs changed (early-cutoff stops propagation when an
//! action's outputs come out identical). The action node's value being its
//! *output* digest is what makes that firewall work.
//!
//! The engine memoizes the digest; the **output files** are produced as a side
//! effect of `compute`. When the engine skips an action (inputs unchanged) the
//! outputs are assumed already present in the (warm) exec root — exactly the
//! daemon's persistent-workspace model.
//!
//! `compute` is infallible (`Fn(&[Digest]) -> Digest`), so action failures are
//! captured in a shared error sink and surfaced by `build` after the request.

use crate::analyze_build;
use razel_actions::Action;
use razel_core::Digest;
use razel_engine::Engine;
use razel_exec::{Cache, Materialize, Sandbox, build_action_in};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

/// One incremental build session over a fixed exec root + cache. Holds the warm
/// engine graph; not `Send` (the engine is single-threaded — serialize builds
/// behind a lock if shared).
pub struct IncrementalBuilder {
    engine: Engine,
    exec_root: PathBuf,
    cache: Rc<Cache>,
    errors: Rc<RefCell<Vec<String>>>,
    /// Leaf (source) input node keys that exist on disk and can be `sync_file`d.
    leaf_inputs: HashSet<String>,
    /// How each action's sandbox materializes its inputs (symlink vs hardlink).
    materialize: Materialize,
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

fn digest_of(path: &std::path::Path) -> Digest {
    std::fs::read(path)
        .map(|b| Digest::of(&b))
        .unwrap_or_else(|_| Digest::of(b""))
}

/// Combine dependency digests into a node value (order-sensitive).
fn combine(parts: &[Digest]) -> Digest {
    Digest::of(
        parts
            .iter()
            .map(|p| p.to_hex())
            .collect::<String>()
            .as_bytes(),
    )
}

impl IncrementalBuilder {
    pub fn new(exec_root: impl Into<PathBuf>, cache: Cache) -> Self {
        Self {
            engine: Engine::new(),
            exec_root: exec_root.into(),
            cache: Rc::new(cache),
            errors: Rc::new(RefCell::new(Vec::new())),
            leaf_inputs: HashSet::new(),
            materialize: Materialize::default(),
        }
    }

    /// Choose how sandboxes materialize inputs (symlink, the default, or hardlink).
    pub fn with_materialize(mut self, how: Materialize) -> Self {
        self.materialize = how;
        self
    }

    /// Wire a BUILD's analyzed targets into the engine graph (once per BUILD).
    pub fn configure(&mut self, build_src: &str) -> Result<(), String> {
        let targets = analyze_build(build_src)?;

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
                // Deps: a generated input → its producing action; else a leaf file input.
                let mut deps = Vec::new();
                for inp in &act.inputs {
                    if let Some(prod) = producer.get(inp) {
                        deps.push(prod.clone());
                    } else {
                        let fk = file_key(inp);
                        if self.leaf_inputs.insert(fk.clone()) {
                            self.engine
                                .add_input(&fk, digest_of(&self.exec_root.join(inp)));
                        }
                        deps.push(fk);
                    }
                }

                // Each action gets a PERSISTENT sandbox, reused across rebuilds —
                // only the changed input links are fixed up (Bazel's stash trick),
                // and a content-only change is zero link churn.
                let sb_dir = self
                    .exec_root
                    .join(".razel-sandbox")
                    .join(akey.replace([':', '#'], "_"));
                let sandbox = Rc::new(RefCell::new(
                    Sandbox::persistent(sb_dir, self.materialize).map_err(|e| e.to_string())?,
                ));

                // The action's compute: restore-or-run, value = digest of its outputs.
                let argv = act.argv.clone();
                let inputs = act.inputs.clone();
                let outputs = act.outputs.clone();
                let cache = self.cache.clone();
                let exec_root = self.exec_root.clone();
                let errors = self.errors.clone();
                let dep_refs: Vec<&str> = deps.iter().map(String::as_str).collect();
                self.engine.add_derived(&akey, &dep_refs, move |_| {
                    run_action(
                        &argv, &inputs, &outputs, &cache, &exec_root, &errors, &sandbox,
                    )
                });
                act_keys.push(akey);
            }
            let tkey = target_key(&t.name);
            let act_refs: Vec<&str> = act_keys.iter().map(String::as_str).collect();
            self.engine.add_derived(&tkey, &act_refs, combine);
        }
        Ok(())
    }

    /// An edited file changed on disk: re-digest it and feed the engine. The next
    /// `build` recomputes only what transitively depends on it.
    pub fn sync_file(&self, path: &str) {
        let key = file_key(path);
        if self.leaf_inputs.contains(&key) {
            self.engine
                .set_input(&key, digest_of(&self.exec_root.join(path)));
        }
    }

    /// Build `target`; returns how many engine nodes recomputed (the O(affected)
    /// metric). Errors from any action surface here.
    pub fn build(&self, target: &str) -> Result<usize, String> {
        self.errors.borrow_mut().clear();
        self.engine.reset_recomputes();
        self.engine.request(&target_key(target))?;
        let errs = self.errors.borrow();
        if !errs.is_empty() {
            return Err(errs.join("; "));
        }
        Ok(self.engine.recomputes())
    }
}

/// Run one action (cache restore-or-run) in its persistent `sandbox` and return
/// the digest of its outputs. Side-effecting; failures are pushed to `errors`
/// and a sentinel digest returned.
#[allow(clippy::too_many_arguments)]
fn run_action(
    argv: &[String],
    inputs: &[String],
    outputs: &[String],
    cache: &Cache,
    exec_root: &std::path::Path,
    errors: &RefCell<Vec<String>>,
    sandbox: &Rc<RefCell<Sandbox>>,
) -> Digest {
    let mut input_digests = BTreeMap::new();
    for inp in inputs {
        if let Ok(bytes) = std::fs::read(exec_root.join(inp)) {
            input_digests.insert(inp.clone(), Digest::of(&bytes));
        }
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
    match build_action_in(&action, cache, exec_root, &mut sb) {
        Ok(r) if r.exit_code == 0 => {
            // Value = digest over the produced outputs, so early-cutoff tracks them.
            let parts: Vec<Digest> = outputs
                .iter()
                .map(|o| digest_of(&exec_root.join(o)))
                .collect();
            combine(&parts)
        }
        Ok(r) => {
            errors
                .borrow_mut()
                .push(format!("action failed ({}): {argv:?}", r.exit_code));
            Digest::of(b"<error>")
        }
        Err(e) => {
            errors.borrow_mut().push(e.to_string());
            Digest::of(b"<error>")
        }
    }
}

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
}
