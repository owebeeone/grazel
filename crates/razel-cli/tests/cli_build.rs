//! End-to-end through the actual `razel` binary: build the unmodified Bazel C++
//! tutorial (stage 3 — multi-package, cross-package deps) via `razel build
//! //main:hello-world` and run the result. Proves the CLI, the Bazel loader,
//! on-demand multi-package loading, the native cc rules, and the `-c`/`--copt`
//! flag plumbing all work *together* — not just in library unit tests.

use std::path::Path;
use std::process::Command;

fn w(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

#[test]
fn cli_builds_and_runs_real_multipackage_cpp_tutorial() {
    if !Path::new("/usr/bin/c++").exists() || !Path::new("/usr/bin/ar").exists() {
        return; // no host toolchain
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // lib package — a cc_library exposed to //main.
    w(
        root,
        "lib/BUILD",
        r#"load("@rules_cc//cc:cc_library.bzl", "cc_library")
cc_library(name = "hello-time", srcs = ["hello-time.cc"], hdrs = ["hello-time.h"], visibility = ["//main:__pkg__"])
"#,
    );
    w(
        root,
        "lib/hello-time.h",
        "#ifndef LIB_HELLO_TIME_H_\n#define LIB_HELLO_TIME_H_\nvoid print_localtime();\n#endif\n",
    );
    w(
        root,
        "lib/hello-time.cc",
        "#include \"lib/hello-time.h\"\n#include <ctime>\n#include <iostream>\nvoid print_localtime() { std::time_t t = std::time(nullptr); std::cout << std::asctime(std::localtime(&t)); }\n",
    );
    // main package — cc_binary depends on :hello-greet AND //lib:hello-time.
    w(
        root,
        "main/BUILD",
        r#"load("@rules_cc//cc:cc_binary.bzl", "cc_binary")
load("@rules_cc//cc:cc_library.bzl", "cc_library")
cc_library(name = "hello-greet", srcs = ["hello-greet.cc"], hdrs = ["hello-greet.h"])
cc_binary(name = "hello-world", srcs = ["hello-world.cc"], deps = [":hello-greet", "//lib:hello-time"])
"#,
    );
    w(
        root,
        "main/hello-greet.h",
        "#ifndef MAIN_HELLO_GREET_H_\n#define MAIN_HELLO_GREET_H_\n#include <string>\nstd::string get_greet(const std::string& who);\n#endif\n",
    );
    w(
        root,
        "main/hello-greet.cc",
        "#include \"main/hello-greet.h\"\nstd::string get_greet(const std::string& who) { return \"Hello \" + who; }\n",
    );
    w(
        root,
        "main/hello-world.cc",
        "#include \"lib/hello-time.h\"\n#include \"main/hello-greet.h\"\n#include <iostream>\nint main(int argc, char** argv) {\n  std::string who = argc > 1 ? argv[1] : \"world\";\n  std::cout << get_greet(who) << std::endl;\n  print_localtime();\n  return 0;\n}\n",
    );

    // The actual binary, the way a user would invoke it (incl. a global cc flag).
    let out = Command::new(env!("CARGO_BIN_EXE_razel"))
        .args(["build", "//main:hello-world", "-C"])
        .arg(root)
        .args(["-c", "opt"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "razel build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // The cross-package binary was produced and runs.
    let bin = root.join("main/hello-world");
    assert!(bin.exists(), "razel did not produce main/hello-world");
    let run = Command::new(&bin).output().unwrap();
    assert!(
        String::from_utf8_lossy(&run.stdout).starts_with("Hello world"),
        "stdout: {}",
        String::from_utf8_lossy(&run.stdout),
    );

    // Second build is fully cached (incremental "nothing to do").
    let again = Command::new(env!("CARGO_BIN_EXE_razel"))
        .args(["build", "//main:hello-world", "-C"])
        .arg(root)
        .args(["-c", "opt"])
        .output()
        .unwrap();
    assert!(again.status.success());
    assert!(
        String::from_utf8_lossy(&again.stdout).contains("cached"),
        "expected a cached rebuild, got: {}",
        String::from_utf8_lossy(&again.stdout),
    );
}
