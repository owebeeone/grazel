//! The warm-build actor (FixMissingServerImplementationPlan §3.5a, WS-D).
//!
//! The engine graph ([`razel_build::IncrementalBuilder`]) is `!Send` (single-threaded `Rc`/
//! `RefCell`), so it lives on ONE owner thread for the daemon's life — never moved across a
//! boundary. The RPC plane talks to it only through an [`ActorHandle`]: a `build` request is
//! enqueued and the caller blocks on a one-shot reply.
//!
//! This is what makes the daemon WARM. The cold path (`build_workspace_with` → `run_one_target`)
//! re-`digest_path`s every declared action input on every invocation — the ~34s no-op. The actor
//! holds the warm engine: an unchanged input's digest is never re-read (it lives in the engine's
//! named `DepValue`s), so a no-op rebuild recomputes **zero** nodes, and a source edit recomputes
//! only the affected action subgraph.
//!
//! Invalidation is bazel cancel-and-restart (R-9.5): a watcher event pushes the changed path onto
//! a shared `pending` queue and flips the `cancel` flag; an in-flight build aborts at the next
//! action boundary; the actor folds the queued change and re-builds. Builds (which carry a reply)
//! travel a SEPARATE channel from invalidations, so a build is never dropped by a drain.

use razel_build::args::parse_opts_with_rc;
use razel_build::{
    AnalyzedTarget, GlobalFlags, IncrementalBuilder, analyze_workspace_resolved, prepare_exec_root,
};
use razel_core::Digest;
use razel_exec::Cache;
use razel_wire::{BuildResult, BuildState, BuildStatus, OutputArtifact, TargetKind, TargetStatus};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

/// A request to the actor. `build` carries the raw client arg tokens + cwd (C3 — parsed
/// server-side by the one shared parser) and a one-shot reply channel.
pub enum ActorMessage {
    Build {
        args: Vec<String>,
        cwd: PathBuf,
        reply: Sender<Result<BuildResult, String>>,
        /// When set, per-action progress lines (`"<mnemonic> <output>"`) are forwarded here as the
        /// build executes — the streaming build path (WS-E.2). `None` for a unary build.
        progress: Option<Sender<String>>,
    },
    Shutdown,
}

/// The RPC plane's handle to the actor: enqueue builds, feed watcher invalidations, read the
/// analysis counter. Cheap to clone the shared bits (the watcher thread holds a clone).
#[derive(Clone)]
pub struct ActorHandle {
    inbox: Sender<ActorMessage>,
    /// Flipped by a watcher event to abort an in-flight build (cancel-and-restart).
    cancel: Arc<AtomicBool>,
    /// Changed paths awaiting fold-in; drained at the top of each build attempt.
    pending: Arc<Mutex<Vec<PathBuf>>>,
    /// The live set of generated output paths (workspace-relative). The watcher ignores events on
    /// these so the daemon never cancels on its own writes.
    outputs: Arc<Mutex<HashSet<String>>>,
    /// How many times analysis actually ran (the warm-reuse signal; flat across no-op rebuilds).
    analyses: Arc<AtomicUsize>,
    /// The executor concurrency the LAST build resolved (auto = machine cores, or the request's
    /// `-j N`). Re-resolved per request from that request's args, so an override never persists.
    jobs: Arc<AtomicUsize>,
}

