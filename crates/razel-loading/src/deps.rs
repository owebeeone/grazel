//! Dep-provider resolution: record_target, resolve_dep, the DepInfo a dependent reads. C0.

use crate::state::{AnalyzedTarget, Session, canon_label, pkg_of};
use crate::rules::load_package;
use std::collections::BTreeMap;




// ---- native cc rules (the "build Google's BUILD files" path) -------------------
//
// `load("@rules_cc//cc:cc_binary.bzl", "cc_binary")` resolves to these — razel
// provides cc_library/cc_binary *natively* (via the host gnu/clang toolchain)
// instead of executing rules_cc's Starlark. The declared `srcs`/`hdrs`/`deps` are
// exactly the sandbox's declared inputs, so F12 enforcement holds with no header
// discovery (Bazel already makes you declare them).



pub(crate) fn record_target(sess: &Session, t: AnalyzedTarget) {
    // E0d: assert into the Session's live fact store as we go — folds read it directly. A failure
    // here is a programming error (registry schema and capture out of sync), like `session()`.
    crate::dds::assert_target(&mut crate::dds::session_dds(sess), &t, razel_dds::InstanceId::SINGLE)
        .expect("DDS assert: provider value/schema mismatch");
    sess.results.borrow_mut().insert(t.name.clone(), t.clone());
    sess.state.borrow_mut().targets.push(t);
}


/// What a dep contributes to its users: its linkable outputs (`libs` — own `DefaultInfo`), its
/// canonical label, and the TRANSITIVE dep-folded provider fields keyed by dep-struct projection
/// (`fields` — e.g. `"headers"`, `"cflags"`; C3a.3b). A native rule reads its own projections via
/// [`DepInfo::field`] — the generic `resolve_dep` no longer hardcodes cc/java.
pub(crate) struct DepInfo {
    pub(crate) libs: Vec<String>,
    pub(crate) canon: String,
    fields: BTreeMap<String, Vec<String>>,
}

impl DepInfo {
    /// A transitive dep-folded field by projection name (empty if the dep doesn't provide it).
    pub(crate) fn field(&self, projection: &str) -> Vec<String> {
        self.fields.get(projection).cloned().unwrap_or_default()
    }
}

/// Resolve a dep label to its [`DepInfo`]. A same-package pending declaration (E0c) is analyzed
/// on demand — forward references resolve; in workspace mode a cross-package dep's package loads
/// on demand; otherwise a missing dep errors clearly.
pub(crate) fn resolve_dep<'v>(
    eval: &mut starlark::eval::Evaluator<'v, '_, '_>,
    label: &str,
) -> anyhow::Result<DepInfo> {
    use razel_dds::InstanceId;
    let sess = crate::state::session(eval);
    let mut canon = canon_label(sess, label);
    // E0c: a forward-referenced local declaration analyzes first (demand-driven).
    crate::dialect::ensure_analyzed(eval, &canon).map_err(|e| anyhow::anyhow!("{e}"))?;
    // Aliases forward to their `actual` (files/providers live on the terminal target).
    for _ in 0..32 {
        let next = crate::state::session(eval).aliases.borrow().get(&canon).cloned();
        match next {
            Some(actual) => {
                canon = actual;
                crate::dialect::ensure_analyzed(eval, &canon)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
            }
            None => break,
        }
    }
    let sess = crate::state::session(eval);
    // Workspace mode: lazy-load the dep's package if absent. The borrow in the condition is dropped
    // before `load_package` recurses into a nested eval (the [R1] discipline — a held `results`
    // borrow across the nested eval would double-borrow-panic).
    if sess.results.borrow().get(&canon).is_none()
        && sess.workspace.is_some()
        && let Some(pkg) = pkg_of(&canon)
    {
        let _ = load_package(sess, &pkg);
    }
    let results = sess.results.borrow();
    let Some(t) = results.get(&canon) else {
        drop(results);
        // A GENERATED-output file label: the producer analyzes on demand; the dep's file is
        // the registered output path.
        let produced = sess.output_index.borrow().get(&canon).cloned();
        if let Some((producer, out_path)) = produced {
            crate::dialect::ensure_analyzed(eval, &producer)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            return Ok(DepInfo {
                libs: vec![out_path],
                canon,
                fields: Default::default(),
            });
        }
        // Bazel file-label semantics: a label naming no declared target resolves to a SOURCE
        // FILE when it exists (mirrors the deps-arm fallback). External labels check the
        // vendored repo (exec-root path form).
        let on_disk = if let Some(rest) = canon.strip_prefix('@') {
            rest.split_once("//")
                .and_then(|(r, pf)| pf.split_once(':').map(|(p, f)| (r, p, f)))
                .and_then(|(repo, pkg, file)| {
                    sess.global.external_base.as_ref().and_then(|base| {
                        [repo.to_string(), repo.replace('_', "-")]
                            .iter()
                            .find(|d| crate::state::path_is_file(sess, &base.join(d).join(pkg).join(file)))
                            .map(|_| format!("external/{repo}/{pkg}/{file}"))
                    })
                })
        } else {
            canon.strip_prefix("//").and_then(|rest| {
                rest.split_once(':').and_then(|(p, f)| {
                    let rel = if p.is_empty() { f.to_string() } else { format!("{p}/{f}") };
                    sess.workspace
                        .as_ref()
                        .filter(|root| crate::state::path_is_file(sess, &root.join(&rel)))
                        .map(|_| rel)
                })
            })
        };
        if let Some(path) = on_disk {
            return Ok(DepInfo { libs: vec![path], canon, fields: Default::default() });
        }
        return Err(anyhow::anyhow!(
            "dep `{label}` not analyzed — declare it before its users (cyclic or missing package)"
        ));
    };
    let libs = t.default_info.clone();
    drop(results);
    // The transitive provider closure via the ONE registry-driven fold (C3a.3b), over the
    // Session's LIVE store (E0d) — no per-dep rebuild, no snapshot clones.
    let fields = if let Some(hit) = sess.fold_cache.borrow().get(&canon) {
        hit.clone()
    } else {
        let key =
            crate::dds::target_key(InstanceId::SINGLE, &canon).map_err(|e| anyhow::anyhow!(e))?;
        let dds = crate::dds::session_dds(sess);
        let f = crate::dds::fold_dep_fields(&dds, &key);
        drop(dds);
        sess.fold_cache.borrow_mut().insert(canon.clone(), f.clone());
        f
    };
    Ok(DepInfo { libs, canon, fields: fields.into_iter().collect() })
}


/// Record an analysis-visible target with no actions (a build-graph placeholder).
pub(crate) fn record_named(sess: &Session, name: &str) {
    record_target(sess, AnalyzedTarget {
        name: canon_label(sess, name),
        ..Default::default()
    });
}

