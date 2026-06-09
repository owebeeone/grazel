//! Dep-provider resolution: record_target, resolve_dep, the DepInfo a dependent reads. C0.


#[allow(unused_imports)]
use crate::{
    dialect::*, engine::*, glob::*, native_cc::*, providers::*, shims::*, state::*,
    values::*,
};
#[allow(unused_imports)]
use crate::rules::*;


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
    let canon = canon_label(sess, label);
    let get = || {
        sess.results
            .borrow()
            .get(&canon)
            .map(|t| (t.default_info.clone(), t.hdrs.clone(), t.cflags.clone()))
    };
    let hit = get().or_else(|| {
        // Workspace mode: pull in the dep's package, then retry. The `get()` borrow is
        // dropped before `load_package` recurses into a nested eval (the [R1] discipline —
        // a `results` borrow held across the nested eval would double-borrow-panic).
        if sess.workspace.is_some()
            && let Some(pkg) = pkg_of(&canon)
        {
            let _ = load_package(sess, &pkg);
        }
        get()
    });
    let Some((libs, hdrs, cflags)) = hit else {
        return Err(anyhow::anyhow!(
            "dep `{label}` not analyzed — declare it before its users (cyclic or missing package)"
        ));
    };
    Ok(DepInfo {
        libs,
        hdrs,
        cflags,
        canon,
    })
}


/// Record an analysis-visible target with no actions (a build-graph placeholder).
pub(crate) fn record_named(sess: &Session, name: &str) {
    record_target(sess, AnalyzedTarget {
        name: canon_label(sess, name),
        ..Default::default()
    });
}

