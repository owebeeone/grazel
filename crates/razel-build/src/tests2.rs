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

    #[test]
    fn label_native_and_autoconfig_helpers_evaluate() {
        // Label(), native.package_name(), and an auto-config helper (if_rocm_is_configured
        // → not configured) are exercised during BUILD eval; the cc_binary still builds
        // (gpu.cc is dropped because ROCm is "not configured").
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
load(\"@local_config_rocm//rocm:build_defs.bzl\", \"if_rocm_is_configured\")\n\
_lbl = Label(\"//pkg:prog\")\n\
_here = native.package_name()\n\
cc_binary(name = \"prog\", srcs = [\"main.cc\"] + if_rocm_is_configured([\"gpu.cc\"]))\n",
        );
        w(
            "pkg/main.cc",
            "#include <iostream>\nint main() { std::cout << \"ok\" << std::endl; return 0; }\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // gpu.cc doesn't exist; if the not-configured helper didn't drop it, this fails.
        let report = build_workspace(root.path(), "//pkg:prog", &cache).unwrap();
        assert!(report.produced.contains(&"pkg/prog".to_string()));
        let out = std::process::Command::new(root.path().join("pkg/prog"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");
    }

    #[test]
    fn stdlib_print_works_in_a_loaded_rule() {
        // Mirrors bazelbuild-examples rules/empty: a loaded .bzl rule whose impl
        // calls print() — exercises the standard-library globals (Print extension).
        let root = tempfile::tempdir().unwrap();
        let w = |rel: &str, body: &str| {
            let p = root.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        w(
            "pkg/empty.bzl",
            "def _impl(_):\n    print(\"this rule does nothing\")\nempty = rule(implementation = _impl)\n",
        );
        w(
            "pkg/BUILD",
            "load(\"//pkg:empty.bzl\", \"empty\")\nempty(name = \"nothing\")\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // No actions, no outputs — but it must analyze + "build" (0 executed) cleanly.
        let report = build_workspace(root.path(), "//pkg:nothing", &cache).unwrap();
        assert_eq!(report.executed, 0);
    }

    #[test]
    fn rule_writes_a_file_via_ctx_outputs_and_actions_write() {
        // Mirrors bazelbuild-examples rules/actions_write: attr.string/output schema,
        // ctx.outputs.<name>, and ctx.actions.write(content=) producing a real file.
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
            "pkg/defs.bzl",
            "def _impl(ctx):\n    ctx.actions.write(output = ctx.outputs.out, content = ctx.attr.content)\n\
gen = rule(implementation = _impl, attrs = {\"content\": attr.string(), \"out\": attr.output()})\n",
        );
        w(
            "pkg/BUILD",
            "load(\"//pkg:defs.bzl\", \"gen\")\ngen(name = \"hi\", content = \"hello there\", out = \"hi.txt\")\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        let report = build_workspace(root.path(), "//pkg:hi", &cache).unwrap();
        assert!(report.produced.contains(&"pkg/hi.txt".to_string()));
        assert_eq!(
            std::fs::read_to_string(root.path().join("pkg/hi.txt")).unwrap(),
            "hello there"
        );
    }

    #[test]
    fn depset_flattens_direct_and_transitive_deduped() {
        // AE primitive: depset(direct, transitive=[depset]).to_list() — folds direct +
        // transitive members, de-duplicated, order-preserving.
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
            "pkg/defs.bzl",
            "def _impl(ctx):\n    inner = depset([\"b\", \"c\"])\n    d = depset([\"a\", \"b\"], transitive = [inner])\n    ctx.actions.write(output = ctx.outputs.out, content = \",\".join(d.to_list()))\n\
gen = rule(implementation = _impl, attrs = {\"out\": attr.output()})\n",
        );
        w(
            "pkg/BUILD",
            "load(\"//pkg:defs.bzl\", \"gen\")\ngen(name = \"x\", out = \"x.txt\")\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        build_workspace(root.path(), "//pkg:x", &cache).unwrap();
        // direct [a,b], then transitive [b,c] → b deduped → a,b,c.
        assert_eq!(
            std::fs::read_to_string(root.path().join("pkg/x.txt")).unwrap(),
            "a,b,c"
        );
    }

    #[test]
    fn builds_via_a_rule_defined_in_a_loaded_bzl() {
        // The freeze-fix payoff: a custom `rule()` defined in a .bzl and load()ed.
        // Before RuleObj was freezable, freezing the loaded .bzl module errored here.
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
            "tools/rules.bzl",
            "def _impl(ctx):\n    out = ctx.attr.name\n    ctx.actions.run(executable = \"/bin/sh\", arguments = [\"-c\", \"echo built > \" + out], outputs = [out], inputs = [])\n    return [DefaultInfo(files = [out])]\nmyrule = rule(implementation = _impl, attrs = {})\n",
        );
        w(
            "app/BUILD",
            "load(\"//tools:rules.bzl\", \"myrule\")\nmyrule(name = \"thing\")\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        let report = build_workspace(root.path(), "//app:thing", &cache).unwrap();
        assert!(report.produced.contains(&"thing".to_string()));
        assert_eq!(
            std::fs::read_to_string(root.path().join("thing"))
                .unwrap()
                .trim(),
            "built"
        );
    }

    #[test]
    fn affected_query_splits_tests_and_deliverables() {
        // A lib and a test both consume widget.c; analysis-only (no toolchain needed).
        let src = r#"
def _impl(ctx):
    out = ctx.attr.name + ".o"
    ctx.actions.run(executable = "cc", outputs = [out], inputs = [ctx.attr.src], arguments = [])
    return [DefaultInfo(files = [out])]
thing = rule(implementation = _impl, attrs = {"src": 1})
thing(name = "widget", src = "widget.c")
thing(name = "widget_test", src = "widget.c")
"#;
        let labels = |v: &[AffectedTarget]| v.iter().map(|t| t.label.clone()).collect::<Vec<_>>();

        let a = affected(src, "pkg", &["widget.c".to_string()]).unwrap();
        assert_eq!(labels(&a.targets), vec!["//pkg:widget"]); // deliverable
        assert_eq!(labels(&a.tests), vec!["//pkg:widget_test"]); // test (by suffix)

        // An unrelated file impacts nothing — the rdep walk is output-sensitive.
        let none = affected(src, "pkg", &["other.c".to_string()]).unwrap();
        assert!(none.targets.is_empty() && none.tests.is_empty());
    }

    // The D7 path: a `define_config` transform generates the compile command; the rule
    // unpacks it into ctx.actions.run; razel executes it into a real object.
    const BUILD_TRANSFORM: &str = r#"
def _gnu_compile(req):
    return struct(
        executable = req.tool,
        args = ["-c", req.src, "-o", req.out],
        inputs = [req.src],
        outputs = [req.out],
    )

gnu = define_config(name = "gnu", compile = _gnu_compile)

def _impl(ctx):
    out = ctx.attr.name + ".o"
    spec = gnu.compile(struct(tool = "/usr/bin/cc", src = ctx.attr.src, out = out))
    ctx.actions.run(
        executable = spec.executable,
        arguments = spec.args,
        inputs = spec.inputs,
        outputs = spec.outputs,
    )
    return [DefaultInfo(files = spec.outputs)]

cc_obj = rule(implementation = _impl, attrs = {"src": 1})
cc_obj(name = "gadget", src = "gadget.c")
"#;

    #[test]
    fn compiles_via_a_define_config_transform() {
        if !Path::new("/usr/bin/cc").exists() {
            return;
        }
        let exec = tempfile::tempdir().unwrap();
        std::fs::write(exec.path().join("gadget.c"), "int g(void){return 7;}").unwrap();
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        let produced = build_target(BUILD_TRANSFORM, "gadget", exec.path(), &cache).unwrap();
        assert_eq!(produced, vec!["gadget.o"]);
        assert!(exec.path().join("gadget.o").exists());
        // The successful build exercised the define_config transform during analysis (select()
        // resolution); per-analysis configs live in the Session, not a global post-hoc query.
    }

    // A real cc_library: compile N sources, then archive into a static lib (multi-action,
    // single target — no cross-target deps yet).
    const BUILD_LIBRARY: &str = r#"
def _gnu_compile(req):
    return struct(executable = req.tool, args = ["-c", req.src, "-o", req.out],
                  inputs = [req.src], outputs = [req.out])

def _gnu_archive(req):
    return struct(executable = req.ar, args = ["rcs", req.out] + req.objs,
                  inputs = req.objs, outputs = [req.out])

gnu = define_config(name = "gnu", compile = _gnu_compile, archive = _gnu_archive)

def _cc_library_impl(ctx):
    objs = []
    for src in ctx.attr.srcs:
        o = src + ".o"
        c = gnu.compile(struct(tool = "/usr/bin/cc", src = src, out = o))
        ctx.actions.run(executable = c.executable, arguments = c.args, inputs = c.inputs, outputs = c.outputs)
        objs.append(o)
    lib = "lib" + ctx.attr.name + ".a"
    a = gnu.archive(struct(ar = "/usr/bin/ar", objs = objs, out = lib))
    ctx.actions.run(executable = a.executable, arguments = a.args, inputs = a.inputs, outputs = a.outputs)
    return [DefaultInfo(files = [lib])]

cc_library = rule(implementation = _cc_library_impl, attrs = {"srcs": 1})
cc_library(name = "math", srcs = ["add.c", "sub.c"])
"#;

    #[test]
    fn compiles_a_static_library_from_multiple_sources() {
        if !Path::new("/usr/bin/cc").exists() || !Path::new("/usr/bin/ar").exists() {
            return;
        }
        let exec = tempfile::tempdir().unwrap();
        std::fs::write(
            exec.path().join("add.c"),
            "int add(int a,int b){return a+b;}",
        )
        .unwrap();
        std::fs::write(
            exec.path().join("sub.c"),
            "int sub(int a,int b){return a-b;}",
        )
        .unwrap();
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        let produced = build_target(BUILD_LIBRARY, "math", exec.path(), &cache).unwrap();
        assert_eq!(produced, vec!["add.c.o", "sub.c.o", "libmath.a"]);
        let lib = exec.path().join("libmath.a");
        assert!(lib.exists(), "razel did not produce libmath.a");
        assert!(std::fs::metadata(&lib).unwrap().len() > 0);
    }

    // The two-phase milestone: cc_binary depends on cc_library, reads its DefaultInfo
    // (libmath.a), and LINKS it into a runnable binary — transitive build, deps first.
    const BUILD_BINARY: &str = r#"
def _gnu_compile(req):
    return struct(executable = req.tool, args = ["-c", req.src, "-o", req.out],
                  inputs = [req.src], outputs = [req.out])
def _gnu_archive(req):
    return struct(executable = req.ar, args = ["rcs", req.out] + req.objs,
                  inputs = req.objs, outputs = [req.out])
def _gnu_link(req):
    return struct(executable = req.cc, args = ["-o", req.out] + req.objs + req.libs,
                  inputs = req.objs + req.libs, outputs = [req.out])

gnu = define_config(name = "gnu", compile = _gnu_compile, archive = _gnu_archive, link = _gnu_link)

def _cc_library_impl(ctx):
    objs = []
    for src in ctx.attr.srcs:
        o = src + ".o"
        c = gnu.compile(struct(tool = "/usr/bin/cc", src = src, out = o))
        ctx.actions.run(executable = c.executable, arguments = c.args, inputs = c.inputs, outputs = c.outputs)
        objs.append(o)
    lib = "lib" + ctx.attr.name + ".a"
    a = gnu.archive(struct(ar = "/usr/bin/ar", objs = objs, out = lib))
    ctx.actions.run(executable = a.executable, arguments = a.args, inputs = a.inputs, outputs = a.outputs)
    return [DefaultInfo(files = [lib])]
cc_library = rule(implementation = _cc_library_impl, attrs = {"srcs": 1})

def _cc_binary_impl(ctx):
    o = ctx.attr.name + ".o"
    c = gnu.compile(struct(tool = "/usr/bin/cc", src = ctx.attr.src, out = o))
    ctx.actions.run(executable = c.executable, arguments = c.args, inputs = c.inputs, outputs = c.outputs)
    libs = []
    for d in ctx.attr.deps:
        libs = libs + d.files
    out = ctx.attr.name
    l = gnu.link(struct(cc = "/usr/bin/cc", objs = [o], libs = libs, out = out))
    ctx.actions.run(executable = l.executable, arguments = l.args, inputs = l.inputs, outputs = l.outputs)
    return [DefaultInfo(files = [out])]
cc_binary = rule(implementation = _cc_binary_impl, attrs = {"src": 1})

cc_library(name = "math", srcs = ["add.c"])
cc_binary(name = "app", src = "app.c", deps = [":math"])
"#;

    #[test]
    fn cc_binary_links_cc_library_into_a_runnable_binary() {
        if !Path::new("/usr/bin/cc").exists() || !Path::new("/usr/bin/ar").exists() {
            return;
        }
        let exec = tempfile::tempdir().unwrap();
        std::fs::write(
            exec.path().join("add.c"),
            "int add(int a,int b){return a+b;}",
        )
        .unwrap();
        // main returns add(40,2)-42 == 0; resolving `add` requires linking libmath.a.
        std::fs::write(
            exec.path().join("app.c"),
            "int add(int,int); int main(void){return add(40,2)-42;}",
        )
        .unwrap();
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // Build the binary: razel runs math's actions (lib) first, then app's (link).
        let produced = build_target(BUILD_BINARY, "app", exec.path(), &cache).unwrap();
        assert!(
            produced.contains(&"libmath.a".to_string()),
            "dep lib not built: {produced:?}"
        );
        assert!(
            produced.contains(&"app".to_string()),
            "binary not built: {produced:?}"
        );

        // The produced binary actually runs and links correctly (exit 0).
        let app = exec.path().join("app");
        assert!(app.exists());
        let status = std::process::Command::new(&app).status().unwrap();
        assert_eq!(status.code(), Some(0), "linked binary did not run/return 0");
    }

    /// RAZEL_BAZEL_BUILD_COMPAT: a cc_library→cc_binary dep chain (native rules) builds with
    /// EVERY output under `bazel-out/<config>/bin/…` (Bazel's layout), nothing in-tree, and the
    /// binary still links + runs — proving dep references resolve to the bazel-out paths. The
    /// dep's `libmath.a` is read from bazel-out by the link, because both its declared output
    /// and the binary's reference go through `qualify_output`.
    #[test]
    fn bazel_build_compat_puts_outputs_under_bazel_out_and_links() {
        if !Path::new("/usr/bin/cc").exists() {
            return;
        }
        let exec = tempfile::tempdir().unwrap();
        let src = "load(\"@rules_cc//cc:defs.bzl\", \"cc_library\", \"cc_binary\")\n\
             cc_library(name = \"math\", srcs = [\"add.c\"])\n\
             cc_binary(name = \"app\", srcs = [\"app.c\"], deps = [\":math\"])\n";
        std::fs::write(exec.path().join("add.c"), "int add(int a,int b){return a+b;}").unwrap();
        std::fs::write(
            exec.path().join("app.c"),
            "int add(int,int); int main(void){return add(40,2)-42;}",
        )
        .unwrap();
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();
        let flags = GlobalFlags { bazel_build_compat: true, ..Default::default() };
        let report = build_bazel_with(src, "app", exec.path(), &cache, flags).unwrap();
        // Every produced output carries the bazel-out/<config>/bin prefix — nothing in-tree.
        assert!(
            report.produced.iter().all(|p| p.starts_with("bazel-out/")),
            "outputs not all under bazel-out: {:?}",
            report.produced
        );
        let appout = report.default_outputs.first().expect("a default output");
        assert!(appout.contains("/bin/") && appout.ends_with("app"), "binary path: {appout}");
        // It physically lands there AND links (libmath.a resolved from bazel-out) + runs.
        let app = exec.path().join(appout);
        assert!(app.exists(), "binary missing at {}", app.display());
        assert!(!exec.path().join("app").exists(), "binary leaked in-tree");
        let status = std::process::Command::new(&app).status().unwrap();
        assert_eq!(status.code(), Some(0), "compat-built binary did not link/run");
    }

    /// Target-pattern expansion: `//...` spans packages, `//pkg:all` stays in its package
    /// (subpackages excluded), `//pkg/...` recurses, and a concrete label passes through.
    #[test]
    fn expand_pattern_spans_packages_and_excludes_subpackages() {
        let root = tempfile::tempdir().unwrap();
        let cc = "load(\"@rules_cc//cc:defs.bzl\", \"cc_library\")\n";
        std::fs::create_dir_all(root.path().join("a/b")).unwrap();
        std::fs::write(root.path().join("a/BUILD"), format!("{cc}cc_library(name = \"x\", srcs = [\"x.c\"])\n")).unwrap();
        std::fs::write(root.path().join("a/b/BUILD"), format!("{cc}cc_library(name = \"y\", srcs = [\"y.c\"])\n")).unwrap();
        let g = GlobalFlags::default;
        let has = |v: &[String], l: &str| v.iter().any(|x| x == l);

        let all = expand_pattern(root.path(), "//...", g()).unwrap();
        assert!(has(&all, "//a:x") && has(&all, "//a/b:y"), "//... spans packages: {all:?}");

        let a = expand_pattern(root.path(), "//a:all", g()).unwrap();
        assert!(has(&a, "//a:x") && !has(&a, "//a/b:y"), "//a:all excludes subpackage: {a:?}");

        let rec = expand_pattern(root.path(), "//a/...", g()).unwrap();
        assert!(has(&rec, "//a:x") && has(&rec, "//a/b:y"), "//a/... recurses: {rec:?}");

        // Concrete label → returned as-is, no discovery.
        assert_eq!(expand_pattern(root.path(), "//a:x", g()).unwrap(), vec!["//a:x".to_string()]);
    }

    /// S5x: the parallel executor. Diamond `base <- {a, b} <- top`. Each dependent CATs
    /// its deps' outputs, so a build that completes AT ALL proves deps ran first (cat fails
    /// on a missing input); `a` and `b` are independent (run concurrently at jobs>=2). The
    /// SAME graph at jobs=1 and jobs=4 must give a byte-identical report + outputs — the
    /// `-j1 == -jN` determinism bar.
    #[test]
    fn parallel_execute_respects_deps_and_is_deterministic_across_jobs() {
        use razel_loading::{AnalyzedAction, AnalyzedTarget};
        let act = |argv: &[&str], ins: &[&str], out: &str| AnalyzedAction {
            mnemonic: "Gen".into(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            inputs: ins.iter().map(|s| s.to_string()).collect(),
            outputs: vec![out.into()],
        };
        let t = |name: &str, deps: &[&str], a: AnalyzedAction, out: &str| AnalyzedTarget {
            name: name.into(),
            deps: deps.iter().map(|s| s.to_string()).collect(),
            actions: vec![a],
            default_info: vec![out.into()],
            ..Default::default()
        };
        let targets = vec![
            t("base", &[], act(&["/bin/sh", "-c", "echo base > base.txt"], &[], "base.txt"), "base.txt"),
            t("a", &["base"], act(&["/bin/sh", "-c", "cat base.txt > a.txt; echo a >> a.txt"], &["base.txt"], "a.txt"), "a.txt"),
            t("b", &["base"], act(&["/bin/sh", "-c", "cat base.txt > b.txt; echo b >> b.txt"], &["base.txt"], "b.txt"), "b.txt"),
            t("top", &["a", "b"], act(&["/bin/sh", "-c", "cat a.txt b.txt > top.txt"], &["a.txt", "b.txt"], "top.txt"), "top.txt"),
        ];
        let run = |jobs: usize| {
            let exec = tempfile::tempdir().unwrap();
            let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();
            let r = execute_jobs(&targets, "top", exec.path(), &cache, jobs).expect("build ok");
            let top = std::fs::read_to_string(exec.path().join("top.txt")).unwrap();
            (r.executed, r.produced, top)
        };
        let (e1, p1, top1) = run(1);
        let (e4, p4, top4) = run(4);
        // Deps respected (cat only succeeds if inputs exist first) — same content either way.
        assert_eq!(top1, "base\na\nbase\nb\n", "ordering/content: {top1:?}");
        // -j1 == -jN: identical executed count, produced list, and output bytes.
        assert_eq!(e1, 4, "fresh cache executes all four");
        assert_eq!(e4, 4, "parallel executes all four");
        assert_eq!(p1, p4, "produced list deterministic across jobs: {p1:?} vs {p4:?}");
        assert_eq!(top1, top4, "output byte-identical across jobs");
    }
