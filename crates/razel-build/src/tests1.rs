#![cfg(test)]
    use super::*;

    const BUILD: &str = r#"
def _impl(ctx):
    out = ctx.attr.name + ".o"
    ctx.actions.run(
        executable = "/usr/bin/cc",
        outputs = [out],
        inputs = [ctx.attr.src],
        arguments = ["-c", ctx.attr.src, "-o", out],
    )
    return [DefaultInfo(files = [out])]

cc_obj = rule(implementation = _impl, attrs = {"src": 1})
cc_obj(name = "widget", src = "widget.c")
"#;

    /// S4: a build's OUTPUTS are the requested target's DefaultInfo — never the
    /// intermediates (`run` execs outputs[0]; a cc_binary's first PRODUCED file is
    /// a .o, its DefaultInfo is the linked binary).
    #[test]
    fn report_default_outputs_are_the_targets_default_info() {
        use razel_loading::{AnalyzedAction, AnalyzedTarget};
        let exec = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(cache_dir.path()).unwrap();
        let touch = |out: &str| AnalyzedAction {
            mnemonic: "Touch".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), format!("touch {out}")],
            inputs: vec![],
            outputs: vec![out.into()],
            description: String::new(),
        };
        let targets = vec![
            AnalyzedTarget {
                name: "//p:dep".into(),
                deps: vec![],
                actions: vec![touch("dep.txt")],
                default_info: vec!["dep.txt".into()],
                providers: Default::default(),
            },
            AnalyzedTarget {
                name: "//p:bin".into(),
                deps: vec!["//p:dep".into()],
                actions: vec![touch("a.o"), touch("bin")],
                default_info: vec!["bin".into()],
                providers: Default::default(),
            },
        ];
        let report = execute(&targets, "//p:bin", exec.path(), &cache).unwrap();
        assert_eq!(report.default_outputs, vec!["bin".to_string()]);
        assert_eq!(report.produced, vec!["dep.txt", "a.o", "bin"], "intermediates stay in produced");
    }

    #[test]
    fn compiles_a_real_object_through_the_rule_engine() {
        if !Path::new("/usr/bin/cc").exists() {
            return; // skip where no cc
        }
        let exec = tempfile::tempdir().unwrap();
        std::fs::write(exec.path().join("widget.c"), "int answer(void){return 42;}").unwrap();
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // First build: rule impl runs → emits a cc compile action → razel executes it.
        let produced = build_target(BUILD, "widget", exec.path(), &cache).unwrap();
        assert_eq!(produced, vec!["widget.o"]);
        let obj = exec.path().join("widget.o");
        assert!(obj.exists(), "razel did not produce widget.o");
        assert!(std::fs::metadata(&obj).unwrap().len() > 0);

        // Second build in a fresh exec root: cache hit restores the object (0 exec).
        let exec2 = tempfile::tempdir().unwrap();
        std::fs::write(
            exec2.path().join("widget.c"),
            "int answer(void){return 42;}",
        )
        .unwrap();
        let produced2 = build_target(BUILD, "widget", exec2.path(), &cache).unwrap();
        assert_eq!(produced2, vec!["widget.o"]);
        assert!(exec2.path().join("widget.o").exists());
    }

    #[test]
    fn report_counts_executed_then_zero_on_cache_hit() {
        if !Path::new("/usr/bin/cc").exists() {
            return;
        }
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // Cold build: the one compile action executes (a cache miss).
        let exec = tempfile::tempdir().unwrap();
        std::fs::write(exec.path().join("widget.c"), "int answer(void){return 42;}").unwrap();
        let r1 = build_target_report(BUILD, "widget", exec.path(), &cache).unwrap();
        assert_eq!(r1.produced, vec!["widget.o"]);
        assert_eq!(r1.executed, 1, "cold build executes the action");

        // Warm rebuild in a fresh exec root: same content key → cache hit → 0 executed.
        let exec2 = tempfile::tempdir().unwrap();
        std::fs::write(
            exec2.path().join("widget.c"),
            "int answer(void){return 42;}",
        )
        .unwrap();
        let r2 = build_target_report(BUILD, "widget", exec2.path(), &cache).unwrap();
        assert_eq!(r2.produced, vec!["widget.o"]);
        assert_eq!(
            r2.executed, 0,
            "warm rebuild is fully cached — nothing recomputed"
        );
    }

    // The cpp-tutorial stage1 BUILD, verbatim — a real Bazel BUILD with a load().
    const CPP_TUTORIAL_STAGE1: &str = r#"
load("@rules_cc//cc:cc_binary.bzl", "cc_binary")

