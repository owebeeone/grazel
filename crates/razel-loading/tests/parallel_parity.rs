//! P4a: the worker pool must produce SEQUENTIAL results at any thread count. The round-23
//! diagnosis: per-eval-stack Session state (current_pkg / current_bzl_repo / the in-flight
//! target / analyzing) was Session-wide, so concurrent evals clobbered each other's package
//! context and labels misresolved. This fixture is deliberately HETEROGENEOUS (unique target
//! and file names per package) so any cross-thread context bleed is a hard failure, never a
//! coincidental hit. Test-first (AGENTS.md).

use razel_loading::{GlobalFlags, load_tree_report_with_threads};

/// N packages: per-package unique source file (qualify() exercise), a Starlark provider rule
/// (freeze-and-harvest under threads), a filegroup (file-label resolution), and a consumer
/// depping the package's own lib + a cross-package lib (demand loads + cross-thread waits +
/// cross-package provider reads).
fn write_fixture(root: &std::path::Path, n: usize) {
    std::fs::create_dir_all(root.join("defs")).unwrap();
    std::fs::write(
        root.join("defs/info.bzl"),
        r#"MyInfo = provider(fields = ["msg"])

def _lib_impl(ctx):
    return [MyInfo(msg = "lib-" + ctx.attr.tag)]

lib = rule(implementation = _lib_impl, attrs = {"tag": attr.string()})

def _use_impl(ctx):
    msgs = []
    for d in ctx.attr.deps:
        msgs.append(d[MyInfo].msg)
    ctx.actions.run(executable = "tool", outputs = [], inputs = [], arguments = msgs)

use = rule(implementation = _use_impl, attrs = {"deps": attr.label_list()})
"#,
    )
    .unwrap();
    std::fs::write(root.join("defs/BUILD"), "").unwrap();
    for i in 0..n {
        let pkg = root.join(format!("pkg{i}"));
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join(format!("src{i}.txt")), "x").unwrap();
        let j = (i + 3) % n;
        std::fs::write(
            pkg.join("BUILD"),
            format!(
                r#"load("//defs:info.bzl", "lib", "use")
filegroup(name = "files{i}", srcs = ["src{i}.txt"])
lib(name = "lib{i}", tag = "pkg{i}")
use(name = "use{i}", deps = [":lib{i}", "//pkg{j}:lib{j}"])
"#
            ),
        )
        .unwrap();
    }
}

#[test]
fn parallel_load_matches_sequential() {
    let root = std::env::temp_dir().join(format!("razel-par-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    const N: usize = 12;
    write_fixture(&root, N);
    let packages: Vec<String> = (0..N).map(|i| format!("pkg{i}")).collect();

    // The contract baseline: sequential loads EVERYTHING green (a broken fixture would make
    // the parity assertion below vacuous).
    let (seq, _) =
        load_tree_report_with_threads(&root, GlobalFlags::default(), &packages, Vec::new(), 1);
    for (pkg, r) in &seq {
        assert!(r.is_ok(), "fixture must be sequentially green: {pkg}: {r:?}");
    }

    // Parity: many rounds (it is a RACE — one corrupt interleaving fails the round; the fix
    // makes every round green by construction, not by luck).
    for round in 0..20 {
        let (par, _) = load_tree_report_with_threads(
            &root,
            GlobalFlags::default(),
            &packages,
            Vec::new(),
            4,
        );
        for (pkg, r) in &par {
            assert!(
                r.is_ok(),
                "threads=4 diverged from sequential (round {round}): {pkg}: {r:?}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}
