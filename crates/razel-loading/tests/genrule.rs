//! genrule (razelV3 — Bazel's most ubiquitous native rule; 129 uses in TF alone). One bash action:
//! `cmd` with Make-variable expansion — `$@` (the single output), `$<` (the single src), `$(SRCS)`,
//! `$(OUTS)`, `$(location X)`/`$(locations X)`, `$$` escape. Unmodeled variables error LOUDLY.
//! Test-first (AGENTS.md).

use razel_loading::analyze_starlark;

fn target<'a>(
    ts: &'a [razel_loading::AnalyzedTarget],
    name: &str,
) -> &'a razel_loading::AnalyzedTarget {
    ts.iter().find(|t| t.name.ends_with(name)).unwrap()
}

#[test]
fn single_src_single_out_expands_positional_vars() {
    let src = r#"
genrule(name = "g", srcs = ["in.txt"], outs = ["out.txt"], cmd = "cp $< $@")
"#;
    let ts = analyze_starlark("BUILD", src).unwrap();
    let g = target(&ts, "g");
    let a = &g.actions[0];
    assert_eq!(a.mnemonic, "Genrule");
    assert_eq!(a.argv, ["/bin/bash", "-c", "cp in.txt out.txt"]);
    assert_eq!(a.inputs, ["in.txt"]);
    assert_eq!(a.outputs, ["out.txt"]);
    assert_eq!(g.default_info, ["out.txt"]);
}

#[test]
fn srcs_outs_vars_join_all_paths() {
    let src = r#"
genrule(name = "g", srcs = ["a.txt", "b.txt"], outs = ["x", "y"], cmd = "tool $(SRCS) -- $(OUTS)")
"#;
    let ts = analyze_starlark("BUILD", src).unwrap();
    assert_eq!(target(&ts, "g").actions[0].argv[2], "tool a.txt b.txt -- x y");
}

#[test]
fn location_resolves_a_target_src_to_its_files() {
    let src = r#"
filegroup(name = "data", srcs = ["d.txt"])
genrule(name = "g", srcs = [":data", "in.txt"], outs = ["o"], cmd = "use $(location :data) $(location in.txt)")
"#;
    let ts = analyze_starlark("BUILD", src).unwrap();
    let a = &target(&ts, "g").actions[0];
    assert!(a.argv[2].contains("use d.txt"), "location expanded to the dep's file: {}", a.argv[2]);
    assert!(a.inputs.contains(&"d.txt".to_string()), "dep files are inputs: {:?}", a.inputs);
}

#[test]
fn dollar_dollar_escapes() {
    let src = r#"
genrule(name = "g", srcs = [], outs = ["o"], cmd = "echo $$HOME > $@")
"#;
    let ts = analyze_starlark("BUILD", src).unwrap();
    assert_eq!(target(&ts, "g").actions[0].argv[2], "echo $HOME > o");
}

#[test]
fn at_with_multiple_outs_errors() {
    let src = r#"
genrule(name = "g", srcs = [], outs = ["a", "b"], cmd = "touch $@")
"#;
    let err = analyze_starlark("BUILD", src).unwrap_err();
    assert!(err.contains("exactly one output"), "$@ needs exactly one out: {err}");
}

#[test]
fn unmodeled_make_variable_errors() {
    let src = r#"
genrule(name = "g", srcs = [], outs = ["o"], cmd = "$(JAVABASE)/bin/java -o o")
"#;
    let err = analyze_starlark("BUILD", src).unwrap_err();
    assert!(err.contains("JAVABASE"), "unmodeled variable must error loudly: {err}");
}
