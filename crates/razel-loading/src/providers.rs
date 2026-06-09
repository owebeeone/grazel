//! The transitive provider-field fold (C0 decomposition). One generic walk over the analyzed
//! dep graph; the cc/java providers' header/classpath assembly fold through it. (C2 will fold
//! this onto the razel-dds field-kind algebra — see RazelGaps F3.)

use crate::state::AnalyzedTarget;
use std::collections::{BTreeMap, BTreeSet};

/// The ONE transitive provider-field fold (F24 — consolidates the former three hand-written folds).
/// Walks `root`'s closure accumulating `field(tgt)`, deduped first-occurrence, cycle-safe.
/// - `ordered=true`  → a preorder walk (deps in declared order), result order-significant (java
///   classpaths).
/// - `ordered=false` → a set (sorted output), order irrelevant (cc headers).
/// - `skip` prunes a node **and its subtree** (the `neverlink` conditional).
///
/// Folds ONE `root` (a single dep); a rule with multiple deps assembles per-dep closures and dedups
/// ACROSS siblings via the `.bzl`'s `dedup()` (F1/F2). NOTE: `razel-dds`'s `DdsRead::fold_set`/
/// `fold_depset` is the same algorithm on the typed DDS spine, but `razel-loading` does not depend on
/// `razel-dds`, so the live loader uses this. Wiring the loader through the DDS fold (tested==run) is
/// the Phase-C "parallel spine" reconciliation — see `RazelGaps.md` (F3).
fn fold_field(
    results: &BTreeMap<String, AnalyzedTarget>,
    root: &str,
    field: impl Fn(&AnalyzedTarget) -> &[String],
    ordered: bool,
    skip: impl Fn(&AnalyzedTarget) -> bool,
) -> Vec<String> {
    let mut acc: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut stack = vec![root.to_string()];
    while let Some(t) = stack.pop() {
        if !visited.insert(t.clone()) {
            continue;
        }
        let Some(tgt) = results.get(&t) else { continue };
        if skip(tgt) {
            continue; // prune this node + its subtree (neverlink)
        }
        for x in field(tgt) {
            if seen.insert(x.clone()) {
                acc.push(x.clone());
            }
        }
        if ordered {
            // Reversed so dep[0] is visited next — preorder, declared dep order preserved.
            for d in tgt.deps.iter().rev() {
                stack.push(d.clone());
            }
        } else {
            stack.extend(tgt.deps.iter().cloned());
        }
    }
    if !ordered {
        acc.sort(); // set semantics: stable sorted output (matches the former BTreeSet fold)
    }
    acc
}

/// Transitive exported headers of one `root` (cc `CcInfo.hdrs` — set-valued, sorted). See [`fold_field`].
pub(crate) fn fold_headers(results: &BTreeMap<String, AnalyzedTarget>, root: &str) -> Vec<String> {
    fold_field(results, root, |t| &t.hdrs, false, |_| false)
}

/// Transitive compile classpath of one `root` (java `JavaInfo.compile_jars` — ordered). See [`fold_field`].
pub(crate) fn fold_compile_jars(results: &BTreeMap<String, AnalyzedTarget>, root: &str) -> Vec<String> {
    fold_field(results, root, |t| &t.compile_jars, true, |_| false)
}

/// Transitive runtime classpath of one `root` (java `JavaInfo.runtime_jars` — ordered; `neverlink`
/// prunes a node + its subtree). See [`fold_field`].
pub(crate) fn fold_runtime_jars(results: &BTreeMap<String, AnalyzedTarget>, root: &str) -> Vec<String> {
    fold_field(results, root, |t| &t.runtime_jars, true, |t| t.neverlink)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn at(name: &str, deps: &[&str], cj: &[&str], rj: &[&str], hdrs: &[&str], neverlink: bool) -> AnalyzedTarget {
        AnalyzedTarget {
            name: name.into(),
            deps: deps.iter().map(|s| s.to_string()).collect(),
            compile_jars: cj.iter().map(|s| s.to_string()).collect(),
            runtime_jars: rj.iter().map(|s| s.to_string()).collect(),
            hdrs: hdrs.iter().map(|s| s.to_string()).collect(),
            neverlink,
            ..Default::default()
        }
    }
    fn graph(ts: Vec<AnalyzedTarget>) -> BTreeMap<String, AnalyzedTarget> {
        ts.into_iter().map(|t| (t.name.clone(), t)).collect()
    }

    #[test]
    fn fold_field_ordered_is_preorder_and_dedups_a_diamond() {
        // app -> [x, y] -> base. compile_jars: preorder, declared dep order, base ONCE.
        let g = graph(vec![
            at("base", &[], &["base.jar"], &[], &[], false),
            at("x", &["base"], &["x.jar"], &[], &[], false),
            at("y", &["base"], &["y.jar"], &[], &[], false),
            at("app", &["x", "y"], &["app.jar"], &[], &[], false),
        ]);
        assert_eq!(fold_compile_jars(&g, "app"), ["app.jar", "x.jar", "base.jar", "y.jar"]);
    }

    #[test]
    fn fold_field_is_cycle_safe() {
        let g = graph(vec![
            at("a", &["b"], &["a.jar"], &[], &[], false),
            at("b", &["a"], &["b.jar"], &[], &[], false),
        ]);
        assert_eq!(fold_compile_jars(&g, "a"), ["a.jar", "b.jar"]); // terminates
    }

    #[test]
    fn fold_field_neverlink_prunes_runtime_subtree_but_not_compile() {
        // app -> api(neverlink) -> hidden. compile sees all; runtime prunes api AND its subtree.
        let g = graph(vec![
            at("hidden", &[], &["hidden.jar"], &["hidden.jar"], &[], false),
            at("api", &["hidden"], &["api.jar"], &["api.jar"], &[], true),
            at("app", &["api"], &["app.jar"], &["app.jar"], &[], false),
        ]);
        assert_eq!(fold_compile_jars(&g, "app"), ["app.jar", "api.jar", "hidden.jar"]);
        assert_eq!(fold_runtime_jars(&g, "app"), ["app.jar"]); // neverlink prunes hidden too
    }

    #[test]
    fn fold_field_unordered_headers_is_sorted_set() {
        let g = graph(vec![
            at("base", &[], &[], &[], &["b.h"], false),
            at("a", &["base"], &[], &[], &["z.h", "a.h"], false),
        ]);
        assert_eq!(fold_headers(&g, "a"), ["a.h", "b.h", "z.h"]); // sorted, deduped
    }
}
