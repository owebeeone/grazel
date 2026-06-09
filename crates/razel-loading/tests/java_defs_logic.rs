//! B3 — the java spike. razel's `java:defs.bzl` produces java's THREE-action structure (Turbine
//! header-jar + Javac + JavaSourceJar) over the existing machinery, with the ORDERED compile
//! classpath (`dep.compile_jars` — the OrderedDepset fold, B2). Proven structurally via
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
    // dep.compile_jars OrderedDepset fold — B2), not base's full jar.
    let base_hjar = "bazel-out/darwin_arm64-fastbuild/bin/libbase-hjar.jar";
    assert!(javac.argv.contains(&base_hjar.to_string()), "classpath missing base hjar: {:?}", javac.argv);
    assert!(javac.inputs.contains(&base_hjar.to_string()));
    assert_eq!(javac.outputs, ["bazel-out/darwin_arm64-fastbuild/bin/libutil.jar"]);

    // JavaInfo captured util's OWN header jar (the OrderedDepset element dependents fold).
    assert_eq!(util.compile_jars, ["bazel-out/darwin_arm64-fastbuild/bin/libutil-hjar.jar"]);
}
