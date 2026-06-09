//! Phase 1.1 — characterization snapshots of the LIVE analysis path
//! (`analyze_bazel`/`analyze_starlark` → `AnalyzedTarget`/`AnalyzedAction`).
//!
//! These pin the *current* declared output (full argv/inputs/outputs) so the Phase 1.4/1.5
//! Session migration (the 7 thread-locals → a passed `Analysis` handle) cannot silently
//! regress it. This is a regression guard against razel's OWN output — NOT Bazel parity
//! (that is the separate golden harness, `RazelParityHarness.md`; razel's lowering here
//! deliberately differs from Bazel's feature-config command line).

use razel_loading::{analyze_bazel, analyze_starlark};

const CC_BUILD: &str = r#"
load("@rules_cc//cc:defs.bzl", "cc_library", "cc_binary")
cc_library(name = "greet", srcs = ["greet.cc"], hdrs = ["greet.h"])
cc_binary(name = "app", srcs = ["main.cc"], deps = [":greet"])
"#;

#[test]
fn cc_library_lowering_is_stable() {
    let targets = analyze_bazel(CC_BUILD).unwrap();
    let greet = targets.iter().find(|t| t.name == "greet").unwrap();
    assert_eq!(greet.actions.len(), 2, "cc_library = compile + archive");

    let compile = &greet.actions[0];
    assert_eq!(compile.mnemonic, "CppCompile");
    // ·iii: argv[0] is the PATH-resolved host compiler (Native toolchain), not a hardcoded path.
    let cc = &compile.argv[0];
    assert!(["c++", "clang++", "g++", "cc"].iter().any(|s| cc.ends_with(s)), "resolved cc: {cc}");
    assert_eq!(&compile.argv[1..], ["-iquote", ".", "-c", "greet.cc", "-o", "greet.cc.o"]);
    assert_eq!(compile.inputs, ["greet.cc", "greet.h"]);
    assert_eq!(compile.outputs, ["greet.cc.o"]);

    let archive = &greet.actions[1];
    assert_eq!(archive.mnemonic, "CppArchive");
    assert_eq!(archive.argv, ["/usr/bin/ar", "rcs", "libgreet.a", "greet.cc.o"]);
    assert_eq!(archive.inputs, ["greet.cc.o"]);
    assert_eq!(archive.outputs, ["libgreet.a"]);

    assert_eq!(greet.default_info, ["libgreet.a"]);
}

#[test]
fn cc_binary_links_dep_and_sees_transitive_header() {
    let targets = analyze_bazel(CC_BUILD).unwrap();
    let app = targets.iter().find(|t| t.name == "app").unwrap();
    assert_eq!(app.deps, ["greet"]);
    assert_eq!(app.actions.len(), 2, "cc_binary = compile + link");

    // ·iii: argv[0] is the PATH-resolved host compiler (Native toolchain).
    let is_cc = |s: &str| ["c++", "clang++", "g++", "cc"].iter().any(|c| s.ends_with(c));

    let compile = &app.actions[0];
    assert_eq!(compile.mnemonic, "CppCompile");
    assert!(is_cc(&compile.argv[0]), "resolved cc: {}", compile.argv[0]);
    assert_eq!(&compile.argv[1..], ["-iquote", ".", "-c", "main.cc", "-o", "main.cc.o"]);
    // The dep's public header propagates into the dependent's compile inputs.
    assert_eq!(compile.inputs, ["main.cc", "greet.h"]);

    let link = &app.actions[1];
    assert_eq!(link.mnemonic, "CppLink");
    assert!(is_cc(&link.argv[0]), "resolved cc: {}", link.argv[0]);
    assert_eq!(&link.argv[1..], ["-o", "app", "main.cc.o", "libgreet.a"]);
    assert_eq!(link.inputs, ["main.cc.o", "libgreet.a"]); // dep's archive linked in
    assert_eq!(link.outputs, ["app"]);
    assert_eq!(app.default_info, ["app"]);
}

#[test]
fn legacy_macro_expands_to_native_rules() {
    // A plain Starlark `def` (legacy macro) instantiating native rules expands on the live path.
    let src = r#"
load("@rules_cc//cc:defs.bzl", "cc_library", "cc_binary")
def my_component(name):
    cc_library(name = name + "_lib", srcs = [name + ".cc"])
    cc_binary(name = name, srcs = ["main.cc"], deps = [":" + name + "_lib"])

my_component(name = "widget")
"#;
    let targets = analyze_bazel(src).unwrap();
    assert_eq!(targets.len(), 2);
    assert_eq!(targets[0].name, "widget_lib");
    let bin = targets.iter().find(|t| t.name == "widget").unwrap();
    assert_eq!(bin.deps, ["widget_lib"]);
}

