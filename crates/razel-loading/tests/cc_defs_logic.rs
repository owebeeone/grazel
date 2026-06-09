//! A3/4·i: razel's `cc:defs.bzl` LOGIC, proven in isolation via `analyze_starlark` (package "").
//! Asserts the STRUCTURE of the produced actions — mnemonics, faithful argv flags, the `_objs` path
//! shape, transitive headers in inputs. (Golden-match needs the workspace/package context — that's
//! ·ii, the live switch.)

use razel_loading::analyze_starlark;

const CC_DEFS: &str = include_str!("../src/cc_defs.bzl");

#[test]
fn cc_library_rule_logic_produces_structurally_faithful_actions() {
    let src = format!(
        "{CC_DEFS}\n\
         cc_library(name = \"base\", srcs = [\"base.cc\"], hdrs = [\"base.h\"], deps = [])\n\
         cc_library(name = \"util\", srcs = [\"util.cc\"], hdrs = [\"util.h\"], deps = [\":base\"])\n"
    );
    let targets = analyze_starlark("BUILD", &src).unwrap();
    let util = targets.iter().find(|t| t.name.ends_with("util")).unwrap();

    let compile = util.actions.iter().find(|a| a.mnemonic == "CppCompile").unwrap();
    let archive = util.actions.iter().find(|a| a.mnemonic == "CppArchive").unwrap();

    // Faithful compile: cc_wrapper.sh + the feature flags, the _objs path shape.
    assert_eq!(compile.argv[0], "external/<repo>/cc_wrapper.sh");
    assert!(compile.argv.contains(&"-fstack-protector".to_string()));
    assert!(compile.argv.contains(&"-std=c++17".to_string()));
    assert_eq!(
        compile.outputs,
        [
            "bazel-out/darwin_arm64-fastbuild/bin/_objs/util/util.d",
            "bazel-out/darwin_arm64-fastbuild/bin/_objs/util/util.o",
        ]
    );
    // dep.headers folded base.h into util's compile inputs (pkg "" → unqualified here).
    for h in ["util.cc", "util.h", "base.h"] {
        assert!(compile.inputs.contains(&h.to_string()), "missing input {h}");
    }

    // Faithful archive: libtool + the static lib.
    assert_eq!(archive.argv[0], "/usr/bin/libtool");
    assert_eq!(archive.outputs, ["bazel-out/darwin_arm64-fastbuild/bin/libutil.a"]);
}

#[test]
fn cc_diamond_dedups_the_shared_transitive_header() {
    // F1 regression: app -> [x, y] -> base. base.h reaches app via BOTH x and y; the per-dep fold +
    // the .bzl's dedup() must list it ONCE in app's compile inputs, not twice.
    let src = format!(
        "{CC_DEFS}\n\
         cc_library(name = \"base\", srcs = [\"base.cc\"], hdrs = [\"base.h\"], deps = [])\n\
         cc_library(name = \"x\", srcs = [\"x.cc\"], hdrs = [\"x.h\"], deps = [\":base\"])\n\
         cc_library(name = \"y\", srcs = [\"y.cc\"], hdrs = [\"y.h\"], deps = [\":base\"])\n\
         cc_library(name = \"app\", srcs = [\"app.cc\"], hdrs = [\"app.h\"], deps = [\":x\", \":y\"])\n"
    );
    let targets = analyze_starlark("BUILD", &src).unwrap();
    let app = targets.iter().find(|t| t.name.ends_with("app")).unwrap();
    let compile = app.actions.iter().find(|a| a.mnemonic == "CppCompile").unwrap();
    let n = compile.inputs.iter().filter(|i| i.ends_with("base.h")).count();
    assert_eq!(n, 1, "diamond must dedup base.h (got {n}): {:?}", compile.inputs);
}
