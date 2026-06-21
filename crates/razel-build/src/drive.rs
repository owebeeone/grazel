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
/// Canonicalize a user's build-target token to a workspace label — the ONE place the CLI and the
/// daemon agree on. They used to inline this separately and drifted: the daemon turned the relative
/// `:name` into `//::name` (double colon) while the CLI got it right. Bazel forms: an absolute
/// `//pkg:name` or an external `@repo//:name` passes through; a bare `name` OR a relative `:name`
/// both name the ROOT package's `name` target → `//:name` (the leading `:` is consumed, never
/// doubled). `rsplit(':')` takes the name after the last colon, so `:name`, `name`, and even
/// `pkg:name`-without-`//` all reduce to the root package's `name`.
pub fn canonical_target(token: &str) -> String {
    if token.starts_with("//") || (token.starts_with('@') && token.contains("//")) {
        token.to_string()
    } else {
        let name = token.rsplit(':').next().unwrap_or(token);
        format!("//:{name}")
    }
}

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


/// Resolve executor concurrency: an explicit `--jobs N` wins; `0` (unset) means "auto" — the
/// machine's logical CPU count (bazel's `--jobs=auto`), so a build saturates the host by default
/// instead of running on a single core. The single source of truth for the default, shared by the
/// CLI's in-process path ([`execute_jobs`]) and the daemon actor (which records it per request).
pub fn effective_jobs(requested: usize) -> usize {
    if requested == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    } else {
        requested
    }
}


/// Execute a pre-analyzed target graph through the **one** engine executor, with up to `jobs`
/// actions running CONCURRENTLY (`jobs<=1` ⇒ serial). The cold/`--batch`/local build and the warm
/// daemon now share this single path: a fresh [`IncrementalBuilder`] per call is the cold build; the
/// daemon keeps the same builder warm. Same shared `restore_or_run` execution core as before — so
/// outputs are byte-identical — but driven by the engine's parallel evaluator (named-`DepValue`
/// input digests, NO per-input re-hash) instead of the old per-target Kahn walk + `run_one_target`.
/// `produced`/`default_outputs` are pure functions of the analysis (unchanged); `executed` is the
/// engine's cache-miss count. The result is order-independent (content-addressed outputs).
pub fn execute_jobs(
    targets: &[AnalyzedTarget],
    target: &str,
    exec_root: &Path,
    cache: &Cache,
    jobs: usize,
) -> Result<BuildReport, String> {
    // `0` ⇒ auto = the machine's logical cores (bazel parity); an explicit `-j N` is honored as-is.
    let jobs = effective_jobs(jobs);
    let by_name: HashMap<String, AnalyzedTarget> = targets
        .iter()
        .map(|t| (t.name.clone(), t.clone()))
        .collect();

    // The demanded closure, deps-first (post-order ⇒ the requested target is last).
    let mut order = Vec::new();
    collect_order(target, &by_name, &mut order, &mut HashSet::new())?;
    // Bazel semantics: a build's OUTPUTS are the requested target's DefaultInfo, not every
    // intermediate; `produced` keeps every intermediate output of the closure (topo order).
    let default_outputs = order
        .last()
        .and_then(|t| by_name.get(t))
        .map(|t| t.default_info.clone())
        .unwrap_or_default();
    let produced: Vec<String> = order
        .iter()
        .flat_map(|t| by_name[t].actions.iter().flat_map(|a| a.outputs.clone()))
        .collect();

    // Run through the engine. A fresh builder = the cold build; `request_parallel` runs only the
    // demanded closure's actions (reachable from `target`), exactly the set `order` covers.
    let mut builder = IncrementalBuilder::new(exec_root, cache.clone());
    builder.configure_targets(targets.to_vec())?;
    // Cold/--batch path: no streaming client, so surface action console output (compiler WARNINGS)
    // straight to stderr — bazel's "INFO: From <action>: …". The `S`/`F`/`T` progress tags drive the
    // daemon's live bar; ignore them here. (Build FAILURES carry their output in the error already.)
    builder.set_progress(Some(Box::new(|line: &str| {
        if let Some(("L", rest)) = line.split_once('\x1f') {
            let (desc, output) = rest.split_once('\x1f').unwrap_or(("", rest));
            eprintln!("INFO: From {desc}:\n{output}");
        }
    })));
    builder.build(target, jobs)?;
    let executed = builder.executed_actions();

    Ok(BuildReport { produced, executed, default_outputs })
}

#[cfg(test)]
mod canonical_target_tests {
    use super::canonical_target;

    /// The CLI and daemon used to inline this separately and drifted — `:razel` shipped as
    /// `//::razel` ("unknown node") through the daemon. Pin every target form to its label.
    #[test]
    fn canonicalizes_every_target_form() {
        assert_eq!(canonical_target(":razel"), "//:razel", "relative :name must consume the colon");
        assert_eq!(canonical_target("razel"), "//:razel", "bare name → root package");
        assert_eq!(canonical_target("//:razel"), "//:razel", "absolute root-package label passes through");
        assert_eq!(
            canonical_target("//crates/razel-cli:razel"),
            "//crates/razel-cli:razel",
            "absolute //pkg:name passes through"
        );
        assert_eq!(canonical_target("//crates/razel-cli"), "//crates/razel-cli", "//pkg passes through");
        assert_eq!(canonical_target("@crates//:blake3"), "@crates//:blake3", "external @repo//:name passes through");
    }
}


