//! select()/config_setting REAL resolution (razelV3 — retires the first-branch stub, the worst
//! silent-wrong behavior for a Bazel-compatibility goal). `config_setting` declares constraint
//! specs; `select()` matches them against the Session's structured configuration
//! (`compilation_mode`, `--define`), Bazel semantics: all-constraints-match, most-specialized
//! wins, `//conditions:default` fallback, loud errors otherwise. Test-first (AGENTS.md).

use razel_loading::{GlobalFlags, analyze_bazel_with};

fn flags(mode: &str, defines: &[(&str, &str)]) -> GlobalFlags {
    GlobalFlags {
        compilation_mode: mode.to_string(),
        defines: defines.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        ..Default::default()
    }
}

const SRC: &str = r#"
config_setting(name = "opt", values = {"compilation_mode": "opt"})
config_setting(name = "foo_on", define_values = {"foo": "on"})
filegroup(name = "f", srcs = select({":opt": ["opt.txt"], "//conditions:default": ["default.txt"]}))
filegroup(name = "g", srcs = select({":foo_on": ["on.txt"], "//conditions:default": ["off.txt"]}))
"#;

fn files_of(targets: &[razel_loading::AnalyzedTarget], name: &str) -> Vec<String> {
    targets.iter().find(|t| t.name.ends_with(name)).unwrap().default_info.clone()
}

#[test]
fn matching_condition_selects_its_branch() {
    let targets = analyze_bazel_with(SRC, flags("opt", &[])).unwrap();
    assert_eq!(files_of(&targets, "f"), ["opt.txt"], "-c opt matches :opt");
    assert_eq!(files_of(&targets, "g"), ["off.txt"], "no --define -> default");
}

#[test]
fn default_branch_when_nothing_matches() {
    let targets = analyze_bazel_with(SRC, flags("", &[])).unwrap(); // fastbuild
    assert_eq!(files_of(&targets, "f"), ["default.txt"]);
}

#[test]
fn define_values_match() {
    let targets = analyze_bazel_with(SRC, flags("", &[("foo", "on")])).unwrap();
    assert_eq!(files_of(&targets, "g"), ["on.txt"]);
}

#[test]
fn most_specialized_match_wins() {
    let src = r#"
config_setting(name = "opt", values = {"compilation_mode": "opt"})
config_setting(name = "opt_foo", values = {"compilation_mode": "opt"}, define_values = {"foo": "on"})
filegroup(name = "f", srcs = select({":opt": ["a.txt"], ":opt_foo": ["b.txt"]}))
"#;
    let targets = analyze_bazel_with(src, flags("opt", &[("foo", "on")])).unwrap();
    assert_eq!(files_of(&targets, "f"), ["b.txt"], "the strictly-more-constrained condition wins");
}

#[test]
fn ambiguous_matches_error() {
    let src = r#"
config_setting(name = "opt", values = {"compilation_mode": "opt"})
config_setting(name = "foo_on", define_values = {"foo": "on"})
filegroup(name = "f", srcs = select({":opt": ["a.txt"], ":foo_on": ["b.txt"]}))
"#;
    let err = analyze_bazel_with(src, flags("opt", &[("foo", "on")])).unwrap_err();
    assert!(err.contains("ambiguous"), "disjoint co-matching conditions must error: {err}");
}

#[test]
fn no_match_and_no_default_errors() {
    let src = r#"
config_setting(name = "opt", values = {"compilation_mode": "opt"})
filegroup(name = "f", srcs = select({":opt": ["a.txt"]}))
"#;
    let err = analyze_bazel_with(src, flags("", &[])).unwrap_err();
    assert!(err.contains("matched no condition"), "no match + no default must error: {err}");
}

#[test]
fn undeclared_condition_errors_clearly() {
    // Eager resolution: conditions must be declared before the select that uses them (deferred
    // select resolution is registered debt).
    let src = r#"
filegroup(name = "f", srcs = select({":later": ["a.txt"], "//conditions:default": ["d.txt"]}))
config_setting(name = "later", values = {"compilation_mode": "opt"})
"#;
    let err = analyze_bazel_with(src, flags("opt", &[])).unwrap_err();
    assert!(err.contains("config_setting"), "undeclared condition must error clearly: {err}");
}
