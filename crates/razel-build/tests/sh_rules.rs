//! Integration test: `load("@rules_shell//shell:sh_binary.bzl", "sh_binary")` builds a
//! runnable shell script via razel's native sh rules.

use razel_build::build_workspace;
use razel_exec::Cache;
use std::path::Path;

#[test]
fn builds_and_runs_a_sh_binary() {
    if !Path::new("/bin/sh").exists() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let w = |rel: &str, body: &str| {
        let p = root.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    w(
        "pkg/BUILD",
        "load(\"@rules_shell//shell:sh_binary.bzl\", \"sh_binary\")\nsh_binary(name = \"hello\", srcs = [\"hello.sh\"])\n",
    );
    w("pkg/hello.sh", "#!/bin/sh\necho \"hi from sh\"\n");

    let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();
    let report = build_workspace(root.path(), "//pkg:hello", &cache).unwrap();
    assert!(
        report.produced.contains(&"pkg/hello".to_string()),
        "produced: {:?}",
        report.produced
    );

    let out = std::process::Command::new(root.path().join("pkg/hello"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("hi from sh"),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}