impl ActorHandle {
    /// Enqueue a build and block on its result (the RPC connection thread waits here). Unary — no
    /// progress stream (used by `do_build`/`do_run` + grazel).
    pub fn build(&self, args: Vec<String>, cwd: PathBuf) -> Result<BuildResult, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.inbox
            .send(ActorMessage::Build {
                args,
                cwd,
                reply: tx,
                progress: None,
            })
            .map_err(|_| "razel daemon: build actor stopped".to_string())?;
        rx.recv()
            .map_err(|_| "razel daemon: build actor died mid-build".to_string())?
    }

    /// Enqueue a STREAMING build: returns a progress receiver (one `"<mnemonic> <output>"` line per
    /// executed action, closes when the build finishes) and a one-shot result receiver. Non-blocking
    /// — the caller drains progress, then reads the result (WS-E.2). If the actor is gone, both
    /// receivers close, so the caller never hangs.
    #[allow(clippy::type_complexity)]
    pub fn build_streaming(
        &self,
        args: Vec<String>,
        cwd: PathBuf,
    ) -> (Receiver<String>, Receiver<Result<BuildResult, String>>) {
        let (ptx, prx) = std::sync::mpsc::channel::<String>();
        let (rtx, rrx) = std::sync::mpsc::channel::<Result<BuildResult, String>>();
        let _ = self.inbox.send(ActorMessage::Build {
            args,
            cwd,
            reply: rtx,
            progress: Some(ptx),
        });
        (prx, rrx)
    }

    /// Times analysis ran. Stays flat across rebuilds of an unchanged BUILD (warm-reuse).
    pub fn analyses_run(&self) -> usize {
        self.analyses.load(Ordering::SeqCst)
    }

    /// The executor concurrency the last build resolved — machine cores by default, or that
    /// request's `-j N`. Re-resolved per request, so an override applies only to its own build.
    pub fn last_jobs(&self) -> usize {
        self.jobs.load(Ordering::SeqCst)
    }

    /// Watcher hook: record a changed path and cancel any in-flight build so it restarts folding
    /// the change. The caller drops excluded paths (infra dirs, `.git`, …) and known outputs
    /// (via [`is_known_output`](Self::is_known_output)) BEFORE calling this, so the daemon never
    /// cancels on its own writes.
    pub fn notify_change(&self, path: PathBuf) {
        self.pending.lock().unwrap().push(path);
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Is `rel` (workspace-relative) a generated output of the current graph? The watcher uses this
    /// to drop the daemon's own output writes (else they self-cancel the producing build).
    pub fn is_known_output(&self, rel: &str) -> bool {
        self.outputs.lock().unwrap().contains(rel)
    }

    /// Stop the actor thread (best-effort).
    pub fn shutdown(&self) {
        let _ = self.inbox.send(ActorMessage::Shutdown);
    }
}

/// Cached analysis for the current `(graph-shape digest, requested token)`.
struct Analysis {
    digest: Digest,
    token: String,
    targets: Vec<AnalyzedTarget>,
    build_name: String,
}

/// The single-owner warm-build actor. Constructed and run entirely on its own thread (it holds the
/// `!Send` builder), so it is NEVER moved across a thread boundary — [`spawn`](Self::spawn) only
/// moves `Send` handles into the closure and builds this in place.
pub struct WorkspaceActor {
    inbox: Receiver<ActorMessage>,
    workspace: PathBuf,
    cache_dir: PathBuf,
    /// The exec root the warm graph builds over: a persistent `.razel-exec` forest for external-
    /// crate builds, else the workspace itself.
    exec_root: PathBuf,
    /// The warm engine graph; built on first analysis, reused across builds.
    builder: Option<IncrementalBuilder>,
    analysis: Option<Analysis>,
    cancel: Arc<AtomicBool>,
    pending: Arc<Mutex<Vec<PathBuf>>>,
    outputs: Arc<Mutex<HashSet<String>>>,
    analyses: Arc<AtomicUsize>,
    jobs: Arc<AtomicUsize>,
    state: Arc<Mutex<BuildState>>,
    bump: Arc<Condvar>,
}

impl WorkspaceActor {
    /// Spawn the actor on its own thread; return the handle the RPC plane and watcher use. The
    /// `!Send` builder is created INSIDE the thread (never crosses the boundary).
    pub fn spawn(
        workspace: PathBuf,
        cache_dir: PathBuf,
        state: Arc<Mutex<BuildState>>,
        bump: Arc<Condvar>,
    ) -> ActorHandle {
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let pending = Arc::new(Mutex::new(Vec::new()));
        let outputs = Arc::new(Mutex::new(HashSet::new()));
        let analyses = Arc::new(AtomicUsize::new(0));
        let jobs = Arc::new(AtomicUsize::new(0));
        let handle = ActorHandle {
            inbox: tx,
            cancel: cancel.clone(),
            pending: pending.clone(),
            outputs: outputs.clone(),
            analyses: analyses.clone(),
            jobs: jobs.clone(),
        };
        std::thread::Builder::new()
            .name("razel-actor".into())
            .spawn(move || {
                let mut actor = WorkspaceActor {
                    inbox: rx,
                    workspace: workspace.clone(),
                    cache_dir,
                    exec_root: workspace,
                    builder: None,
                    analysis: None,
                    cancel,
                    pending,
                    outputs,
                    analyses,
                    jobs,
                    state,
                    bump,
                };
                actor.run();
            })
            .expect("spawn razel-actor thread");
        handle
    }

