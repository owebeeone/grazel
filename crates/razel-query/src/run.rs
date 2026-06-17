//! P1.6: the `razel query` entry — parse → load the patterns' packages (load-only, no analysis) →
//! evaluate over the §11 graph → format. Returns the output text (results → stdout); CLI-local
//! (no daemon) in v1 (§13).

use crate::{Expr, Output, QueryGraph, eval, format, parse};
use razel_loading::{GlobalFlags, load_query_graph, packages_for_pattern};
use std::collections::BTreeSet;
use std::path::Path;

/// Run a query expression against the workspace at `root`. `implicit` = `--implicit_deps`.
pub fn run(
    root: &Path,
    flags: GlobalFlags,
    expr_str: &str,
    output: Output,
    implicit: bool,
) -> Result<String, String> {
    let mut expr = parse(expr_str)?;
    // q4 (§13): canonicalize `@crates`-family patterns to their `@@rules_rust++crate+…` identity, so
    // an apparent `@crates//:blake3` both MATERIALIZES and MATCHES the canonical-keyed loaded graph
    // (the loader canonicalizes `@crates` edges when the lock is seeded). Workspace + non-crate `@`
    // patterns pass through unchanged; the golden comparator normalizes the canonical display.
    canonicalize_patterns(&mut expr, &|p| razel_loading::canonicalize_query_pattern(root, &flags, p));

    // The packages to load = the patterns the expression references; loading pulls in their deps,
    // so traversal (deps/rdeps) has them. (Traversal into @crates / the slice is q4 / P5.2/P5.3.)
    let mut patterns = Vec::new();
    collect_patterns(&expr, &mut patterns);
    let mut packages: BTreeSet<String> = BTreeSet::new();
    for p in &patterns {
        for pkg in packages_for_pattern(root, p, flags.strict_bazel)? {
            packages.insert(pkg);
        }
    }

    let pkgs: Vec<String> = packages.into_iter().collect();
    let graph = QueryGraph::new(load_query_graph(root, flags, &pkgs));
    let set = eval(&graph, &expr, implicit)?;
    Ok(format(&graph, &set, output))
}

/// The target-pattern literals an expression references (which packages to load). `$var`s and
/// string args (regex/attr-name) contribute none.
fn collect_patterns(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Pattern(p) => out.push(p.clone()),
        Expr::Var(_) => {}
        Expr::Deps(x, _)
        | Expr::Kind(_, x)
        | Expr::Filter(_, x)
        | Expr::Attr(_, _, x)
        | Expr::Labels(_, x)
        | Expr::Siblings(x)
        | Expr::SamePkgDirectRdeps(x)
        | Expr::Tests(x) => collect_patterns(x, out),
        Expr::Rdeps(a, b, _)
        | Expr::SomePath(a, b)
        | Expr::AllPaths(a, b)
        | Expr::Union(a, b)
        | Expr::Except(a, b)
        | Expr::Intersect(a, b) => {
            collect_patterns(a, out);
            collect_patterns(b, out);
        }
        Expr::Let(_, v, b) => {
            collect_patterns(v, out);
            collect_patterns(b, out);
        }
    }
}

/// Rewrite every target-pattern LITERAL in the expression through `canon` (q4 §13 — canonicalize
/// `@crates`-family patterns to their `@@rules_rust++crate+…` identity). Mirrors [`collect_patterns`]'
/// structure; `$var`s and string args (regex/attr-name/kind) are not labels and are left untouched.
fn canonicalize_patterns(expr: &mut Expr, canon: &dyn Fn(&str) -> String) {
    match expr {
        Expr::Pattern(p) => *p = canon(p),
        Expr::Var(_) => {}
        Expr::Deps(x, _)
        | Expr::Kind(_, x)
        | Expr::Filter(_, x)
        | Expr::Attr(_, _, x)
        | Expr::Labels(_, x)
        | Expr::Siblings(x)
        | Expr::SamePkgDirectRdeps(x)
        | Expr::Tests(x) => canonicalize_patterns(x, canon),
        Expr::Rdeps(a, b, _)
        | Expr::SomePath(a, b)
        | Expr::AllPaths(a, b)
        | Expr::Union(a, b)
        | Expr::Except(a, b)
        | Expr::Intersect(a, b) => {
            canonicalize_patterns(a, canon);
            canonicalize_patterns(b, canon);
        }
        Expr::Let(_, v, b) => {
            canonicalize_patterns(v, canon);
            canonicalize_patterns(b, canon);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_patterns_finds_all_literals() {
        let e = parse("deps(//a:x) + rdeps(//..., //b:y) - kind(foo, //c:z)").unwrap();
        let mut p = Vec::new();
        collect_patterns(&e, &mut p);
        p.sort();
        assert_eq!(p, ["//...", "//a:x", "//b:y", "//c:z"]);
    }

    #[test]
    fn run_end_to_end_over_a_temp_workspace() {
        let tmp = std::env::temp_dir().join(format!("razel-p16-{}", std::process::id()));
        let pkg = tmp.join("app");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(tmp.join("MODULE.bazel"), "").unwrap();
        std::fs::write(
            pkg.join("BUILD"),
            "filegroup(name = \"base\", srcs = [])\nfilegroup(name = \"lib\", srcs = [\":base\"])\n",
        )
        .unwrap();
        let out =
            run(&tmp, GlobalFlags::default(), "deps(//app:lib)", Output::LabelKind, false).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
        // deps(//app:lib) = lib + its dep base; label_kind, sorted.
        assert_eq!(out, "filegroup rule //app:base\nfilegroup rule //app:lib\n");
    }
}
