//! `constrain` — the cc feature-config interpreter (RazelV2Contracts `Constrain`; the §8c engine).
//!
//! Reimplements Bazel's feature **selection** (default-enabled + requested, `implies` transitive
//! closure, then a `requires` fix-point) and flag **expansion** (per enabled feature in config
//! order: each `flag_set` matching the action + `with_features` gate; each `flag_group` after
//! `expand_if_available`/`_not_available` gates, with `iterate_over` unrolling and `%{var}`
//! substitution) over a toolchain's feature definitions + an action's variables. See
//! `dev-docs/BazelCcCommandLine.md`. Pure + deterministic.
//!
//! The real toolchain config (`unix_cc_toolchain_config.bzl` + the `cc_configure`-detected flag
//! lists) is **ingested into these types** — the `.bzl` parser is a later slice. Reserved:
//! `coptsFilter` (+ its `unfiltered_compile_flags` exemption), `expand_if_{true,false,equal}`,
//! `action_config` tool paths, and `%%` literal-percent escaping (no current flag uses it).

use std::collections::{BTreeMap, BTreeSet};

/// An action variable value: a scalar (for `%{var}`) or a sequence (for `iterate_over`).
#[derive(Clone, Debug)]
pub enum VarValue {
    Scalar(String),
    Sequence(Vec<String>),
}

/// The variables an action provides (`source_file`, `output_file`, `quote_include_paths`, …).
pub type Vars = BTreeMap<String, VarValue>;

/// A `flag_group`: literal/`%{var}` flags + optional `iterate_over` + presence gates.
#[derive(Clone, Debug, Default)]
pub struct FlagGroup {
    pub flags: Vec<String>,
    pub iterate_over: Option<String>,
    pub expand_if_available: Vec<String>,
    pub expand_if_not_available: Vec<String>,
}

/// `with_feature_set`: satisfied when all `features` are enabled AND all `not_features` disabled.
#[derive(Clone, Debug, Default)]
pub struct WithFeatures {
    pub features: Vec<String>,
    pub not_features: Vec<String>,
}

/// A `flag_set`: applies to `actions`; gated by `with_features` (disjunction over the sets);
/// ordered `flag_groups`.
#[derive(Clone, Debug, Default)]
pub struct FlagSet {
    pub actions: Vec<String>,
    pub with_features: Vec<WithFeatures>,
    pub flag_groups: Vec<FlagGroup>,
}

/// A `feature`: name, default-enabled, flag_sets, `implies`, `requires`
/// (disjunction-of-conjunction), `provides` (collision symbols — reserved).
#[derive(Clone, Debug, Default)]
pub struct Feature {
    pub name: String,
    pub enabled: bool,
    pub flag_sets: Vec<FlagSet>,
    pub implies: Vec<String>,
    pub requires: Vec<Vec<String>>,
    pub provides: Vec<String>,
}

/// A toolchain's feature definitions, in declaration order (the ingested config).
#[derive(Clone, Debug, Default)]
pub struct FeatureConfig {
    pub features: Vec<Feature>,
}

impl FeatureConfig {
    fn get(&self, name: &str) -> Option<&Feature> {
        self.features.iter().find(|f| f.name == name)
    }

    /// Select the enabled feature set (names, in config order) for `requested`: seed with
    /// requested + default-enabled, take the `implies` closure, then a fix-point that disables
    /// any feature whose `requires` aren't met. (`provides`-collision detection is reserved.)
    pub fn select(&self, requested: &[String]) -> Vec<String> {
        let mut enabled: BTreeSet<String> = self
            .features
            .iter()
            .filter(|f| f.enabled)
            .map(|f| f.name.clone())
            .collect();
        // Requested features are filtered to those the toolchain actually defines — Bazel
        // ignores unknown requested names (so `with_features`/`requires` reference real features).
        enabled.extend(requested.iter().filter(|r| self.get(r).is_some()).cloned());

        // `implies` transitive closure.
        loop {
            let mut added = false;
            for name in enabled.iter().cloned().collect::<Vec<_>>() {
                if let Some(f) = self.get(&name) {
                    for imp in &f.implies {
                        added |= enabled.insert(imp.clone());
                    }
                }
            }
            if !added {
                break;
            }
        }

        // `requires` fix-point: disable a feature whose requirements aren't met (no require set
        // fully enabled). Repeat until stable (a disable may invalidate another's requirement).
        loop {
            let mut removed = false;
            for name in enabled.iter().cloned().collect::<Vec<_>>() {
                if let Some(f) = self.get(&name)
                    && !f.requires.is_empty()
                    && !f.requires.iter().any(|set| set.iter().all(|r| enabled.contains(r)))
                {
                    enabled.remove(&name);
                    removed = true;
                }
            }
            if !removed {
                break;
            }
        }

        self.features
            .iter()
            .map(|f| f.name.clone())
            .filter(|n| enabled.contains(n))
            .collect()
    }

