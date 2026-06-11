//! Fetch R1 (RazelFetchPlan §3): WORKSPACE spec extraction. `repository_rule` binds to a
//! RECORDER, the REAL repo-rule wrapper macros run (mirror expansion and all), and every
//! instantiation lands in the spec list — no network, no repository_ctx. Test-first.

use razel_loading::{AttrV, GlobalFlags, extract_workspace_repos};

fn write(path: &std::path::Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// A miniature TF shape: repo.bzl defines the rule via repository_rule + a wrapper macro
/// that post-processes urls (mirror-first); WORKSPACE declares repos directly AND through
/// a workspace2-style chain module; register_toolchains/workspace() are workspace-only
/// surface that must absorb.
#[test]
fn extracts_specs_through_the_real_wrapper_macros() {
    let root = std::env::temp_dir().join(format!("razel-fetch-extract-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    write(
        &root.join("third_party/repo.bzl"),
        r#"def _tf_http_archive_impl(ctx):
    pass

_tf_http_archive = repository_rule(
    implementation = _tf_http_archive_impl,
    attrs = {},
)

def tf_mirror_urls(url):
    return ["https://mirror.example/" + url, url]

def tf_http_archive(name, sha256, urls, strip_prefix = None, patch_file = None, link_files = None, **kwargs):
    _tf_http_archive(
        name = name,
        sha256 = sha256,
        urls = urls,
        strip_prefix = strip_prefix,
        patch_file = patch_file,
        link_files = link_files,
    )
"#,
    );
    write(
        &root.join("chain.bzl"),
        r#"load("//third_party:repo.bzl", "tf_http_archive", "tf_mirror_urls")

def workspace():
    tf_http_archive(
        name = "curl",
        sha256 = "feed0123",
        urls = tf_mirror_urls("https://curl.se/download/curl-8.tar.gz"),
        strip_prefix = "curl-8",
        patch_file = ["//third_party:curl.patch"],
        link_files = {"//third_party:curl.BUILD": "BUILD.bazel"},
    )
"#,
    );
    write(
        &root.join("WORKSPACE"),
        r#"workspace(name = "org_test")

load("//third_party:repo.bzl", "tf_http_archive", "tf_mirror_urls")

tf_http_archive(
    name = "rules_shell",
    sha256 = "abc123",
    urls = tf_mirror_urls("https://example.com/rules_shell-0.4.1.tar.gz"),
    strip_prefix = "rules_shell-0.4.1",
)

load("//:chain.bzl", workspace2 = "workspace")

workspace2()

register_toolchains("//toolchains:all")
"#,
    );
    let (specs, notes) = extract_workspace_repos(&root, GlobalFlags::default()).unwrap();
    let _ = std::fs::remove_dir_all(&root);
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["rules_shell", "curl"], "declaration order preserved: {notes:?}");
    let curl = specs.iter().find(|s| s.name == "curl").unwrap();
    let urls = curl.attrs.iter().find(|(k, _)| k == "urls").map(|(_, v)| v).unwrap();
    assert_eq!(
        urls,
        &AttrV::List(vec![
            "https://mirror.example/https://curl.se/download/curl-8.tar.gz".to_string(),
            "https://curl.se/download/curl-8.tar.gz".to_string()
        ]),
        "the REAL wrapper's mirror expansion must run"
    );
    let links = curl.attrs.iter().find(|(k, _)| k == "link_files").map(|(_, v)| v).unwrap();
    assert_eq!(
        links,
        &AttrV::Dict(vec![("//third_party:curl.BUILD".to_string(), "BUILD.bazel".to_string())])
    );
    let sha = curl.attrs.iter().find(|(k, _)| k == "sha256").map(|(_, v)| v).unwrap();
    assert_eq!(sha, &AttrV::Str("feed0123".to_string()));
}
