//! B3 — the java spike. razel's `java:defs.bzl` produces java's THREE-action structure (Turbine
//! header-jar + Javac + JavaSourceJar) over the existing machinery, with the ORDERED compile
//! classpath (`dep.compile_jars()` — the OrderedDepset fold, B2). Proven structurally via
//! `analyze_starlark` (package ""): the abstraction stretches to java (multi-action kinds, a
//! template command, an ordered header-jar classpath). Byte-parity vs the golden is the B-A0 step.

use razel_loading::analyze_starlark;

const JAVA_DEFS: &str = include_str!("../src/java_defs.bzl");

#[test]
fn java_library_produces_three_actions_and_ordered_header_jar_classpath() {
    let src = format!(
        "{JAVA_DEFS}\n\
         java_library(name = \"base\", srcs = [\"Base.java\"])\n\
         java_library(name = \"util\", srcs = [\"Util.java\"], deps = [\":base\"])\n"
    );
    let targets = analyze_starlark("BUILD", &src).unwrap();
    let util = targets.iter().find(|t| t.name.ends_with("util")).unwrap();

    // THREE action kinds — vs cc's two (compile + archive).
    let mnem: Vec<&str> = util.actions.iter().map(|a| a.mnemonic.as_str()).collect();
    for m in ["Turbine", "Javac", "JavaSourceJar"] {
        assert!(mnem.contains(&m), "missing {m} in {mnem:?}");
    }

    let javac = util.actions.iter().find(|a| a.mnemonic == "Javac").unwrap();
    // Template command (java + JavaBuilder), not a Constrain feature-config.
    assert_eq!(javac.argv[0], "external/<repo>/bin/java");
    assert!(javac.argv.iter().any(|a| a.ends_with("JavaBuilder_deploy.jar")));
    // ORDERED classpath: util compiles against base's HEADER jar (compile-avoidance; the
    // dep.compile_jars() OrderedDepset fold — B2), not base's full jar.
    let base_hjar = "bazel-out/darwin_arm64-fastbuild/bin/libbase-hjar.jar";
    assert!(javac.argv.contains(&base_hjar.to_string()), "classpath missing base hjar: {:?}", javac.argv);
    assert!(javac.inputs.contains(&base_hjar.to_string()));
    assert_eq!(javac.outputs, ["bazel-out/darwin_arm64-fastbuild/bin/libutil.jar"]);

    // JavaInfo captured util's OWN header jar (the OrderedDepset element dependents fold).
    assert_eq!(util.compile_jars(), ["bazel-out/darwin_arm64-fastbuild/bin/libutil-hjar.jar"]);
}

#[test]
fn java_info_dual_depsets_dont_cross_merge_and_neverlink_excludes_runtime() {
    // B4: app deps a normal lib (base) + a neverlink lib (api). The compile classpath sees BOTH
    // (neverlink is compile-time); the runtime classpath sees base's jar but NOT api's (neverlink
    // excluded). And compile carries HEADER jars, runtime carries FULL jars — no cross-merge.
    let src = format!(
        "{JAVA_DEFS}\n\
         java_library(name = \"base\", srcs = [\"Base.java\"])\n\
         java_library(name = \"api\", srcs = [\"Api.java\"], neverlink = True)\n\
         java_library(name = \"app\", srcs = [\"App.java\"], deps = [\":base\", \":api\"])\n"
    );
    let targets = analyze_starlark("BUILD", &src).unwrap();
    let app = targets.iter().find(|t| t.name.ends_with("app")).unwrap();
    let javac = app.actions.iter().find(|a| a.mnemonic == "Javac").unwrap();

    let argv = &javac.argv;
    let at = |flag: &str| argv.iter().position(|a| a == flag).unwrap();
    let compile_cp = &argv[at("--classpath") + 1..at("--runtime_classpath")];
    let runtime_cp = &argv[at("--runtime_classpath") + 1..at("--sources")];
    let p = |s: &str| format!("bazel-out/darwin_arm64-fastbuild/bin/{s}");

    // Compile classpath: BOTH deps' header jars (neverlink api included at compile time).
    assert!(compile_cp.contains(&p("libbase-hjar.jar")), "compile: {compile_cp:?}");
    assert!(compile_cp.contains(&p("libapi-hjar.jar")), "compile: {compile_cp:?}");
    // Runtime classpath: base's FULL jar; api EXCLUDED (neverlink). No cross-merge (no hjars here).
    assert_eq!(runtime_cp, [p("libbase.jar")], "runtime: {runtime_cp:?}");

    // api carries a compile jar but no runtime jar, and is flagged neverlink.
    let api = targets.iter().find(|t| t.name.ends_with("api")).unwrap();
    assert_eq!(api.compile_jars(), [p("libapi-hjar.jar")]);
    assert!(api.runtime_jars().is_empty() && api.neverlink());
}