    /// Expand the command line for `action` given the `enabled` features + the action's `vars`:
    /// each enabled feature (config order) → each flag_set matching `action` + `with_features` →
    /// each flag_group (after presence gates + `iterate_over` + `%{var}` substitution).
    pub fn command_line(&self, enabled: &[String], action: &str, vars: &Vars) -> Vec<String> {
        let enabled_set: BTreeSet<&str> = enabled.iter().map(String::as_str).collect();
        let mut out = Vec::new();
        for name in enabled {
            let Some(feature) = self.get(name) else { continue };
            for fs in &feature.flag_sets {
                if !fs.actions.iter().any(|a| a == action) {
                    continue;
                }
                if !with_features_ok(&fs.with_features, &enabled_set) {
                    continue;
                }
                for fg in &fs.flag_groups {
                    expand_flag_group(fg, vars, &mut out);
                }
            }
        }
        out
    }
}

/// Satisfied if any `with_feature_set` matches (all its `features` enabled and all `not_features`
/// disabled). Empty ⇒ always applies.
fn with_features_ok(sets: &[WithFeatures], enabled: &BTreeSet<&str>) -> bool {
    sets.is_empty()
        || sets.iter().any(|w| {
            w.features.iter().all(|f| enabled.contains(f.as_str()))
                && w.not_features.iter().all(|f| !enabled.contains(f.as_str()))
        })
}

fn expand_flag_group(fg: &FlagGroup, vars: &Vars, out: &mut Vec<String>) {
    if !fg.expand_if_available.iter().all(|v| vars.contains_key(v)) {
        return;
    }
    if fg.expand_if_not_available.iter().any(|v| vars.contains_key(v)) {
        return;
    }
    match &fg.iterate_over {
        Some(seq) => {
            let Some(VarValue::Sequence(items)) = vars.get(seq) else { return };
            for item in items {
                // Bind the iterate variable to this element's scalar for the iteration's scope.
                let mut scoped = vars.clone();
                scoped.insert(seq.clone(), VarValue::Scalar(item.clone()));
                for flag in &fg.flags {
                    out.push(substitute(flag, &scoped));
                }
            }
        }
        None => {
            for flag in &fg.flags {
                out.push(substitute(flag, vars));
            }
        }
    }
}