    fn run(&mut self) {
        while let Ok(msg) = self.inbox.recv() {
            match msg {
                ActorMessage::Build {
                    args,
                    cwd,
                    reply,
                    progress,
                } => {
                    let r = self.handle_build(&args, &cwd, progress.as_ref());
                    // Close the progress stream BEFORE replying, so a streaming client drains every
                    // progress frame ahead of the terminal result (the connection thread reads
                    // progress until close, then the result).
                    drop(progress);
                    let _ = reply.send(r); // a gone client just drops the reply
                }
                ActorMessage::Shutdown => break,
            }
        }
    }

    /// One client build, with bazel cancel-and-restart: if a watcher event raced the build,
    /// discard the result and rebuild so the change is folded — a stale snapshot is never committed.
    ///
    /// The restart gate must read the SAME state the watcher writes FIRST. `notify_change` pushes
    /// the path onto `pending` (under its Mutex) and only THEN sets `cancel`. So checking `cancel`
    /// alone has a hole: an edit whose `push` landed after this build's drain but whose `cancel`
    /// store lands after our `cancel.load` would be stranded in `pending` and never folded — a
    /// stale commit (the exact R-9.5 failure). Checking `pending` under its lock closes the window:
    /// any edit that became knowable (pushed) before we finalize forces a restart; an edit pushed
    /// strictly after this check is genuinely post-build and folds on the next request.
    fn handle_build(
        &mut self,
        args: &[String],
        cwd: &Path,
        progress: Option<&Sender<String>>,
    ) -> Result<BuildResult, String> {
        let result = loop {
            self.cancel.store(false, Ordering::SeqCst);
            let r = self.build_once(args, cwd, progress);
            let raced = matches!(&r, Err(e) if e == "cancelled")
                || self.cancel.load(Ordering::SeqCst)
                || !self.pending.lock().unwrap().is_empty();
            if raced {
                continue;
            }
            break r;
        };
        if let Ok(ref r) = result {
            self.commit_snapshot(r);
        }
        result
    }

    /// A single warm build attempt. Folds pending edits, (re)analyzes only on a graph-shape/target
    /// change, then drives the warm engine.
    fn build_once(
        &mut self,
        args: &[String],
        cwd: &Path,
        progress: Option<&Sender<String>>,
    ) -> Result<BuildResult, String> {
        // cwd rides the wire (C3) for future per-package target resolution; the root-workspace
        // daemon resolves against `self.workspace`.
        let _ = cwd;
        self.apply_pending_changes();

        // rc-lite: read `.bazelrc`/`.razelrc` build/common flags server-side, the SAME way the CLI
        // does (parity — a daemon build must use the same compiler flags as a local build).
        let opts = parse_opts_with_rc(&["common", "build"], args)?;
        let flags = opts.global_flags();
        // Resolve + record this request's executor concurrency: machine cores by default (bazel
        // `auto`), or the request's `-j N`. Re-resolved every build from THIS request's args, so an
        // override applies only to its own build — never sticky on the warm daemon. (The warm engine
        // path is serial today; this drives the cold/`--batch` executor and is the per-request
        // contract for when warm-parallel execution lands.)
        self.jobs.store(
            razel_build::effective_jobs(flags.jobs),
            Ordering::SeqCst,
        );
        let token = opts
            .positionals
            .last()
            .cloned()
            .ok_or_else(|| "build: no target specified".to_string())?;

        let build_name = self.ensure_analysis(&token, &flags)?;
        let builder = self.builder.as_ref().expect("analysis populated the builder");
        // Forward per-action progress to the streaming client (if any) for THIS build. Set after
        // ensure_analysis (which may have built a fresh builder); always cleared before returning.
        if let Some(tx) = progress {
            let tx = tx.clone();
            builder.set_progress(Some(Box::new(move |line: &str| {
                let _ = tx.send(line.to_string());
            })));
        }
        let build_res = builder.build(&build_name); // drives the warm engine; may Err("cancelled")
        builder.set_progress(None);
        build_res?;
        // Built-vs-Cached uses ACTIONS EXECUTED (cache misses) — the cold path's `report.executed`
        // — NOT the engine recompute count (which also counts aggregation nodes + cache HITs).
        let executed = builder.executed_actions();
        let default_info = self.default_info_for(&build_name);
        let outputs = builder
            .produced_outputs(&default_info)?
            .into_iter()
            .map(|(path, d)| OutputArtifact {
                path,
                digest: d.as_bytes().to_vec(),
            })
            .collect();

        Ok(BuildResult {
            target: token,
            status: if executed == 0 {
                BuildStatus::Cached
            } else {
                BuildStatus::Built
            },
            recomputes: executed as i64,
            outputs,
            message: None,
        })
    }