#[test]
fn java_diamond_dedups_the_shared_transitive_jar() {
    // F1 regression: app -> [x, y] -> base. base's header jar reaches app via BOTH x and y; the
    // per-dep fold + the .bzl's dedup() must list it ONCE in app's compile classpath.
    let src = format!(
        "{JAVA_DEFS}\n\
         java_library(name = \"base\", srcs = [\"Base.java\"])\n\
         java_library(name = \"x\", srcs = [\"X.java\"], deps = [\":base\"])\n\
         java_library(name = \"y\", srcs = [\"Y.java\"], deps = [\":base\"])\n\
         java_library(name = \"app\", srcs = [\"App.java\"], deps = [\":x\", \":y\"])\n"
    );
    let targets = analyze_starlark("BUILD", &src).unwrap();
    let app = targets.iter().find(|t| t.name.ends_with("app")).unwrap();
    let javac = app.actions.iter().find(|a| a.mnemonic == "Javac").unwrap();
    let n = javac.argv.iter().filter(|a| a.ends_with("libbase-hjar.jar")).count();
    assert_eq!(n, 1, "diamond must dedup base's hjar (got {n}): {:?}", javac.argv);
}

#[test]
fn java_neverlink_prunes_transitive_runtime_through_the_rule() {
    // F14: app -> api(neverlink) -> hidden. api is compile-only; `hidden` (reachable only via api)
    // must be ABSENT from app's RUNTIME classpath, while api+hidden are present at COMPILE. (R1b
    // unit-tested the fold's subtree-prune; this pins it through java_defs.bzl + the dep resolution.)
    let src = format!(
        "{JAVA_DEFS}\n\
         java_library(name = \"hidden\", srcs = [\"Hidden.java\"])\n\
         java_library(name = \"api\", srcs = [\"Api.java\"], deps = [\":hidden\"], neverlink = True)\n\
         java_library(name = \"app\", srcs = [\"App.java\"], deps = [\":api\"])\n"
    );
    let targets = analyze_starlark("BUILD", &src).unwrap();
    let app = targets.iter().find(|t| t.name.ends_with("app")).unwrap();
    let javac = app.actions.iter().find(|a| a.mnemonic == "Javac").unwrap();
    let argv = &javac.argv;
    let at = |f: &str| argv.iter().position(|a| a == f).unwrap();
    let compile_cp = &argv[at("--classpath") + 1..at("--runtime_classpath")];
    let runtime_cp = &argv[at("--runtime_classpath") + 1..at("--sources")];
    let p = |s: &str| format!("bazel-out/darwin_arm64-fastbuild/bin/{s}");
    assert!(compile_cp.contains(&p("libapi-hjar.jar")), "compile sees neverlink api: {compile_cp:?}");
    assert!(compile_cp.contains(&p("libhidden-hjar.jar")), "compile sees api's dep: {compile_cp:?}");
    assert!(runtime_cp.is_empty(), "neverlink api prunes its whole subtree from runtime: {runtime_cp:?}");
}
