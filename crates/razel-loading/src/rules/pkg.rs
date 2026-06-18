//! `rules::pkg` — split from `rules.rs` (facade in `mod.rs`).

use crate::state::{AnalyzedTarget, GlobalFlags, Session, canon_label, pkg_of};
use std::path::Path;
use super::*;

/// Load a package's BUILD (once) under workspace mode, evaluating it with that
/// package as context. Cross-package deps trigger further loads via `resolve_dep`.
pub(crate) fn load_package(sess: &Session, pkg: &str) -> Result<(), String> {
    load_package_mode(sess, pkg, false)
}


/// [`load_package`] for the ENTRY package: drives every declaration (tests assert on the whole
/// package); dependency loads analyze on demand only.
pub(crate) fn load_package_entry(sess: &Session, pkg: &str) -> Result<(), String> {
    load_package_mode(sess, pkg, true)
}


/// A load failure, PHASE-TAGGED (round 29): `pkg_in_error` = the failure precedes or is in
/// the DECLARE phase (missing repo/BUILD, read error, eval_module error) — Bazel's "package
/// in error", cached so consumers don't re-evaluate. Analysis-phase (drive) failures stay
/// retryable: the declarations are fine (cross_package_providers' contract).
pub(crate) struct LoadErr {
    pub(crate) msg: String,
    pub(crate) pkg_in_error: bool,
}


impl LoadErr {
    pub(crate) fn declare(msg: impl Into<String>) -> Self {
        LoadErr {
            msg: msg.into(),
            pkg_in_error: true,
        }
    }
}


pub(crate) fn load_package_mode(sess: &Session, pkg: &str, drive_all: bool) -> Result<(), String> {
    match crate::state::acquire_resource(sess, &crate::state::ResKey::Pkg(pkg.to_string())) {
        crate::state::Acquire::Ready
        | crate::state::Acquire::Reentry
        | crate::state::Acquire::CycleProceed => return Ok(()),
        // Package-in-error: the load ran once; serve the cached error (loud, no re-eval).
        crate::state::Acquire::Failed(e) => return Err(e),
        crate::state::Acquire::Own => {}
    }
    // EVERY exit after an owned begin must finish (P4a): an early `?` between begin and
    // finish leaked a dead InFlight entry — sequentially masked by re-entry semantics, under
    // the pool every later waiter parked into the 20s-timeout livelock ("deadlock" at TF
    // scale: each unvendored-repo demand cost every waiter a 20s quantum, forever).
    let res = load_package_body(sess, pkg, drive_all);
    let outcome = match &res {
        Ok(()) => crate::state::FinishOutcome::Ok,
        Err(e) if e.pkg_in_error => crate::state::FinishOutcome::FailCached(e.msg.clone()),
        // Analysis failure must not poison the loaded-set (the guard would silently no-op
        // retries and every later condition/dep would report "not declared").
        Err(_) => crate::state::FinishOutcome::FailRetry,
    };
    crate::state::finish_pkg_load(sess, pkg, outcome);
    res.map_err(|e| e.msg)
}