#[test]
fn starlark_rule_impl_declared_action_is_stable() {
    // The user-`rule()` path (running a Starlark impl) — pins the declared action verbatim.
    let src = r#"
def _impl(ctx):
    out = ctx.actions.declare_file(ctx.attr.name + ".o")
    ctx.actions.run(executable = "cc", outputs = [out], inputs = [ctx.attr.src], arguments = ["-c", ctx.attr.src])
    return [DefaultInfo(files = [out])]

cc_thing = rule(implementation = _impl, attrs = {"src": 1})
cc_thing(name = "widget", src = "widget.c")
"#;
    let targets = analyze_starlark("BUILD", src).unwrap();
    assert_eq!(targets.len(), 1);
    let a = &targets[0].actions[0];
    assert_eq!(a.mnemonic, "cc");
    assert_eq!(a.argv, ["cc", "-c", "widget.c"]); // executable is argv[0]
    assert_eq!(a.inputs, ["widget.c"]);
    assert_eq!(a.outputs, ["widget.o"]);
    assert_eq!(targets[0].default_info, ["widget.o"]);
}

/// End-to-end: razel's REAL analyzed cc data (from `analyze_bazel`) drives the §8c faithful argv
/// via the Constrain interpreter. Proves the loader→Constrain wiring on live data. The FLAGS are
/// Bazel-faithful; the PATHS are razel's (the path-model gap — byte-parity needs razel's output
/// layout to match Bazel's `_objs/...`, a separate step). The faithful argv is the *declared*
/// graph (for parity) — distinct from razel's executable command (it names Bazel's cc_wrapper.sh).
#[test]
fn real_analyzed_cc_data_drives_the_faithful_argv() {
    use razel_cc_toolchain::{CompileInputs, cc_compile_argv, macos_core_config};

    let src = r#"
load("@rules_cc//cc:defs.bzl", "cc_library")
cc_library(name = "greet", srcs = ["greet.cc"], hdrs = ["greet.h"])
"#;
    let targets = analyze_bazel(src).unwrap();
    let greet = targets.iter().find(|t| t.name == "greet").unwrap();
    let compile = greet.actions.iter().find(|a| a.mnemonic == "CppCompile").unwrap();
    let source = compile.inputs.iter().find(|i| i.ends_with(".cc")).unwrap().clone();
    let output = compile.outputs[0].clone();

    let cfg = macos_core_config().unwrap();
    let argv = cc_compile_argv(
        &cfg,
        &CompileInputs {
            source_file: source.clone(),
            output_file: output.clone(),
            dependency_file: format!("{output}.d"),
            quote_include_paths: vec![".".into()],
            minimum_os_version: "26.4".into(),
        },
    );

    // Bazel-faithful flags, from razel's real analyzed data:
    assert_eq!(argv[0], "external/<repo>/cc_wrapper.sh");
    assert!(argv.contains(&"-fstack-protector".to_string()));
    assert!(argv.contains(&"-std=c++17".to_string()));
    assert!(argv.contains(&"-mmacosx-version-min=26.4".to_string()));
    assert!(argv.contains(&"-D__DATE__=\"redacted\"".to_string()));
    // razel's own source/output paths flow through -c / -o (the path-model gap vs Bazel's _objs/):
    let c = argv.iter().position(|x| x == "-c").unwrap();
    assert_eq!(argv[c + 1], source);
    let o = argv.iter().position(|x| x == "-o").unwrap();
    assert_eq!(argv[o + 1], output);
}

/// Parity (source-level inputs): razel's transitive-header propagation produces the same source
/// inputs (sources + own + transitive-dep headers) as Bazel's `CppCompile`, reduced to the
/// build-relevant set. Bazel additionally lists generated `.cppmap`s and `external/` toolchain
/// inputs (modules + sandbox detail) and a package prefix — razel's simpler model omits those.
#[test]
fn cc_compile_source_inputs_match_the_golden() {
    let corpus = r#"
load("@rules_cc//cc:defs.bzl", "cc_library")
cc_library(name = "base", srcs = ["base.cc"], hdrs = ["base.h"])
cc_library(name = "util", srcs = ["util.cc"], hdrs = ["util.h"], deps = [":base"])
"#;
    let targets = analyze_bazel(corpus).unwrap();
    let util = targets.iter().find(|t| t.name == "util").unwrap();
    let compile = util.actions.iter().find(|a| a.mnemonic == "CppCompile").unwrap();
    let mut razel = compile.inputs.clone();
    razel.sort();

    // The golden's util CppCompile inputs, reduced to source-level: keep package-relative paths,
    // drop generated `.cppmap`s + `external/` toolchain inputs, strip the package prefix.
    let golden = include_str!("../../../parity/corpus/cc/transitive/golden.txt");
    let block = &golden[golden.find("Compiling corpus/cc/transitive/util.cc").unwrap()..];
    let line = &block[block.find("Inputs: [").unwrap() + "Inputs: [".len()..];
    let pkg = "corpus/cc/transitive/";
    let mut golden_src: Vec<String> = line[..line.find(']').unwrap()]
        .split(", ")
        .filter(|t| t.starts_with(pkg) && !t.ends_with(".cppmap"))
        .map(|t| t[pkg.len()..].to_string())
        .collect();
    golden_src.sort();

    assert_eq!(razel, golden_src); // {base.h, util.cc, util.h}
}
