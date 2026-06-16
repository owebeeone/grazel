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
        repo_dir.join(sub)
    } else {
        sess.workspace
            .clone()
            .ok_or_else(|| LoadErr::declare("load_package called outside workspace mode"))?
            .join(pkg)
    };
    // Resolve the package file UNCONDITIONALLY (cheap stat probes): the E-mode XOR and
    // boundary guard must hold even when the parallel pre-pass already cached the AST.
    let build_path = crate::workspace::resolve_build_file(&pkg_dir, sess.global.strict_bazel)
        .map_err(LoadErr::declare)?
        .ok_or_else(|| {
            LoadErr::declare(format!(
                "no BUILD in package `{pkg}` ({})",
                pkg_dir.display()
            ))
        })?;
    // E-package in the MAIN repo: the boundary guard (§3c rule 2 — warning during
    // S1 only; a hard ERROR since S3d: boundary divergence must not be warnable).
    if build_path.file_name().is_some_and(|f| f == "BUILD.razel") && !pkg.starts_with('@') {
        if let Some(root) = sess.workspace.as_deref() {
            let ignore = std::fs::read_to_string(root.join(".bazelignore")).ok();
            if let Some(w) = crate::workspace::e_mode_guard(
                crate::workspace::root_is_dual(root),
                ignore.as_deref(),
                pkg,
            ) {
                return Err(LoadErr::declare(w));
            }
        }
    }
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
    // P3.1e: seed the `@crates` lock so `@crates` labels canonicalize (§11.3). Read-if-present —
    // a non-`@crates` workspace has no lock (or no `@crates` labels), so this is inert there; an
    // already-seeded `crate_lock` (a caller/test) wins. A malformed lock stays `None`: a real
    // `@crates` build then fails later with a clear "not vendored", not a cryptic parse error here.
    if flags.crate_lock.is_none() {
        let lock_path = root.join("MODULE.bazel.lock");
        if lock_path.exists()
            && let Ok(lock) = crate::lock::read_lock(&lock_path)
        {
            flags.crate_lock = Some(std::sync::Arc::new(lock));
        }
    }
    // RazelRustParityPlan B2 (§2.2/§5.6): with the `@crates` lock seeded, resolve external repos
    // against a workspace-local `.razel-crates` dir; `load_package_body` materializes each repo from
    // the lock LAZILY on first demand (root → inline; per-crate → `fetch_crate`), so only the
    // requested closure (e.g. blake3's ~15 crates) is fetched — NOT the whole 140-crate lock. SKIPPED
    // when a caller already pointed `fetched_external_base` at vendored dirs (the parity/unit tests).
    // A non-`@crates` workspace has no lock (`read_lock` requires a root `@crates` repo) → inert here.
    if flags.fetched_external_base.is_none() && flags.crate_lock.is_some() {
        flags.fetched_external_base = Some(root.join(".razel-crates"));
    }
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
    // P3.4b (§5.4): an incompatible target reached by an EXPLICIT request (the top label) or pulled
    // in as a DEP of a compatible target is a LOUD ERROR — never a silent drop. (A wildcard build
    // SKIPS incompatible targets instead; that filter lives in the CLI's pattern loop — P3.4c.)
    if let Some(bad) = targets
        .iter()
        .find(|t| session.incompatible_targets.borrow().contains(&t.name))
    {
        return Err(format!(
            "target `{}` is incompatible with the target platform (target_compatible_with) — \
             it cannot be built when named or required by a built target",
            bad.name
        ));
    }
    Ok((targets, canon))
}


