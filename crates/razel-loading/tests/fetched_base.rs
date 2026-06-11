//! Fetch R4 (RazelFetchPlan §3): the FETCHED external root (`_razel_<user>/<hash>/external`)
//! is a SECOND resolution base — consulted after the hand-vendored `third-party/` (curated
//! overlays win until deliberately retired). Test-first.

use razel_loading::{GlobalFlags, analyze_workspace_with};

fn write(path: &std::path::Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// Upstream flatbuffers' BUILD.bazel (the fetched, bazel-faithful copy) declares java and
/// android targets via Bazel AUTOLOADED globals and an `@build_bazel_rules_android` load —
/// surface the hand-vendored curated BUILD used to mask. The package must LOAD (cc targets
/// resolve); the java/android declarations are placeholders nothing in the TF cone deps.
#[test]
fn upstream_build_with_autoload_java_and_android_loads() {
    let root = std::env::temp_dir().join(format!("razel-autoload-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let ws = root.join("ws");
    let fetched = root.join("fetched");
    write(
        &fetched.join("fb/BUILD.bazel"),
        "load(\"@build_bazel_rules_android//android:rules.bzl\", \"android_library\")\n\
         filegroup(name = \"runtime_srcs\", srcs = [\"r.h\"])\n\
         java_library(name = \"runtime_java\")\n\
         android_library(name = \"runtime_android\")\n",
    );
    write(&fetched.join("fb/r.h"), "");
    write(&ws.join("a/BUILD"), "filegroup(name = \"a\", srcs = [\"@fb//:runtime_srcs\"])\n");
    let mut flags = GlobalFlags::default();
    flags.fetched_external_base = Some(fetched);
    let targets = analyze_workspace_with(&ws, "//a:a", flags).unwrap();
    let a = targets.iter().find(|t| t.name == "//a:a").unwrap();
    assert!(
        a.default_info.iter().any(|f| f == "external/fb/r.h"),
        "the cc-side of an upstream BUILD must serve through the java/android noise: {:?}",
        a.default_info
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn fetched_repos_resolve_and_vendored_wins() {
    let root = std::env::temp_dir().join(format!("razel-fetched-base-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let ws = root.join("ws");
    let vendored = root.join("vendored");
    let fetched = root.join("fetched");
    std::fs::create_dir_all(&vendored).unwrap();
    // @fr exists ONLY in the fetched root.
    write(&fetched.join("fr/BUILD"), "filegroup(name = \"lib\", srcs = [\"x.txt\"])\n");
    write(&fetched.join("fr/x.txt"), "x");
    // @both exists in BOTH — the vendored copy must win (different src name proves which).
    write(&vendored.join("both/BUILD"), "filegroup(name = \"lib\", srcs = [\"v.txt\"])\n");
    write(&vendored.join("both/v.txt"), "v");
    write(&fetched.join("both/BUILD"), "filegroup(name = \"lib\", srcs = [\"f.txt\"])\n");
    write(&fetched.join("both/f.txt"), "f");
    write(
        &ws.join("a/BUILD"),
        "filegroup(name = \"a\", srcs = [\"@fr//:lib\", \"@both//:lib\"])\n",
    );
    let mut flags = GlobalFlags::default();
    flags.external_base = Some(vendored);
    flags.fetched_external_base = Some(fetched);
    let targets = analyze_workspace_with(&ws, "//a:a", flags).unwrap();
    let a = targets.iter().find(|t| t.name == "//a:a").unwrap();
    let files = &a.default_info;
    assert!(
        files.iter().any(|f| f == "external/fr/x.txt"),
        "fetched-only repo must resolve: {files:?}"
    );
    assert!(
        files.iter().any(|f| f == "external/both/v.txt"),
        "vendored copy must win over fetched: {files:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
