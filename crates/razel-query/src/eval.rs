//! P1.3: evaluate a parsed [`Expr`] over the [`QueryGraph`] → a [`LabelSet`]. Patterns, `deps`,
//! `rdeps`, the set operators, and `let`/`$var`. The predicates (`kind`/`filter`/`attr`/`labels`,
//! P1.4) and path operators (`somepath`/`allpaths`, P1.5) land next.

use crate::graph::{LabelSet, QueryGraph};
use crate::parse::Expr;
use std::collections::BTreeMap;

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
            Expr::Kind(..) | Expr::Filter(..) | Expr::Attr(..) | Expr::Labels(..) => {
                Err("predicate (kind/filter/attr/labels) not yet implemented (P1.4)".into())
            }
            Expr::SomePath(..) | Expr::AllPaths(..) => {
                Err("path operators (somepath/allpaths) not yet implemented (P1.5)".into())
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
    fn unimplemented_ops_error_clearly() {
        let g = graph();
        assert!(eval(&g, &parse("kind(rust, //...)").unwrap(), false).unwrap_err().contains("P1.4"));
        assert!(eval(&g, &parse("somepath(//a:bin, //a:base)").unwrap(), false).unwrap_err().contains("P1.5"));
    }
}
