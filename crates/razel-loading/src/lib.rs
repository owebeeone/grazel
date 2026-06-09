//! Loading (Phase 2): the `BUILD`/`.bzl` evaluator + the shared glob matcher.
//!
//! The live path is in [`rules`]: `analyze_starlark`/`analyze_bazel`/`analyze_workspace`
//! evaluate Starlark `rule()` impls and produce [`AnalyzedTarget`]s (consumed by
//! `razel-analysis`). [`glob_match`] is the glob matcher used by the rule machinery.
//!
//! Phase 1 removed the dead *first* loader — `load_build`/`TargetDecl`/`build_rules`/
//! `query_targets` + the `CTX` thread-local — which had no live callers (the live path never
//! used it). V2 rebuilds loading as a graph effect over the DDS (RazelV2Contracts §6).

pub mod rules;
pub mod state; // C0 decomposition: per-analysis state + core types + the host-cc tool layer
mod providers; // C0: the transitive provider-field fold
mod values;
mod dialect;
mod glob;
mod deps;
mod native_cc;
mod shims;
mod engine;
pub mod dds; // C2: loader -> DDS provider bridge
// Per-language native rulesets — each maps a `@rules_*//` load to native rules, registered in
// `rules::ruleset_modules`. (Independent modules so language support lands without touching the
// shared cc/analysis core.)
mod py_rules;
mod rust_rules;
mod sh_rules;
pub use rules::{
    analyze_bazel, analyze_bazel_with, analyze_starlark, analyze_workspace, analyze_workspace_with,
};
pub use state::{AnalyzedAction, AnalyzedTarget, CcToolchainMode, GlobalFlags};

/// Match a `glob` pattern against a path. Supports `*` (within a segment) and `**`
/// (across segments). A documented subset of Bazel glob — enough for `*.cc`, `a/*.h`,
/// `src/**/*.cc`. (No `?`, char-classes, or `**` mid-segment.)
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let p: Vec<&str> = pattern.split('/').collect();
    let s: Vec<&str> = path.split('/').collect();
    seg_match(&p, &s)
}

fn seg_match(pat: &[&str], path: &[&str]) -> bool {
    match pat.first() {
        None => path.is_empty(),
        Some(&"**") => {
            // `**` matches zero or more path segments.
            (0..=path.len()).any(|i| seg_match(&pat[1..], &path[i..]))
        }
        Some(seg) => {
            !path.is_empty() && star_match(seg, path[0]) && seg_match(&pat[1..], &path[1..])
        }
    }
}

/// Single-segment match with `*` = any run of non-`/` chars.
fn star_match(pat: &str, s: &str) -> bool {
    match pat.split_once('*') {
        None => pat == s,
        Some((pre, rest)) => {
            if !s.starts_with(pre) {
                return false;
            }
            let s = &s[pre.len()..];
            // try every split point for the `*`.
            (0..=s.len()).any(|i| star_match(rest, &s[i..]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matcher_subset() {
        assert!(glob_match("*.cc", "a.cc"));
        assert!(!glob_match("*.cc", "a.h"));
        assert!(glob_match("src/*.cc", "src/a.cc"));
        assert!(!glob_match("src/*.cc", "src/sub/a.cc"));
        assert!(glob_match("**/*.h", "a.h"));
        assert!(glob_match("**/*.h", "src/sub/a.h"));
        assert!(glob_match("src/**/*.cc", "src/a/b/c.cc"));
        assert!(!glob_match("src/**/*.cc", "other/a.cc"));
    }
}