pub(crate) fn load_package_body(sess: &Session, pkg: &str, drive_all: bool) -> Result<(), LoadErr> {
    // Host-materialized packages (Bazel built-ins) take precedence over vendoring.
    if let Some(src) = crate::host::host_build(pkg) {
        let repo_ctx = pkg.strip_prefix('@').and_then(|rest| {
            rest.split_once("//")
                .map(|(r, sub)| (r.to_string(), sub.to_string()))
        });
        let prev = sess.set_current_pkg(Some(pkg.to_string()));
        let res = eval_build_src_in(sess, &format!("{pkg}/BUILD"), src, repo_ctx, drive_all);
        sess.set_current_pkg(prev);
        return res;
    }
    // External package (`@repo//pkg`): its BUILD lives under the vendored repo's root.
    // Failures up to the eval are PRE-EVAL — the package is in error (cacheable).
    let pkg_dir = if pkg.starts_with('@') {
        // Both `@apparent//` and the canonical `@@repo//` (§11.3) — trim all leading `@`.
        let (repo, sub) = pkg
            .trim_start_matches('@')
            .split_once("//")
            .ok_or_else(|| LoadErr::declare(format!("bad package `{pkg}`")))?;
        let repo_dir = match sess.global.external_repo_dir(repo) {
            Some(d) => d,
            None => {
                // B2 (§2.2/§5.6): materialize this `@crates` repo from the lock on FIRST demand
                // (root → inline; per-crate → `fetch_crate`), then resolve — so only the loaded
                // closure is fetched. Inert without a seeded lock / base (non-`@crates` builds).
                if let (Some(lock), Some(base)) =
                    (&sess.global.crate_lock, &sess.global.fetched_external_base)
                {
                    crate::materialize::materialize_one_repo(lock, repo, base)
                        .map_err(LoadErr::declare)?;
                }
                sess.global.external_repo_dir(repo).ok_or_else(|| {
                    LoadErr::declare(format!("external repo for `{pkg}` not vendored"))
                })?
            }
        };
        // P6.Q1.c: every external repo carries bazel's empty REPO.bazel marker (a fresh fetch AND a
        // pre-Q1.c materialization both converge here), so glob(["**"]) lists `@repo//:REPO.bazel`.
        crate::materialize::ensure_repo_marker(&repo_dir).map_err(LoadErr::declare)?;
        repo_dir.join(sub)
    } else {
        sess.workspace
            .clone()
            .ok_or_else(|| LoadErr::declare("load_package called outside workspace mode"))?
            .join(pkg)
    };
    // Resolve the package file UNCONDITIONALLY (cheap stat probes) — bazel precedence
    // must hold even when the parallel pre-pass already cached the AST.
    let build_path = crate::workspace::resolve_build_file(&pkg_dir, sess.global.strict_bazel)
        .map_err(LoadErr::declare)?
        .ok_or_else(|| {
            LoadErr::declare(format!(
                "no BUILD in package `{pkg}` ({})",
                pkg_dir.display()
            ))
        })?;
    // Pre-parsed AST present? Skip BOTH the read and the parse (the parallel pre-pass).
    let prepared = sess
        .ast_cache
        .borrow()
        .contains_key(&format!("{pkg}/BUILD"));
    let src = if prepared {
        String::new()
    } else {
        std::fs::read_to_string(&build_path).map_err(|e| LoadErr::declare(e.to_string()))?
    };

    // Short borrows around the nested eval (the [R1] discipline): set current_pkg, drop the
    // borrow, recurse, then restore — never hold a Session borrow across `eval_build_src`.
    let repo_ctx = pkg.strip_prefix('@').and_then(|rest| {
        rest.split_once("//")
            .map(|(r, sub)| (r.to_string(), sub.to_string()))
    });
    let prev = sess.set_current_pkg(Some(pkg.to_string()));
    let res = eval_build_src_in(sess, &format!("{pkg}/BUILD"), &src, repo_ctx, drive_all);
    sess.set_current_pkg(prev);
    res
}


/// Analyze a **multi-package** workspace rooted at `root`, starting from
/// `top_label` (`//pkg:name`) and loading dependency packages on demand. Targets
/// are keyed by canonical `//pkg:name` labels with package-qualified paths.
pub fn analyze_workspace(root: &Path, top_label: &str) -> Result<Vec<AnalyzedTarget>, String> {
    analyze_workspace_with(root, top_label, GlobalFlags::default())
}

/// Seed the `@crates` materialization context onto `flags` — SHARED by the build driver
/// ([`analyze_workspace_resolved`]) and the query driver ([`crate::load_query_graph`]) so both
/// materialize `@crates` identically (§11.3 / RazelRustParityPlan B2). (1) Read the root
/// `MODULE.bazel.lock` into `crate_lock` (read-if-present — a non-`@crates` workspace has no lock,
/// an already-seeded caller/test wins; a malformed lock stays `None`, so a real `@crates` build
/// fails later with a clear "not vendored", not a cryptic parse error). (2) With the lock seeded,
/// point `fetched_external_base` at a workspace-local `.razel-crates` so `load_package_body`
/// materializes each repo LAZILY on first demand (only the requested closure, not the whole lock).
/// SKIPPED when a caller already pointed `fetched_external_base` at vendored dirs (parity/unit
/// tests). Inert on a non-`@crates` workspace (no lock → no base).
pub(crate) fn seed_crate_lock_and_base(root: &Path, flags: &mut GlobalFlags) {
    if flags.crate_lock.is_none() {
        let lock_path = root.join("MODULE.bazel.lock");
        if lock_path.exists()
            && let Ok(lock) = crate::lock::read_lock(&lock_path)
        {
            flags.crate_lock = Some(std::sync::Arc::new(lock));
        }
    }
    if flags.fetched_external_base.is_none() && flags.crate_lock.is_some() {
        flags.fetched_external_base = Some(root.join(".razel-crates"));
    }
}

