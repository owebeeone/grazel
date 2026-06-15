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
                    if let Some(a) = self.graph.target(&l).and_then(|t| t.attrs.get(attr)) {
                        let mut v = Vec::new();
                        attr_labels(a, &mut v);
                        out.extend(v);
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
        lib.attrs.insert("deps".into(), RawAttr::List(vec![RawAttr::Str("//a:base".into())]));
        m.insert("//a:lib".into(), lib);
        m.insert("//a:bin".into(), target("//a:bin", "a", "rust_binary", &["//a:lib"]));
        let g = QueryGraph::new(m);
        let run = |s: &str| {
            eval(&g, &parse(s).unwrap(), false).unwrap().into_iter().collect::<Vec<_>>()
        };
        assert_eq!(run("kind(\"rust_binary rule\", //...)"), ["//a:bin"]);
        assert_eq!(run("kind(library, //...)"), ["//a:base", "//a:lib"]); // partial match
        assert_eq!(run("filter(base, //...)"), ["//a:base"]);
        assert_eq!(run("labels(deps, //a:lib)"), ["//a:base"]); // the raw dep label
        assert_eq!(run("attr(deps, base, //...)"), ["//a:lib"]); // deps canonical contains "base"
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
