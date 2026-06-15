//! P1.7: the `qg` query-golden harness — `razel query <expr>` over a hermetic mixed-rule corpus,
//! compared to a golden per the §12 output mode. q1 (patterns + kind + label_kind), q2 (deps/rdeps
//! + set ops), q3 (somepath/allpaths + attr).
//!
//! These goldens are **razel-authored behavior sentinels**: they lock razel's query output (the
//! cases below are validated against `bazel query`'s semantics, which the unit tests pin). LIVE
//! `bazel query` parity capture — the design's true `qg` — is the remaining authoring step; it
//! needs `bazel` + a dual-queryable corpus and rides the goldens xtask (like the aquery goldens).

use razel_query::{GlobalFlags, Output, run};
use std::path::Path;

/// A hermetic mixed-rule corpus (bare natives — no external load): a filegroup deps chain
/// (bin→lib→base→base.txt), an alias, a genrule, and a config_setting.
fn corpus(root: &Path) {
    let app = root.join("app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(root.join("MODULE.bazel"), "").unwrap();
    std::fs::write(app.join("base.txt"), "").unwrap();
    std::fs::write(
        app.join("BUILD"),
        r#"filegroup(name = "base", srcs = ["base.txt"])
filegroup(name = "lib", srcs = [":base"])
filegroup(name = "bin", srcs = [":lib"])
alias(name = "lib_alias", actual = ":lib")
genrule(name = "gen", srcs = [], outs = ["gen.out"], cmd = "touch $@")
config_setting(name = "dbg", values = {"compilation_mode": "dbg"})
"#,
    )
    .unwrap();
}

#[test]
fn query_goldens_q1_q2_q3() {
    let tmp = std::env::temp_dir().join(format!("razel-qg-{}", std::process::id()));
    corpus(&tmp);

    // (expr, output mode, expected golden). All run with --noimplicit_deps (implicit=false).
    let cases: &[(&str, Output, &str)] = &[
        // ── q1: patterns + kind + label_kind ──────────────────────────────────────────────
        (
            "//app:all",
            Output::Label,
            "//app:base\n//app:bin\n//app:dbg\n//app:gen\n//app:lib\n//app:lib_alias\n",
        ),
        ("kind(\"filegroup rule\", //...)", Output::Label, "//app:base\n//app:bin\n//app:lib\n"),
        ("//app:lib", Output::LabelKind, "filegroup rule //app:lib\n"),
        // ── q2: deps / rdeps / set ops ────────────────────────────────────────────────────
        ("deps(//app:bin)", Output::Label, "//app:base\n//app:base.txt\n//app:bin\n//app:lib\n"),
        ("rdeps(//..., //app:base)", Output::Label, "//app:base\n//app:bin\n//app:lib\n//app:lib_alias\n"),
        ("deps(//app:bin) - //app:base", Output::Label, "//app:base.txt\n//app:bin\n//app:lib\n"),
        // ── q3: somepath / allpaths / attr ────────────────────────────────────────────────
        ("somepath(//app:bin, //app:base)", Output::Label, "//app:base\n//app:bin\n//app:lib\n"),
        ("allpaths(//app:bin, //app:base)", Output::Label, "//app:base\n//app:bin\n//app:lib\n"),
        ("attr(srcs, base, //app:lib)", Output::Label, "//app:lib\n"),
    ];

    let mut failures = Vec::new();
    for (expr, mode, expected) in cases {
        match run(&tmp, GlobalFlags::default(), expr, *mode, false) {
            Ok(got) if got == *expected => {}
            Ok(got) => failures.push(format!("`{expr}`\n  expected: {expected:?}\n  got:      {got:?}")),
            Err(e) => failures.push(format!("`{expr}` errored: {e}")),
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(failures.is_empty(), "qg mismatches:\n{}", failures.join("\n"));
}