cc_binary(
    name = "hello-world",
    srcs = ["hello-world.cc"],
)
"#;

    #[test]
    fn builds_and_runs_real_bazel_cpp_tutorial_stage1() {
        if !Path::new("/usr/bin/c++").exists() {
            return; // no C++ driver
        }
        let exec = tempfile::tempdir().unwrap();
        // The actual stage1 source (std::iostream/string/ctime; no deps/headers).
        std::fs::write(
            exec.path().join("hello-world.cc"),
            r#"#include <iostream>
#include <string>
int main(int argc, char** argv) {
    std::string who = argc > 1 ? argv[1] : "world";
    std::cout << "Hello " << who << std::endl;
    return 0;
}
"#,
        )
        .unwrap();
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // razel loads the real BUILD (resolving @rules_cc → native cc_binary) and builds.
        let report = build_bazel(CPP_TUTORIAL_STAGE1, "hello-world", exec.path(), &cache).unwrap();
        assert_eq!(report.produced, vec!["hello-world.cc.o", "hello-world"]); // compile + link outputs
        assert_eq!(report.executed, 2, "compile + link");

        // The produced binary runs and prints the greeting.
        let bin = exec.path().join("hello-world");
        assert!(bin.exists(), "razel did not produce the binary");
        let out = std::process::Command::new(&bin).output().unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Hello world");
    }

    // cpp-tutorial stage2, verbatim: a cc_library + a cc_binary depending on it.
    const CPP_TUTORIAL_STAGE2: &str = r#"
load("@rules_cc//cc:cc_binary.bzl", "cc_binary")
load("@rules_cc//cc:cc_library.bzl", "cc_library")

cc_library(
    name = "hello-greet",
    srcs = ["hello-greet.cc"],
    hdrs = ["hello-greet.h"],
)