    /// Reuse the warm builder when the graph-shape digest AND the requested token are unchanged;
    /// otherwise (re)analyze and build a fresh warm graph. Returns the engine target name to build.
    fn ensure_analysis(&mut self, token: &str, flags: &GlobalFlags) -> Result<String, String> {
        let digest = self.compute_analysis_digest(flags);
        let hit = self
            .analysis
            .as_ref()
            .is_some_and(|a| a.digest == digest && a.token == token);
        if hit && self.builder.is_some() {
            return Ok(self.analysis.as_ref().unwrap().build_name.clone());
        }

        // Re-analyze through the workspace loader for BOTH bare names and //-labels, so
        // cross-package aliases resolve (e.g. `//:razel` → `//crates/razel-cli:razel`) and dep
        // packages load — matching the CLI's local build_one + bazel. A bare token is the root
        // package's same-named target (`razel` → `//:razel`); the single-BUILD `analyze_build`
        // path could not follow aliases and built an action-less stub (the WS-D-review #6 bug).
        let label = if token.starts_with("//") {
            token.to_string()
        } else {
            format!("//:{token}")
        };
        let (targets, build_name) = analyze_workspace_resolved(&self.workspace, &label, flags.clone())?;

        // Persistent exec-root forest for external-crate builds; else build in the workspace.
        self.exec_root = if self.workspace.join(".razel-crates").is_dir() {
            prepare_exec_root(&self.workspace).map_err(|e| format!("prepare exec root: {e}"))?
        } else {
            self.workspace.clone()
        };

        let cache = Cache::new(&self.cache_dir).map_err(|e| e.to_string())?;
        let mut builder = IncrementalBuilder::new(self.exec_root.clone(), cache);
        builder.engine_set_cancel(Some(self.cancel.clone()));
        builder.configure_targets(targets.clone())?;
        // Publish the output set BEFORE any build writes them, so the watcher drops the daemon's
        // own output events (no self-cancellation).
        *self.outputs.lock().unwrap() = builder.output_paths();
        self.builder = Some(builder);
        self.analysis = Some(Analysis {
            digest,
            token: token.to_string(),
            targets,
            build_name: build_name.clone(),
        });
        self.analyses.fetch_add(1, Ordering::SeqCst);
        Ok(build_name)
    }

    /// The requested target's `default_info` paths (the build's outputs, bazel semantics).
    fn default_info_for(&self, build_name: &str) -> Vec<String> {
        self.analysis
            .as_ref()
            .and_then(|a| a.targets.iter().find(|t| t.name == build_name))
            .map(|t| t.default_info.clone())
            .unwrap_or_default()
    }

    /// Fold queued watcher edits into the warm graph: a known leaf is re-digested in place
    /// (`sync_file`); a new/deleted source or a BUILD/MODULE edit forces re-analysis next build.
    fn apply_pending_changes(&mut self) {
        let changes: Vec<PathBuf> = std::mem::take(&mut *self.pending.lock().unwrap());
        for path in changes {
            let rel = path.strip_prefix(&self.workspace).unwrap_or(&path);
            let rel = rel.to_string_lossy().to_string();
            let is_leaf = self
                .builder
                .as_ref()
                .map(|b| b.knows_leaf(&rel))
                .unwrap_or(false);
            if is_leaf {
                self.builder.as_ref().unwrap().sync_file(&rel);
            } else {
                // Unknown to the current graph (new file, glob change, BUILD/MODULE edit) →
                // invalidate analysis; the next build re-analyzes (the digest also catches
                // content edits to the graph-shape files).
                self.analysis = None;
            }
        }
    }