/// Canonicalize a query PATTERN's `@crates`-family repo to its `@@rules_rust++crate+…` identity
/// (§11.3 / q4), so an apparent `@crates//:blake3` entry both MATERIALIZES (the loader + lock are
/// canonical-keyed) and MATCHES the canonical-keyed loaded graph. Seeds the lock from `root` if
/// `flags` carries none. A `//` workspace label or a non-crate `@repo` (`@rules_rust`/`@platforms`)
/// returns verbatim. The query golden comparator normalizes the canonical display back to apparent.
pub fn canonicalize_query_pattern(root: &Path, flags: &GlobalFlags, label: &str) -> String {
    if !label.starts_with('@') {
        return label.to_string();
    }
    let mut f = flags.clone();
    seed_crate_lock_and_base(root, &mut f);
    match &f.crate_lock {
        Some(lock) => crate::state::canonicalize_crate_repo_lock(lock, label),
        None => label.to_string(),
    }
}


/// [`analyze_workspace`] with build-wide [`GlobalFlags`] applied to every cc action.
pub fn analyze_workspace_with(
    root: &Path,
    top_label: &str,
    flags: GlobalFlags,
) -> Result<Vec<AnalyzedTarget>, String> {
    analyze_workspace_resolved(root, top_label, flags).map(|(targets, _)| targets)
}


/// Like [`analyze_workspace_with`] but ALSO returns the requested target's RESOLVED canonical name —
/// the alias chain walked to its terminal `actual` (`@crates//:blake3` → `@@rules_rust++crate+crates__
/// blake3-1.8.2//:blake3`). The build path needs this to `collect_order` from the name the analysis
/// actually keyed the target under (the apparent alias is NOT a target name) — RazelRustParityPlan B4.
pub fn analyze_workspace_resolved(
    root: &Path,
    top_label: &str,
    mut flags: GlobalFlags,
) -> Result<(Vec<AnalyzedTarget>, String), String> {
    // Seed the `@crates` materialization context (lock + `.razel-crates` base), shared with the
    // query driver (§11.3 / RazelRustParityPlan B2). Inert on a non-`@crates` workspace; an
    // already-seeded caller (parity/unit tests) wins.
    seed_crate_lock_and_base(root, &mut flags);
    let session = Session::new(Some(root.to_path_buf()), flags);
    let top_canon = canon_label(&session, top_label);
    let top_pkg = pkg_of(&top_canon)
        .ok_or_else(|| format!("top label must be //pkg:name, got `{top_label}`"))?;
    load_package_entry(&session, &top_pkg)?;
    // A build of an ALIAS top-label builds its terminal `actual` (Bazel resolves the alias, not
    // the alias node). Follow the chain — the alias map fills as the package loads — loading each
    // actual's package on the way (e.g. `@crates//:blake3` → `@crates__blake3-1.8.2//:blake3`).
    // The bound mirrors `resolve_dep`'s alias walk; a cycle just stops (no terminal analyzed).
    let mut canon = top_canon;
    for _ in 0..32 {
        let Some(actual) = session.aliases.borrow().get(&canon).cloned() else { break };
        canon = actual;
        if let Some(pkg) = pkg_of(&canon) {
            load_package_entry(&session, &pkg)?;
        }
    }
    let targets = session.take_targets();
    // P3.4b/P4.3 (§5.4): an incompatible target is a LOUD ERROR only when it is REQUIRED — the
    // EXPLICIT top label, or a dep that survives `select()` resolution into the built target's
    // transitive closure. A target merely DECLARED in the loaded package — sitting in a non-taken
    // `select()` arm, or an unrelated sibling — is SKIPPED, never errored (matching a wildcard
    // build's skip; that CLI-loop filter is P3.4c). So walk `canon`'s closure over the analyzed dep
    // graph and error only when it actually REACHES an incompatible target — never on mere presence.
    let bad = {
        let incompat = session.incompatible_targets.borrow();
        if incompat.is_empty() {
            None
        } else {
            let deps_of: std::collections::HashMap<&str, &[String]> =
                targets.iter().map(|t| (t.name.as_str(), t.deps.as_slice())).collect();
            let mut seen = std::collections::HashSet::new();
            let mut stack = vec![canon.clone()];
            let mut hit = None;
            while let Some(n) = stack.pop() {
                if !seen.insert(n.clone()) {
                    continue;
                }
                if incompat.contains(&n) {
                    hit = Some(n);
                    break;
                }
                if let Some(deps) = deps_of.get(n.as_str()) {
                    stack.extend(deps.iter().cloned());
                }
            }
            hit
        }
    };
    if let Some(bad) = bad {
        return Err(format!(
            "target `{bad}` is incompatible with the target platform (target_compatible_with) — \
             it cannot be built when named or required by a built target"
        ));
    }
    Ok((targets, canon))
}


