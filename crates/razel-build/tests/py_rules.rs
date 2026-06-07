//! Integration: a real Bazel workspace using `@rules_python` rules — a `py_binary`
//! imports a function from a cross-package `py_library`. razel resolves the loads to
//! its native python rules (no rules_python execution), builds the launcher, and the
//! launcher (PYTHONPATH = exec root) runs `python3` so `from lib.greeting import greet`
//! resolves. Mirrors the cc cross-package tutorial pattern in `razel-build`'s lib tests.

use razel_build::build_workspace;
use razel_exec::Cache;
use std::path::Path;

#[test]
fn py_binary_imports_a_cross_package_py_library() {
    // Guard like the cc tests' `/usr/bin/c++` check: skip where python3 is unavailable.
    if !Path::new("/usr/bin/python3").exists() {
        return;
    }

    let root = tempfile::tempdir().unwrap();
    let w = |rel: &str, body: &str| {
        let p = root.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };

    // lib package: a py_library exporting greeting.py (with a visibility kwarg).
    w(
        "lib/BUILD",
        r#"load("@rules_python//python:defs.bzl", "py_library")
py_library(name = "greeting", srcs = ["greeting.py"], visibility = ["//app:__pkg__"])
"#,
    );
    w(
        "lib/greeting.py",
        "def greet():\n    return \"hi from python\"\n",
    );

    // app package: a py_binary importing the lib's function via package-style import.
    w(
        "app/BUILD",
        r#"load("@rules_python//python:defs.bzl", "py_binary")
py_binary(name = "app", srcs = ["app.py"], main = "app.py", deps = ["//lib:greeting"])
"#,
    );
    w(
        "app/app.py",
        "from lib.greeting import greet\nprint(greet())\n",
    );

    let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

    // Cross-package: //lib:greeting is loaded on demand while analyzing //app:app.
    let report = build_workspace(root.path(), "//app:app", &cache).unwrap();
    assert!(
        report.produced.contains(&"app/app".to_string()),
        "launcher not produced: {:?}",
        report.produced
    );

    // Run the produced launcher: PYTHONPATH=exec root makes `lib.greeting` importable.
    let bin = root.path().join("app/app");
    assert!(bin.exists(), "razel did not produce the launcher");
    let out = std::process::Command::new(&bin).output().unwrap();
    assert!(
        out.status.success(),
        "launcher failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("hi from python"),
        "stdout: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}
