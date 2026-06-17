//! P5.2 (§13/R6): query traversal reachability into the vendored `@rules_rust//rust/platform` +
//! `@platforms` slice. `deps()` over a `@crates`-shaped `target_compatible_with` select leaves the
//! workspace into the slice (loaded via `host_build`, P5.0) and reaches the condition + default-arm
//! nodes with their real `rule_class` — kind correctness, not just reachability.

use razel_query::{GlobalFlags, Output, run};

#[test]
fn p52_deps_traverses_into_the_vendored_platform_slice_with_correct_kind() {
    let tmp = std::env::temp_dir().join(format!("razel-p52-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let app = tmp.join("app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(tmp.join("MODULE.bazel"), "").unwrap();
    std::fs::write(app.join("lib.rs"), "pub fn x() {}\n").unwrap();
    // The blake3 pattern (227/227 generated `@crates` targets carry it): host-triple → compatible,
    // default → `@platforms//:incompatible`.
    std::fs::write(
        app.join("BUILD"),
        "load(\"@rules_rust//rust:defs.bzl\", \"rust_library\")\n\
         rust_library(name = \"t\", srcs = [\"lib.rs\"], target_compatible_with = select({\n\
             \"@rules_rust//rust/platform:aarch64-apple-darwin\": [],\n\
             \"//conditions:default\": [\"@platforms//:incompatible\"],\n\
         }))\n",
    )
    .unwrap();

    let out = run(&tmp, GlobalFlags::default(), "deps(//app:t)", Output::LabelKind, false)
        .unwrap_or_else(|e| panic!("query deps(//app:t): {e}"));
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        out.contains("config_setting rule @rules_rust//rust/platform:aarch64-apple-darwin"),
        "deps() reaches the host-triple config_setting NODE with correct kind:\n{out}"
    );
    assert!(
        out.contains("constraint_value rule @platforms//:incompatible"),
        "deps() reaches @platforms//:incompatible NODE with correct kind:\n{out}"
    );
}
