//! P1.3: evaluate a parsed [`Expr`] over the [`QueryGraph`] → a [`LabelSet`]. Patterns, `deps`,
//! `rdeps`, the set operators, and `let`/`$var`. The predicates (`kind`/`filter`/`attr`/`labels`,
//! P1.4) and path operators (`somepath`/`allpaths`, P1.5) land next.

use crate::graph::{LabelSet, QueryGraph};
use crate::parse::Expr;
use razel_loading::RawAttr;
use std::collections::BTreeMap;

/// Compile a query regex (Rust `regex` dialect — an allowlisted deviation from Bazel's Java
/// regex, §13). Matches are UNANCHORED (substring), like Bazel's `kind`/`filter`.
fn compile(re: &str) -> Result<regex::Regex, String> {
    regex::Regex::new(re).map_err(|e| format!("invalid regex `{re}`: {e}"))
}

/// The raw label literals of an attr value — `labels()`'s narrower view (`includeSelectKeys=false`,
/// §12): `Str`/`Label` leaves + list/tuple/concat/dict-keys + select ARM VALUES (NOT the condition
/// labels) + the default. Raw (unresolved), as captured.
fn attr_labels(raw: &RawAttr, out: &mut Vec<String>) {
    match raw {
        RawAttr::Str(s) => out.push(s.clone()),
        RawAttr::Label(l) => out.push(l.clone()),
        RawAttr::List(xs) | RawAttr::Tuple(xs) | RawAttr::Concat(xs) => {
            xs.iter().for_each(|x| attr_labels(x, out))
        }
        RawAttr::Dict(pairs) => pairs.iter().for_each(|(k, _)| attr_labels(k, out)),
        RawAttr::Select { arms, default } => {
            for (_cond, v) in arms {
                attr_labels(v, out);
            }
            if let Some(d) = default {
                attr_labels(d, out);
            }
        }
        RawAttr::Int(_) | RawAttr::Bool(_) | RawAttr::None => {}
    }
}

/// The package-label prefix of a target label — everything up to the target-name `:` (repo + `//` +
/// package). `@@r//p:n` → `@@r//p`; `//p:n` → `//p`. A relative attr value resolves within THIS, so
/// an external target's files key in its own repo (P6.Q1.a), not the main repo.
fn pkg_prefix(label: &str) -> &str {
    label.rsplit_once(':').map(|(p, _)| p).unwrap_or(label)
}

/// Resolve a raw attr label to its CANONICAL form relative to `prefix` (the owning target's
/// repo+package label, from [`pkg_prefix`]) — Bazel's `labels()` semantics: `:n` / bare `n` →
/// `{prefix}:n`; `//…` / `@…` are already absolute. (v1 simplification: no subpackage probing.)
fn canonical_label(raw: &str, prefix: &str) -> String {
    if raw.starts_with("//") || raw.starts_with('@') {
        raw.to_string()
    } else if let Some(rest) = raw.strip_prefix(':') {
        format!("{prefix}:{rest}")
    } else {
        format!("{prefix}:{raw}")
    }
}

/// Evaluate `expr` over `graph`, with `--implicit_deps` = `implicit`.
pub fn eval(graph: &QueryGraph, expr: &Expr, implicit: bool) -> Result<LabelSet, String> {
    Eval { graph, implicit, env: BTreeMap::new() }.go(expr)
}

struct Eval<'g> {
    graph: &'g QueryGraph,
    implicit: bool,
    env: BTreeMap<String, LabelSet>,
}

