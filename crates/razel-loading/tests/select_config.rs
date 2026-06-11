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

/// DEFERRED select (Bazel's model): a select over a not-yet-declared condition is a VALUE,
/// resolved when an attr consumes it at analysis — by which time the condition exists (E0).
/// Real `.bzl` (XLA's tsl.bzl) build module-level `list + select({...})` expressions.
#[test]
fn deferred_select_resolves_at_analysis_with_concat() {
    let src = r#"
def _impl(ctx):
    ctx.actions.run(executable = "tool", outputs = [], inputs = [], arguments = ctx.attr.copts)

r = rule(implementation = _impl, attrs = {})
flags_expr = ["-base"] + select({":opt_mode": ["-opt"], "//conditions:default": ["-noopt"]})
r(name = "t", copts = flags_expr)
config_setting(name = "opt_mode", values = {"compilation_mode": "opt"})
"#;
    let targets = analyze_bazel_with(src, flags("opt", &[])).unwrap();
    let t = targets.iter().find(|t| t.name.ends_with("t")).unwrap();
    assert_eq!(t.actions[0].argv[1..], ["-base".to_string(), "-opt".to_string()],
        "concat + deferred resolution: {:?}", t.actions[0].argv);
}

#[test]
fn truly_undeclared_condition_errors_at_analysis() {
    let src = r#"
def _impl(ctx):
    ctx.actions.run(executable = "tool", outputs = [], inputs = [], arguments = ctx.attr.copts)

r = rule(implementation = _impl, attrs = {})
r(name = "t", copts = select({":never": ["-x"], "//conditions:default": ["-d"]}))
"#;
    let err = analyze_bazel_with(src, flags("opt", &[])).unwrap_err();
    assert!(err.contains("config_setting"), "never-declared condition errors at analysis: {err}");
}

// ---- Round 40: full select deferral (the eager-hybrid retirement) ----------------------

/// The highway 1.3.0 shape (47 pkgs): a module-level `[list] + select({...})` must STAY a
/// select expression (Bazel never resolves at load) so `+ (tuple,)` select-concats — the
/// eager hybrid collapsed it to a plain list and `list + tuple` errored (Bazel rejects that
/// op too; it never sees it because the select stays deferred).
#[test]
fn module_level_select_stays_deferred_and_concats_tuples() {
    let src = r#"
config_setting(name = "dbg", values = {"compilation_mode": "dbg"})
BASE = ["a.txt"] + select({":dbg": ["d.txt"], "//conditions:default": ["r.txt"]})
ALL = BASE + ("t.txt",)

def _impl(ctx):
    return [DefaultInfo(files = ctx.attr.srcs)]

r = rule(implementation = _impl, attrs = {"srcs": attr.string_list()})
r(name = "x", srcs = ALL)
"#;
    let targets = razel_loading::analyze_starlark("BUILD", src).unwrap();
    let x = targets.iter().find(|t| t.name == "x").unwrap();
    assert_eq!(x.default_info, vec!["a.txt", "r.txt", "t.txt"], "default branch + tuple part");
}

/// The ruy shape (46 pkgs): a select whose condition is a `config_setting_group` with a
/// MEMBER in a not-yet-loaded package must DEFER at call time (the eager probe may not
/// error on an unloaded member) and resolve at analysis, demand-loading the member.
#[test]
fn group_member_in_unloaded_package_defers_then_loads() {
    let root = std::env::temp_dir().join(format!("razel-selgroup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("conds")).unwrap();
    std::fs::write(
        root.join("conds/BUILD"),
        "config_setting(name = \"never\", values = {\"compilation_mode\": \"dbg\"})\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::write(
        root.join("a/BUILD"),
        "razel_config_setting_group(name = \"g\", match_any = [\"//conds:never\"])\n\
         filegroup(name = \"x\", srcs = [\"x.txt\"] + select({\":g\": [\"g.txt\"], \"//conditions:default\": [\"d.txt\"]}))\n",
    )
    .unwrap();
    std::fs::write(root.join("a/x.txt"), "").unwrap();
    std::fs::write(root.join("a/d.txt"), "").unwrap();
    let targets =
        razel_loading::analyze_workspace(&root, "//a:x").unwrap();
    let x = targets.iter().find(|t| t.name == "//a:x").unwrap();
    assert_eq!(x.default_info, vec!["a/x.txt", "a/d.txt"], "member loaded; default picked");
    let _ = std::fs::remove_dir_all(&root);
}

/// The 107-pkg class: `filegroup(srcs = <select expression>)` — the typed native param must
/// accept the deferred value and resolve it at the native's analysis.
#[test]
fn filegroup_srcs_accepts_select_expression() {
    let src = r#"
config_setting(name = "dbg", values = {"compilation_mode": "dbg"})
filegroup(name = "fg", srcs = ["a.txt"] + select({":dbg": ["d.txt"], "//conditions:default": ["r.txt"]}))
"#;
    let targets = razel_loading::analyze_starlark("BUILD", src).unwrap();
    let fg = targets.iter().find(|t| t.name == "fg").unwrap();
    assert_eq!(fg.default_info, vec!["a.txt", "r.txt"]);
}
