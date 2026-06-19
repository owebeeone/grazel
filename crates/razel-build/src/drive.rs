//! razel-build `drive` — split from `lib.rs`.

use super::*;

/// Build `target` from a **real Bazel `BUILD`** (loads cc rules from `@rules_cc`,
/// resolved to razel's native rules). Analysis + execution; single-package.
pub fn build_bazel(
    build_src: &str,
    target: &str,
    exec_root: &Path,
    cache: &Cache,
) -> Result<BuildReport, String> {
    build_bazel_with(build_src, target, exec_root, cache, GlobalFlags::default())
}


/// [`build_bazel`] with build-wide [`GlobalFlags`] (the CLI's `-c`/`--copt`/`--linkopt`/…)
/// applied to every cc action.
pub fn build_bazel_with(
    build_src: &str,
    target: &str,
    exec_root: &Path,
    cache: &Cache,
    flags: GlobalFlags,
) -> Result<BuildReport, String> {
    let jobs = flags.jobs;
    execute_jobs(
        &analyze_bazel_with(build_src, flags)?,
        target,
        exec_root,
        cache,
        jobs,
    )
}


/// Build `top_label` (`//pkg:name`) from a **multi-package Bazel workspace** rooted
/// at `root`, loading dependency packages on demand. exec_root = the workspace root
/// (paths are package-qualified, matching Bazel's workspace-relative includes).
pub fn build_workspace(root: &Path, top_label: &str, cache: &Cache) -> Result<BuildReport, String> {
    build_workspace_with(root, top_label, cache, GlobalFlags::default())
}


/// [`build_workspace`] with build-wide [`GlobalFlags`] applied to every cc action.
pub fn build_workspace_with(
    root: &Path,
    top_label: &str,
    cache: &Cache,
    flags: GlobalFlags,
) -> Result<BuildReport, String> {
    let jobs = flags.jobs;
    let prof = std::env::var_os("RAZEL_PROFILE").is_some();
    let ta = std::time::Instant::now();
    // B4: an external-repo top label is an ALIAS (`@crates//:blake3`) whose analysis keys the target
    // under its RESOLVED canonical name (`@@rules_rust++crate+crates__blake3-1.8.2//:blake3`). Start
    // execution from that resolved name — `collect_order` would otherwise not find the apparent alias.
    let (targets, resolved) = analyze_workspace_resolved(root, top_label, flags)?;
    if prof {
        eprintln!("PROFILE analyze: {:.3}s ({} targets)", ta.elapsed().as_secs_f64(), targets.len());
    }
    let tp = std::time::Instant::now();
    // B4: an external (`@crates`) build executes in a PROPER exec-root forest (workspace sources +
    // `external/<repo>` → the fetched repos) so external crate sources resolve at the declared path and
    // outputs land in `bazel-out/`, never the source cache. A pure-local build (no `.razel-crates`
    // materialized) keeps `exec_root = root` — unchanged (A7, the corpus cases).
    let exec_root_buf;
    let exec_root: &Path = if root.join(".razel-crates").is_dir() {
        exec_root_buf = prepare_exec_root(root).map_err(|e| format!("prepare exec root: {e}"))?;
        exec_root_buf.as_path()
    } else {
        root
    };
    if prof {
        eprintln!("PROFILE prepare_exec_root: {:.3}s", tp.elapsed().as_secs_f64());
    }
    let te = std::time::Instant::now();
    let r = execute_jobs(&targets, &resolved, exec_root, cache, jobs);
    if prof {
        eprintln!("PROFILE execute_jobs: {:.3}s", te.elapsed().as_secs_f64());
    }
    r
}


/// Post-order DFS over `deps` → targets ordered deps-first (a target's deps execute before it).
pub(crate) fn collect_order(
    name: &str,
    by_name: &HashMap<String, AnalyzedTarget>,
    order: &mut Vec<String>,
    seen: &mut HashSet<String>,
) -> Result<(), String> {
    if !seen.insert(name.to_string()) {
        return Ok(());
    }
    let t = by_name
        .get(name)
        .ok_or_else(|| format!("unknown target: {name}"))?;
    for d in &t.deps {
        collect_order(d, by_name, order, seen)?;
    }
    order.push(name.to_string());
    Ok(())
}


/// The outcome of a build: the produced output paths (in order) and how many
/// actions actually **executed** (cache misses). `executed == 0` means the whole
/// target was served from cache — the incremental "nothing to do" signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildReport {
    pub produced: Vec<String>,
    pub executed: usize,
    /// The REQUESTED target's DefaultInfo (bazel semantics: the build's outputs —
    /// what `run` execs, what clients see; `produced` keeps every intermediate).
    pub default_outputs: Vec<String>,
}


/// Build a target and its transitive deps: analyze, order deps-first, and execute every
/// action in `exec_root` (cache hit → 0 exec). Returns the produced output paths in order.
pub fn build_target(
    build_src: &str,
    target: &str,
    exec_root: &Path,
    cache: &Cache,
) -> Result<Vec<String>, String> {
    Ok(build_target_report(build_src, target, exec_root, cache)?.produced)
}


/// Analysis only: run the BUILD's rules and capture each target's actions. Split out
/// from execution so a warm daemon can cache it and skip re-parsing an unchanged BUILD.
pub fn analyze_build(build_src: &str) -> Result<Vec<AnalyzedTarget>, String> {
    analyze_starlark("BUILD", build_src)
}