impl Eval<'_> {
    fn go(&mut self, expr: &Expr) -> Result<LabelSet, String> {
        match expr {
            Expr::Pattern(p) => Ok(self.graph.match_pattern(p)),
            Expr::Var(v) => {
                self.env.get(v).cloned().ok_or_else(|| format!("undefined variable `${v}`"))
            }
            Expr::Deps(x, depth) => {
                let s = self.go(x)?;
                Ok(self.graph.deps(&s, *depth, self.implicit))
            }
            Expr::Rdeps(u, x, depth) => {
                let universe = self.go(u)?;
                let roots = self.go(x)?;
                Ok(self.graph.rdeps(&universe, &roots, *depth))
            }
            Expr::Union(a, b) => {
                let (a, b) = (self.go(a)?, self.go(b)?);
                Ok(&a | &b)
            }
            Expr::Except(a, b) => {
                let (a, b) = (self.go(a)?, self.go(b)?);
                Ok(&a - &b)
            }
            Expr::Intersect(a, b) => {
                let (a, b) = (self.go(a)?, self.go(b)?);
                Ok(&a & &b)
            }
            Expr::Let(name, val, body) => {
                let v = self.go(val)?;
                let prev = self.env.insert(name.clone(), v);
                let r = self.go(body);
                match prev {
                    Some(p) => {
                        self.env.insert(name.clone(), p);
                    }
                    None => {
                        self.env.remove(name);
                    }
                }
                r
            }
            Expr::Kind(re, x) => {
                let set = self.go(x)?;
                let re = compile(re)?;
                Ok(set
                    .into_iter()
                    .filter(|l| self.graph.kind(l).is_some_and(|k| re.is_match(k)))
                    .collect())
            }
            Expr::Filter(re, x) => {
                let set = self.go(x)?;
                let re = compile(re)?;
                Ok(set.into_iter().filter(|l| re.is_match(l)).collect())
            }
            Expr::Attr(name, re, x) => {
                let set = self.go(x)?;
                let re = compile(re)?;
                Ok(set
                    .into_iter()
                    .filter(|l| {
                        self.graph
                            .target(l)
                            .and_then(|t| t.attrs.get(name))
                            .is_some_and(|a| re.is_match(&a.canonical()))
                    })
                    .collect())
            }
            Expr::Labels(attr, x) => {
                let set = self.go(x)?;
                let mut out = LabelSet::new();
                for l in set {
                    // Resolve each raw attr value relative to the OWNING target's repo+package, so
                    // the output is canonical (Bazel's `labels()` semantics) rather than as-written
                    // — an EXTERNAL target's relative files key in its own repo, not `//` (P6.Q1.a).
                    let prefix = pkg_prefix(&l);
                    if let Some(a) = self.graph.target(&l).and_then(|t| t.attrs.get(attr)) {
                        let mut v = Vec::new();
                        attr_labels(a, &mut v);
                        out.extend(v.into_iter().map(|raw| canonical_label(&raw, prefix)));
                    }
                }
                Ok(out)
            }
            Expr::SomePath(a, b) => {
                let (a, b) = (self.go(a)?, self.go(b)?);
                Ok(self.graph.somepath(&a, &b, self.implicit))
            }
            Expr::AllPaths(a, b) => {
                let (a, b) = (self.go(a)?, self.go(b)?);
                Ok(self.graph.allpaths(&a, &b, self.implicit))
            }
            Expr::Siblings(x) => {
                let set = self.go(x)?;
                // Every target in the same package(s) as `x` — Bazel's `:*` per owning package
                // (reuses the proven `match_pattern` `:*`: rules + source/generated + the BUILD file).
                let pkgs: std::collections::BTreeSet<&str> = set.iter().map(|l| pkg_prefix(l)).collect();
                let mut out = LabelSet::new();
                for pkg in pkgs {
                    out = &out | &self.graph.match_pattern(&format!("{pkg}:*"));
                }
                Ok(out)
            }
            Expr::SamePkgDirectRdeps(x) => {
                let set = self.go(x)?;
                Ok(self.graph.same_pkg_direct_rdeps(&set))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse;
    use razel_loading::{Edge, EdgeKind, LoadedTarget, TargetKind};

    fn target(label: &str, pkg: &str, rule: &str, deps: &[&str]) -> LoadedTarget {
        LoadedTarget {
            label: label.into(),
            repo: String::new(),
            package: pkg.into(),
            rule_class: rule.into(),
            kind: TargetKind::Library,
            attrs: BTreeMap::new(),
            edges: deps
                .iter()
                .map(|d| Edge { to: (*d).into(), kind: EdgeKind::Rule, attr: "deps".into() })
                .collect(),
            raw_refs: vec![],
        }
    }

    fn graph() -> QueryGraph {
        let mut m = BTreeMap::new();
        m.insert("//a:base".into(), target("//a:base", "a", "rust_library", &[]));
        m.insert("//a:lib".into(), target("//a:lib", "a", "rust_library", &["//a:base"]));
        m.insert("//a:bin".into(), target("//a:bin", "a", "rust_binary", &["//a:lib"]));
        QueryGraph::new(m)
    }

    fn run(expr: &str) -> Vec<String> {
        let g = graph();
        let e = parse(expr).unwrap();
        eval(&g, &e, false).unwrap().into_iter().collect()
    }

    #[test]
    fn pattern_and_deps() {
        assert_eq!(run("//a:bin"), ["//a:bin"]);
        assert_eq!(run("deps(//a:bin)"), ["//a:base", "//a:bin", "//a:lib"]); // sorted
        assert_eq!(run("deps(//a:bin, 1)"), ["//a:bin", "//a:lib"]);
    }

    #[test]
    fn rdeps_and_set_ops() {
        assert_eq!(run("rdeps(//..., //a:base)"), ["//a:base", "//a:bin", "//a:lib"]);
        assert_eq!(run("//a:bin + //a:base"), ["//a:base", "//a:bin"]);
        assert_eq!(run("deps(//a:bin) - //a:base"), ["//a:bin", "//a:lib"]);
        assert_eq!(run("deps(//a:bin) ^ deps(//a:lib)"), ["//a:base", "//a:lib"]);
    }

    #[test]
    fn let_binding_and_vars() {
        assert_eq!(run("let v = //a:lib in $v + deps($v)"), ["//a:base", "//a:lib"]);
        let g = graph();
        assert!(eval(&g, &parse("$undefined").unwrap(), false).unwrap_err().contains("undefined"));
    }

    #[test]
    fn predicates_kind_filter_attr_labels() {
        let mut m = BTreeMap::new();
        m.insert("//a:base".into(), target("//a:base", "a", "rust_library", &[]));
        let mut lib = target("//a:lib", "a", "rust_library", &["//a:base"]);
        // a RELATIVE raw dep — `labels()` must canonicalize it to `//a:base` (Bazel's form).
        lib.attrs.insert("deps".into(), RawAttr::List(vec![RawAttr::Str(":base".into())]));
        m.insert("//a:lib".into(), lib);
        m.insert("//a:bin".into(), target("//a:bin", "a", "rust_binary", &["//a:lib"]));
        let g = QueryGraph::new(m);
        let run = |s: &str| {
            eval(&g, &parse(s).unwrap(), false).unwrap().into_iter().collect::<Vec<_>>()
        };
        assert_eq!(run("kind(\"rust_binary rule\", //...)"), ["//a:bin"]);
        assert_eq!(run("kind(library, //...)"), ["//a:base", "//a:lib"]); // partial match
        assert_eq!(run("filter(base, //...)"), ["//a:base"]);
        assert_eq!(run("labels(deps, //a:lib)"), ["//a:base"]); // `:base` canonicalized to `//a:base`
        assert_eq!(run("attr(deps, base, //...)"), ["//a:lib"]); // deps canonical contains "base"
    }

    #[test]
    fn labels_key_external_relative_values_at_the_targets_repo() {
        // P6.Q1.a: `labels()` resolves a target's RELATIVE attr values (e.g. glob'd source files in
        // `compile_data`) against the target's repo+package. For an EXTERNAL crate_universe target a
        // bare file must key `@@<repo>//:c/blake3.c`, NOT the main repo `//:c/blake3.c` — the pre-fix
        // bug: the package extraction only stripped `//`, leaving external targets an empty prefix.
        let label = "@@rules_rust++crate+crates__blake3-1.8.2//:blake3";
        let mut m = BTreeMap::new();
        let mut t = target(label, "", "rust_library", &[]);
        t.attrs.insert(
            "compile_data".into(),
            RawAttr::List(vec![RawAttr::Str("c/blake3.c".into()), RawAttr::Str(":src/lib.rs".into())]),
        );
        m.insert(label.into(), t);
        let g = QueryGraph::new(m);
        let got = eval(&g, &parse(&format!("labels(compile_data, {label})")).unwrap(), false)
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>();
        assert_eq!(
            got,
            [
                "@@rules_rust++crate+crates__blake3-1.8.2//:c/blake3.c",
                "@@rules_rust++crate+crates__blake3-1.8.2//:src/lib.rs",
            ]
        );
    }

    #[test]
    fn siblings_returns_all_targets_in_the_same_package() {
        // siblings(x) = every target in x's package(s) (Bazel's `:*` per package); reuses match_pattern.
        let mut m = BTreeMap::new();
        m.insert("//a:base".into(), target("//a:base", "a", "rust_library", &[]));
        m.insert("//a:lib".into(), target("//a:lib", "a", "rust_library", &["//a:base"]));
        m.insert("//b:x".into(), target("//b:x", "b", "rust_binary", &[]));
        let g = QueryGraph::new(m);
        let run =
            |s: &str| eval(&g, &parse(s).unwrap(), false).unwrap().into_iter().collect::<Vec<_>>();
        // every target in package `a` (the input + its siblings + the package's BUILD file, per
        // Bazel's `:*` semantics), not `//b:x`.
        assert_eq!(run("siblings(//a:base)"), ["//a:BUILD", "//a:base", "//a:lib"]);
        // a multi-package input unions each package's `:*`.
        assert_eq!(
            run("siblings(//a:lib + //b:x)"),
            ["//a:BUILD", "//a:base", "//a:lib", "//b:BUILD", "//b:x"]
        );
    }

    #[test]
    fn same_pkg_direct_rdeps_finds_direct_callers_in_the_package() {
        // bin → lib → base, all in package `a`; `//c:x` (other package) → //a:lib.
        let mut m = BTreeMap::new();
        m.insert("//a:base".into(), target("//a:base", "a", "rust_library", &[]));
        m.insert("//a:lib".into(), target("//a:lib", "a", "rust_library", &["//a:base"]));
        m.insert("//a:bin".into(), target("//a:bin", "a", "rust_binary", &["//a:lib"]));
        m.insert("//c:x".into(), target("//c:x", "c", "rust_binary", &["//a:lib"]));
        let g = QueryGraph::new(m);
        let run =
            |s: &str| eval(&g, &parse(s).unwrap(), false).unwrap().into_iter().collect::<Vec<_>>();
        // DIRECT same-package rdeps of base = {lib} (lib→base); NOT bin (transitive), NOT base itself.
        assert_eq!(run("same_pkg_direct_rdeps(//a:base)"), ["//a:lib"]);
        // of lib = {bin}; //c:x depends on lib but is a DIFFERENT package → excluded.
        assert_eq!(run("same_pkg_direct_rdeps(//a:lib)"), ["//a:bin"]);
    }

    #[test]
    fn somepath_and_allpaths() {
        let g = graph(); // linear bin → lib → base
        let run = |s: &str| {
            eval(&g, &parse(s).unwrap(), false).unwrap().into_iter().collect::<Vec<_>>()
        };
        assert_eq!(run("somepath(//a:bin, //a:base)"), ["//a:base", "//a:bin", "//a:lib"]);
        assert_eq!(run("allpaths(//a:bin, //a:base)"), ["//a:base", "//a:bin", "//a:lib"]);
        assert!(run("somepath(//a:base, //a:bin)").is_empty()); // no forward path
    }
}
