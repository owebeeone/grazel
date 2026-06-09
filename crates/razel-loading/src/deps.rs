//! Dep-provider resolution: record_target, resolve_dep, the DepInfo a dependent reads. C0.

use crate::state::{AnalyzedTarget, Session, canon_label, pkg_of};
use crate::rules::load_package;




// ---- native cc rules (the "build Google's BUILD files" path) -------------------
//
// `load("@rules_cc//cc:cc_binary.bzl", "cc_binary")` resolves to these — razel
// provides cc_library/cc_binary *natively* (via the host gnu/clang toolchain)
// instead of executing rules_cc's Starlark. The declared `srcs`/`hdrs`/`deps` are
// exactly the sandbox's declared inputs, so F12 enforcement holds with no header
// discovery (Bazel already makes you declare them).



pub(crate) fn record_target(sess: &Session, t: AnalyzedTarget) {
    sess.results.borrow_mut().insert(t.name.clone(), t.clone());
    sess.state.borrow_mut().targets.push(t);
}


/// What a dep contributes to its users: linkable outputs, exported hdrs, exported
/// compile flags (defines/includes), and its canonical label.
pub(crate) struct DepInfo {
    pub(crate) libs: Vec<String>,
    pub(crate) hdrs: Vec<String>,
    pub(crate) cflags: Vec<String>,
    pub(crate) canon: String,
}


/// Resolve a dep label to its [`DepInfo`]. In workspace mode a cross-package dep
/// whose package isn't loaded yet is loaded on demand; otherwise a forward/cross
/// reference errors clearly.
pub(crate) fn resolve_dep(sess: &Session, label: &str) -> anyhow::Result<DepInfo> {
    use razel_dds::{DdsRead, FieldId, InstanceId, ProviderTypeId, Scalar};
    let canon = canon_label(sess, label);
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
        return Err(anyhow::anyhow!(
            "dep `{label}` not analyzed — declare it before its users (cyclic or missing package)"
        ));
    };
    let libs = t.default_info.clone();
    // hdrs/cflags = the TRANSITIVE CcInfo closure via the DDS fold (C2d): targets store OWN, the
    // dependent's view is `fold_set` over the dep graph — the same fold the rule() path uses.
    let all: Vec<AnalyzedTarget> = results.values().cloned().collect();
    drop(results);
    let dds = crate::dds::to_dds(&all, InstanceId::SINGLE).map_err(|e| anyhow::anyhow!(e))?;
    let key = crate::dds::target_key(InstanceId::SINGLE, &canon).map_err(|e| anyhow::anyhow!(e))?;
    let cc = ProviderTypeId::new("CcInfo");
    let fold = |field: &str| -> Vec<String> {
        dds.fold_set(&key, &cc, &FieldId::new(field))
            .into_iter()
            .filter_map(|s| if let Scalar::Str(x) = s { Some(x) } else { None })
            .collect()
    };
    Ok(DepInfo { libs, hdrs: fold("hdrs"), cflags: fold("cflags"), canon })
}


/// Record an analysis-visible target with no actions (a build-graph placeholder).
pub(crate) fn record_named(sess: &Session, name: &str) {
    record_target(sess, AnalyzedTarget {
        name: canon_label(sess, name),
        ..Default::default()
    });
}