cc_binary(
    name = "hello-world",
    srcs = ["hello-world.cc"],
    deps = [
        ":hello-greet",
    ],
)
"#;

    #[test]
    fn builds_and_runs_real_bazel_cpp_tutorial_stage2() {
        if !Path::new("/usr/bin/c++").exists() || !Path::new("/usr/bin/ar").exists() {
            return;
        }
        let exec = tempfile::tempdir().unwrap();
        let w = |name: &str, body: &str| std::fs::write(exec.path().join(name), body).unwrap();
        w(
            "hello-greet.h",
            "#ifndef HELLO_GREET_H_\n#define HELLO_GREET_H_\n#include <string>\nstd::string get_greet(const std::string& who);\n#endif\n",
        );
        w(
            "hello-greet.cc",
            "#include \"hello-greet.h\"\nstd::string get_greet(const std::string& who) { return \"Hello \" + who; }\n",
        );
        w(
            "hello-world.cc",
            "#include \"hello-greet.h\"\n#include <iostream>\nint main(int argc, char** argv) {\n  std::string who = argc > 1 ? argv[1] : \"world\";\n  std::cout << get_greet(who) << std::endl;\n  return 0;\n}\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        let report = build_bazel(CPP_TUTORIAL_STAGE2, "hello-world", exec.path(), &cache).unwrap();
        // deps-first: compile+archive the lib, then compile+link the binary.
        assert_eq!(report.executed, 4, "2 compiles + archive + link");
        assert!(report.produced.contains(&"libhello-greet.a".to_string()));
        assert!(report.produced.contains(&"hello-world".to_string()));

        let out = std::process::Command::new(exec.path().join("hello-world"))
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Hello world");
    }

    #[test]
    fn builds_and_runs_real_bazel_cpp_tutorial_stage3() {
        if !Path::new("/usr/bin/c++").exists() || !Path::new("/usr/bin/ar").exists() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let w = |rel: &str, body: &str| {
            let p = root.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        // lib package
        w(
            "lib/BUILD",
            r#"load("@rules_cc//cc:cc_library.bzl", "cc_library")
cc_library(name = "hello-time", srcs = ["hello-time.cc"], hdrs = ["hello-time.h"], visibility = ["//main:__pkg__"])
"#,
        );
        w(
            "lib/hello-time.h",
            "#ifndef LIB_HELLO_TIME_H_\n#define LIB_HELLO_TIME_H_\nvoid print_localtime();\n#endif\n",
        );
        w(
            "lib/hello-time.cc",
            "#include \"lib/hello-time.h\"\n#include <ctime>\n#include <iostream>\nvoid print_localtime() { std::time_t t = std::time(nullptr); std::cout << std::asctime(std::localtime(&t)); }\n",
        );
        // main package — depends on //lib:hello-time AND :hello-greet
        w(
            "main/BUILD",
            r#"load("@rules_cc//cc:cc_binary.bzl", "cc_binary")
load("@rules_cc//cc:cc_library.bzl", "cc_library")
cc_library(name = "hello-greet", srcs = ["hello-greet.cc"], hdrs = ["hello-greet.h"])
cc_binary(name = "hello-world", srcs = ["hello-world.cc"], deps = [":hello-greet", "//lib:hello-time"])
"#,
        );
        w(
            "main/hello-greet.h",
            "#ifndef MAIN_HELLO_GREET_H_\n#define MAIN_HELLO_GREET_H_\n#include <string>\nstd::string get_greet(const std::string& who);\n#endif\n",
        );
        w(
            "main/hello-greet.cc",
            "#include \"main/hello-greet.h\"\nstd::string get_greet(const std::string& who) { return \"Hello \" + who; }\n",
        );
        w(
            "main/hello-world.cc",
            "#include \"lib/hello-time.h\"\n#include \"main/hello-greet.h\"\n#include <iostream>\nint main(int argc, char** argv) {\n  std::string who = argc > 1 ? argv[1] : \"world\";\n  std::cout << get_greet(who) << std::endl;\n  print_localtime();\n  return 0;\n}\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // Cross-package: //lib:hello-time is loaded on demand while analyzing main.
        let report = build_workspace(root.path(), "//main:hello-world", &cache).unwrap();
        assert_eq!(report.executed, 6, "3 compiles + 2 archives + 1 link");
        assert!(report.produced.contains(&"main/hello-world".to_string()));
        assert!(report.produced.contains(&"lib/libhello-time.a".to_string()));

        let out = std::process::Command::new(root.path().join("main/hello-world"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).starts_with("Hello world"),
            "stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    #[test]
    fn builds_a_bazel_target_with_glob_srcs() {
        if !Path::new("/usr/bin/c++").exists() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let w = |rel: &str, body: &str| {
            let p = root.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        // srcs via glob — the rule never names the files; razel scans the package dir.
        w(
            "app/BUILD",
            "load(\"@rules_cc//cc:cc_binary.bzl\", \"cc_binary\")\ncc_binary(name = \"prog\", srcs = glob([\"*.cc\"]))\n",
        );
        w(
            "app/main.cc",
            "#include <iostream>\nconst char* who();\nint main() { std::cout << \"Hello \" << who() << std::endl; return 0; }\n",
        );
        w("app/who.cc", "const char* who() { return \"world\"; }\n");
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        let report = build_workspace(root.path(), "//app:prog", &cache).unwrap();
        assert_eq!(
            report.executed, 3,
            "glob found 2 srcs → 2 compiles + 1 link"
        );
        assert!(report.produced.contains(&"app/prog".to_string()));

        let out = std::process::Command::new(root.path().join("app/prog"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Hello world");
    }

    #[test]
    fn copts_and_propagated_defines_reach_the_compiler() {
        if !Path::new("/usr/bin/c++").exists() || !Path::new("/usr/bin/ar").exists() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let w = |rel: &str, body: &str| {
            let p = root.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        // lib exports a define; the binary inherits it (propagation) and adds its own
        // via copts (local). main computes LIBVAL + BINVAL.
        w(
            "lib/BUILD",
            "load(\"@rules_cc//cc:cc_library.bzl\", \"cc_library\")\ncc_library(name = \"v\", srcs = [\"v.cc\"], defines = [\"LIBVAL=10\"])\n",
        );
        w("lib/v.cc", "int v_unused() { return 0; }\n");
        w(
            "app/BUILD",
            "load(\"@rules_cc//cc:cc_binary.bzl\", \"cc_binary\")\ncc_binary(name = \"prog\", srcs = [\"main.cc\"], deps = [\"//lib:v\"], copts = [\"-DBINVAL=5\"])\n",
        );
        w(
            "app/main.cc",
            "#include <iostream>\nint main() { std::cout << (LIBVAL + BINVAL) << std::endl; return 0; }\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // Without copts/defines wired, main.cc wouldn't compile (LIBVAL/BINVAL undefined).
        build_workspace(root.path(), "//app:prog", &cache).unwrap();
        let out = std::process::Command::new(root.path().join("app/prog"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "15");
    }

    #[test]
    fn global_copts_reach_every_compile() {
        if !Path::new("/usr/bin/c++").exists() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("BUILD"),
            "load(\"@rules_cc//cc:cc_binary.bzl\", \"cc_binary\")\ncc_binary(name = \"prog\", srcs = [\"main.cc\"])\n",
        )
        .unwrap();
        // No -DANSWER in the BUILD — it must arrive via the global flag (CLI -c/--copt).
        std::fs::write(
            root.path().join("main.cc"),
            "#include <iostream>\nint main() { std::cout << ANSWER << std::endl; return 0; }\n",
        )
        .unwrap();
        let build_src = std::fs::read_to_string(root.path().join("BUILD")).unwrap();
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        let flags = GlobalFlags {
            copts: vec!["-DANSWER=42".into()],
            linkopts: vec![],
            ..Default::default()
        };
        build_bazel_with(&build_src, "prog", root.path(), &cache, flags).unwrap();

        let out = std::process::Command::new(root.path().join("prog"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "42");
    }

    #[test]
    fn builds_via_a_custom_bzl_macro() {
        if !Path::new("/usr/bin/c++").exists() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let w = |rel: &str, body: &str| {
            let p = root.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        // A repo-defined macro in its own .bzl — razel evaluates it (not just resolves
        // @rules_cc), so the BUILD can call cc_app() which wraps cc_binary.
        w(
            "tools/defs.bzl",
            "load(\"@rules_cc//cc:cc_binary.bzl\", \"cc_binary\")\ndef cc_app(name, srcs):\n    cc_binary(name = name, srcs = srcs)\n",
        );
        w(
            "app/BUILD",
            "load(\"//tools:defs.bzl\", \"cc_app\")\ncc_app(name = \"prog\", srcs = [\"main.cc\"])\n",
        );
        w(
            "app/main.cc",
            "#include <iostream>\nint main() { std::cout << \"Hello world\" << std::endl; return 0; }\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        let report = build_workspace(root.path(), "//app:prog", &cache).unwrap();
        assert_eq!(
            report.executed, 2,
            "macro expanded to a cc_binary: compile + link"
        );
        assert!(report.produced.contains(&"app/prog".to_string()));

        let out = std::process::Command::new(root.path().join("app/prog"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Hello world");
    }

    #[test]
    fn build_package_declaration_builtins_are_no_ops() {
        // package()/package_group()/licenses()/exports_files() are recognized so real
        // BUILD files evaluate; razel treats them as no-op declarations.
        if !Path::new("/usr/bin/c++").exists() {
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
            "load(\"@rules_cc//cc:cc_binary.bzl\", \"cc_binary\")\n\
package(default_visibility = [\"//visibility:public\"], licenses = [\"notice\"])\n\
licenses([\"notice\"])\n\
exports_files([\"data.txt\"], visibility = [\"//visibility:public\"])\n\
package_group(name = \"friends\", packages = [\"//...\"])\n\
cc_binary(name = \"prog\", srcs = [\"main.cc\"])\n",
        );
        w(
            "pkg/main.cc",
            "#include <iostream>\nint main() { std::cout << \"ok\" << std::endl; return 0; }\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // The no-op builtins must not derail evaluation; the cc_binary still builds.
        let report = build_workspace(root.path(), "//pkg:prog", &cache).unwrap();
        assert!(report.produced.contains(&"pkg/prog".to_string()));
        let out = std::process::Command::new(root.path().join("pkg/prog"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");
    }

    #[test]
    fn recognizes_build_graph_and_skylib_builtins() {
        // config_setting/filegroup/alias/test_suite + skylib (bzl_library/string_flag)
        // are recognized so a real BUILD evaluates; the cc_binary alongside still builds.
        if !Path::new("/usr/bin/c++").exists() {
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
            "load(\"@rules_cc//cc:cc_binary.bzl\", \"cc_binary\")\n\
load(\"@bazel_skylib//:bzl_library.bzl\", \"bzl_library\")\n\
load(\"@bazel_skylib//rules:common_settings.bzl\", \"string_flag\")\n\
config_setting(name = \"dbg\", values = {\"compilation_mode\": \"dbg\"})\n\
filegroup(name = \"data\", srcs = [\"a.txt\"])\n\
alias(name = \"prog_alias\", actual = \":prog\")\n\
test_suite(name = \"all_tests\", tests = [])\n\
string_flag(name = \"mode\", build_setting_default = \"x\")\n\
bzl_library(name = \"defs_lib\", srcs = [\"defs.bzl\"])\n\
cc_binary(name = \"prog\", srcs = [\"main.cc\"])\n",
        );
        w(
            "pkg/main.cc",
            "#include <iostream>\nint main() { std::cout << \"ok\" << std::endl; return 0; }\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        let report = build_workspace(root.path(), "//pkg:prog", &cache).unwrap();
        assert!(report.produced.contains(&"pkg/prog".to_string()));
        let out = std::process::Command::new(root.path().join("pkg/prog"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");
    }