/// Substitute `%{var}` (scalar) references in a flag template. A missing/sequence var expands to
/// empty — the flag_group's presence gates are expected to prevent that.
fn substitute(flag: &str, vars: &Vars) -> String {
    let mut out = String::with_capacity(flag.len());
    let mut rest = flag;
    while let Some(i) = rest.find("%{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        match after.find('}') {
            Some(j) => {
                if let Some(VarValue::Scalar(v)) = vars.get(&after[..j]) {
                    out.push_str(v);
                }
                rest = &after[j + 1..];
            }
            None => {
                out.push_str("%{");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar(s: &str) -> VarValue {
        VarValue::Scalar(s.into())
    }
    fn seq(xs: &[&str]) -> VarValue {
        VarValue::Sequence(xs.iter().map(|s| s.to_string()).collect())
    }
    fn fg(flags: &[&str]) -> FlagGroup {
        FlagGroup { flags: flags.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }
    fn fs(action: &str, flag_groups: Vec<FlagGroup>) -> FlagSet {
        FlagSet { actions: vec![action.into()], flag_groups, ..Default::default() }
    }
    fn feat(name: &str, enabled: bool, flag_sets: Vec<FlagSet>) -> Feature {
        Feature { name: name.into(), enabled, flag_sets, ..Default::default() }
    }

    /// A slice of the real macOS cc compile config (BazelCcCommandLine.md §D).
    fn cc_config() -> FeatureConfig {
        FeatureConfig {
            features: vec![
                feat("default_compile_flags", true, vec![fs("c++-compile", vec![
                    fg(&["-U_FORTIFY_SOURCE"]),
                    fg(&["-fstack-protector", "-Wall"]),
                ])]),
                feat("dependency_file", true, vec![FlagSet {
                    actions: vec!["c++-compile".into()],
                    flag_groups: vec![FlagGroup {
                        flags: vec!["-MD".into(), "-MF".into(), "%{dependency_file}".into()],
                        expand_if_available: vec!["dependency_file".into()],
                        ..Default::default()
                    }],
                    ..Default::default()
                }]),
                feat("include_paths", true, vec![FlagSet {
                    actions: vec!["c++-compile".into()],
                    flag_groups: vec![FlagGroup {
                        flags: vec!["-iquote".into(), "%{quote_include_paths}".into()],
                        iterate_over: Some("quote_include_paths".into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }]),
                feat("compiler_input_flags", true, vec![fs("c++-compile", vec![fg(&["-c", "%{source_file}"])])]),
                feat("compiler_output_flags", true, vec![fs("c++-compile", vec![fg(&["-o", "%{output_file}"])])]),
                // `opt` is NOT default-enabled — only fires when the build mode requests it.
                feat("opt", false, vec![fs("c++-compile", vec![fg(&["-O2"])])]),
            ],
        }
    }

    fn cc_vars() -> Vars {
        Vars::from([
            ("source_file".into(), scalar("util.cc")),
            ("output_file".into(), scalar("util.o")),
            ("dependency_file".into(), scalar("util.d")),
            ("quote_include_paths".into(), seq(&[".", "bazel-out/bin"])),
        ])
    }

    #[test]
    fn expands_compile_argv_in_feature_order() {
        let cfg = cc_config();
        let argv = cfg.command_line(&cfg.select(&[]), "c++-compile", &cc_vars());
        assert_eq!(
            argv,
            [
                "-U_FORTIFY_SOURCE", "-fstack-protector", "-Wall", // default_compile_flags
                "-MD", "-MF", "util.d",                            // dependency_file
                "-iquote", ".", "-iquote", "bazel-out/bin",        // include_paths (iterate_over)
                "-c", "util.cc",                                   // compiler_input_flags
                "-o", "util.o",                                    // compiler_output_flags
            ]
        );
    }

    #[test]
    fn opt_only_fires_when_requested() {
        let cfg = cc_config();
        assert!(!cfg.command_line(&cfg.select(&[]), "c++-compile", &cc_vars()).contains(&"-O2".into()));
        assert!(cfg.command_line(&cfg.select(&["opt".into()]), "c++-compile", &cc_vars()).contains(&"-O2".into()));
    }

    #[test]
    fn expand_if_available_gates_on_variable() {
        let cfg = cc_config();
        let mut v = cc_vars();
        v.remove("dependency_file"); // no .d → the -MD/-MF flag_group is gated out
        assert!(!cfg.command_line(&cfg.select(&[]), "c++-compile", &v).contains(&"-MD".into()));
    }

    #[test]
    fn implies_closure_enables_implied() {
        let cfg = FeatureConfig {
            features: vec![
                Feature { name: "a".into(), implies: vec!["b".into()], ..Default::default() },
                feat("b", false, vec![fs("x", vec![fg(&["-b"])])]),
            ],
        };
        assert_eq!(cfg.command_line(&cfg.select(&["a".into()]), "x", &Vars::new()), ["-b"]);
    }

    #[test]
    fn requires_disables_when_unmet() {
        let cfg = FeatureConfig {
            features: vec![
                feat("pic", false, vec![]), // a real (build-mode) feature, enabled on request
                Feature {
                    name: "needs_pic".into(),
                    enabled: true,
                    requires: vec![vec!["pic".into()]],
                    flag_sets: vec![fs("x", vec![fg(&["-fPIC"])])],
                    ..Default::default()
                },
            ],
        };
        assert!(cfg.command_line(&cfg.select(&[]), "x", &Vars::new()).is_empty()); // pic unmet
        assert_eq!(cfg.command_line(&cfg.select(&["pic".into()]), "x", &Vars::new()), ["-fPIC"]);
    }

    #[test]
    fn with_features_gates_flag_set() {
        let cfg = FeatureConfig {
            features: vec![
                feat("dbg", false, vec![]), // a real build-mode feature, enabled on request
                feat("f", true, vec![FlagSet {
                    actions: vec!["x".into()],
                    with_features: vec![WithFeatures { features: vec!["dbg".into()], not_features: vec![] }],
                    flag_groups: vec![fg(&["-g"])],
                }]),
            ],
        };
        assert!(cfg.command_line(&cfg.select(&[]), "x", &Vars::new()).is_empty()); // dbg off
        assert_eq!(cfg.command_line(&cfg.select(&["dbg".into()]), "x", &Vars::new()), ["-g"]);
    }
}