    /// Content digest over the graph-shape files plus the semantic flags. Unchanged ⇒ analysis is
    /// reused; ANY graph-topology change ⇒ re-analysis. Covers EVERY package's `BUILD`/`BUILD.bazel`
    /// (not just the root), so a sub-package BUILD edit invalidates the cache even when the file
    /// watcher is off (the grazel host path / a failed watcher) — the §3.6b digest-is-the-backstop
    /// invariant. (Glob membership of NEW source files is still watcher-driven; a residual gap on
    /// the watcher-off path.)
    fn compute_analysis_digest(&self, flags: &GlobalFlags) -> Digest {
        let mut buf = Vec::new();
        for f in ["MODULE.bazel", "MODULE.bazel.lock", ".bazelrc", ".razelrc"] {
            if let Ok(bytes) = std::fs::read(self.workspace.join(f)) {
                buf.extend_from_slice(f.as_bytes());
                buf.push(0);
                buf.extend_from_slice(&bytes);
                buf.push(0);
            }
        }
        // Every BUILD/BUILD.bazel in the source tree, canonical sorted order.
        let mut build_files = Vec::new();
        collect_build_files(&self.workspace, &self.workspace, &mut build_files);
        build_files.sort();
        for rel in &build_files {
            if let Ok(bytes) = std::fs::read(self.workspace.join(rel)) {
                buf.extend_from_slice(rel.as_bytes());
                buf.push(0);
                buf.extend_from_slice(&bytes);
                buf.push(0);
            }
        }
        // Fingerprint the SEMANTIC flags (copts/linkopt/compilation_mode/defines/…) — but NOT
        // `jobs`, which is execution-phase concurrency: changing `-j` between requests must reuse
        // the warm analysis, not re-run it.
        let mut fp = flags.clone();
        fp.jobs = 0;
        buf.extend_from_slice(format!("flags:{fp:?}").as_bytes());
        Digest::of(&buf)
    }

    /// Fold a completed build into the live `BuildState` and wake `build.subscribe` subscribers.
    /// Runs on the actor thread — revision bump + publish are one critical section (no torn frame).
    fn commit_snapshot(&self, result: &BuildResult) {
        let name = result.target.rsplit(':').next().unwrap_or(&result.target);
        let kind = if name.ends_with("_test") {
            TargetKind::Test
        } else if name.ends_with("_binary") {
            TargetKind::Binary
        } else {
            TargetKind::Library
        };
        let ts = TargetStatus {
            label: result.target.clone(),
            kind,
            status: result.status,
            output_digest: result
                .outputs
                .first()
                .map(|o| o.digest.clone())
                .unwrap_or_default(),
        };
        let mut st = self.state.lock().unwrap();
        st.targets.retain(|t| t.label != ts.label);
        st.targets.push(ts);
        st.targets.sort_by(|a, b| a.label.cmp(&b.label));
        st.revision += 1;
        drop(st);
        self.bump.notify_all();
    }
}

/// Recursively collect workspace-relative paths of every `BUILD`/`BUILD.bazel`, skipping the same
/// infra dirs `prepare_exec_root` excludes (`.razel-*`, `.git*`, output trees, `target`). Used by
/// the analysis digest so a sub-package BUILD edit invalidates the warm graph.
fn collect_build_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let n = name.to_string_lossy();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            if n.starts_with(".razel-")
                || n.starts_with(".git")
                || matches!(
                    n.as_ref(),
                    "target" | "bazel-out" | "razel-out" | "razel-bin" | "razel-testlogs"
                )
            {
                continue;
            }
            collect_build_files(root, &entry.path(), out);
        } else if matches!(n.as_ref(), "BUILD" | "BUILD.bazel")
            && let Ok(rel) = entry.path().strip_prefix(root)
        {
            out.push(rel.to_string_lossy().to_string());
        }
    }
}