/// Like [`build_target`] but also reports how many actions executed (cache misses) —
/// the basis for `Cached` vs `Built` status and the `recomputes` metric.
pub fn build_target_report(
    build_src: &str,
    target: &str,
    exec_root: &Path,
    cache: &Cache,
) -> Result<BuildReport, String> {
    execute(&analyze_build(build_src)?, target, exec_root, cache)
}


/// Execute a pre-analyzed target graph: order deps-first and run every action in
/// `exec_root` (cache hit → 0 exec). Serial — equivalent to [`execute_jobs`] with `jobs=1`.
/// Separated from [`analyze_build`] so callers (the daemon) can reuse warm analysis.
pub fn execute(
    targets: &[AnalyzedTarget],
    target: &str,
    exec_root: &Path,
    cache: &Cache,
) -> Result<BuildReport, String> {
    execute_jobs(targets, target, exec_root, cache, 1)
}


/// [`execute`] with up to `jobs` targets running CONCURRENTLY (S5x). A Kahn ready-queue over
/// the dep DAG: a target runs only once all its deps complete, so the shared `exec_root` sees
/// the same writes-before-reads a serial build would; independent targets (same topo layer)
/// run in parallel WITHOUT a barrier (a finished target unblocks its dependents immediately —
/// no waiting on a slow sibling). `jobs<=1` is the plain serial walk. The report is
/// canonicalised to the topo order, so `-j1` and `-jN` are byte-identical (the determinism
/// bar); the per-action outputs are content-addressed, hence identical regardless of order.
pub fn execute_jobs(
    targets: &[AnalyzedTarget],
    target: &str,
    exec_root: &Path,
    cache: &Cache,
    jobs: usize,
) -> Result<BuildReport, String> {
    let by_name: HashMap<String, AnalyzedTarget> = targets
        .iter()
        .map(|t| (t.name.clone(), t.clone()))
        .collect();

    let mut order = Vec::new();
    collect_order(target, &by_name, &mut order, &mut HashSet::new())?;
    let n = order.len();
    // Bazel semantics: a build's OUTPUTS are the requested target's DefaultInfo, not every
    // intermediate (post-order ⇒ the requested target is last in `order`).
    let default_outputs = order
        .last()
        .and_then(|t| by_name.get(t))
        .map(|t| t.default_info.clone())
        .unwrap_or_default();

    if jobs <= 1 {
        let mut produced = Vec::new();
        let mut executed = 0;
        for tname in &order {
            let (e, p) = run_one_target(&by_name[tname], exec_root, cache)?;
            executed += e;
            produced.extend(p);
        }
        return Ok(BuildReport { produced, executed, default_outputs });
    }

    // Parallel. indegree[i] = unfinished in-graph deps of `order[i]`; rdeps[j] = the targets
    // that depend on j (the edges we relax when j completes).
    let pos: HashMap<&str, usize> =
        order.iter().enumerate().map(|(i, t)| (t.as_str(), i)).collect();
    let mut indeg = vec![0usize; n];
    let mut rdeps: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, tname) in order.iter().enumerate() {
        for d in &by_name[tname].deps {
            if let Some(&j) = pos.get(d.as_str()) {
                indeg[i] += 1;
                rdeps[j].push(i);
            }
        }
    }
    // Per-target result slots (each written by exactly one worker → no contention).
    let slots: Vec<Mutex<Option<(usize, Vec<String>)>>> =
        (0..n).map(|_| Mutex::new(None)).collect();

    // All scheduling state under ONE lock so readiness/active/completed stay consistent
    // (no cross-atomic races); `active` = targets currently running, used to detect a stall
    // (ready-empty + active==0 + work-remaining ⇒ a dependency cycle).
    struct Sched {
        ready: VecDeque<usize>,
        indeg: Vec<usize>,
        active: usize,
        completed: usize,
        err: Option<String>,
    }
    let sched = Mutex::new(Sched {
        ready: (0..n).filter(|&i| indeg[i] == 0).collect(),
        indeg,
        active: 0,
        completed: 0,
        err: None,
    });
    let cv = Condvar::new();

    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| {
                loop {
                    let i = {
                        let mut s = sched.lock().expect("sched");
                        loop {
                            if s.err.is_some() || s.completed == n {
                                return;
                            }
                            if let Some(i) = s.ready.pop_front() {
                                s.active += 1;
                                break i;
                            }
                            if s.active == 0 {
                                // Nothing ready, nothing running, work remains → cycle.
                                s.err = Some("dependency cycle in parallel execute".into());
                                cv.notify_all();
                                return;
                            }
                            s = cv.wait(s).expect("sched wait");
                        }
                    };
                    let res = run_one_target(&by_name[&order[i]], exec_root, cache);
                    let mut s = sched.lock().expect("sched");
                    match res {
                        Ok(r) => *slots[i].lock().expect("slot") = Some(r),
                        Err(e) => {
                            s.err = Some(e);
                            cv.notify_all();
                            return;
                        }
                    }
                    for &j in &rdeps[i] {
                        s.indeg[j] -= 1;
                        if s.indeg[j] == 0 {
                            s.ready.push_back(j);
                        }
                    }
                    s.active -= 1;
                    s.completed += 1;
                    cv.notify_all();
                }
            });
        }
    });

    let sched = sched.into_inner().expect("sched");
    if let Some(e) = sched.err {
        return Err(e);
    }
    // Flatten results in topo order → produced/executed independent of completion order.
    let mut produced = Vec::new();
    let mut executed = 0;
    for slot in &slots {
        let (e, p) = slot.lock().expect("slot").take().expect("every target ran");
        executed += e;
        produced.extend(p);
    }
    Ok(BuildReport { produced, executed, default_outputs })
}


