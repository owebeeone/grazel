//! Loading (Phase 2): evaluate a `BUILD` file into target declarations.
//!
//! A `BUILD` file is plain Starlark evaluated with rule functions injected as builtins
//! (`cc_library`/`cc_binary`/`cc_test`/`filegroup`/`genrule`). Each rule call records a
//! [`TargetDecl`]; arbitrary extra kwargs (`hdrs`, `copts`, `visibility`, …) are absorbed
//! and ignored for now. `glob()` matches against the package's file list. Legacy macros
//! (plain Starlark `def`s that call rules) work for free via evaluation.

use razel_ir::TargetKind;
use starlark::collections::SmallMap;
use starlark::environment::{GlobalsBuilder, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};
use starlark::values::Value;
use starlark::values::list::UnpackList;
use starlark::values::none::NoneType;
use std::cell::RefCell;

/// A target instantiated by a rule call in a `BUILD` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetDecl {
    pub name: String,
    pub kind: String,
    pub srcs: Vec<String>,
    pub deps: Vec<String>,
}

impl TargetDecl {
    /// Map the rule kind onto the IR's coarse target kind.
    pub fn target_kind(&self) -> TargetKind {
        match self.kind.as_str() {
            k if k.ends_with("_test") => TargetKind::Test,
            k if k.ends_with("_binary") => TargetKind::Binary,
            _ => TargetKind::Library,
        }
    }
}

/// Per-load context: the package's file list (for `glob`) + collected targets.
#[derive(Default)]
struct LoadCtx {
    files: Vec<String>,
    targets: Vec<TargetDecl>,
}

thread_local! {
    static CTX: RefCell<LoadCtx> = RefCell::new(LoadCtx::default());
}

fn record(kind: &str, name: String, srcs: Option<Vec<String>>, deps: Option<Vec<String>>) {
    CTX.with_borrow_mut(|c| {
        c.targets.push(TargetDecl {
            name,
            kind: kind.to_string(),
            srcs: srcs.unwrap_or_default(),
            deps: deps.unwrap_or_default(),
        })
    });
}

/// Match a `glob` pattern against a path. Supports `*` (within a segment) and `**`
/// (across segments). A documented subset of Bazel glob — enough for `*.cc`, `a/*.h`,
/// `src/**/*.cc`. (No `?`, char-classes, or `**` mid-segment.)
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let p: Vec<&str> = pattern.split('/').collect();
    let s: Vec<&str> = path.split('/').collect();
    seg_match(&p, &s)
}

fn seg_match(pat: &[&str], path: &[&str]) -> bool {
    match pat.first() {
        None => path.is_empty(),
        Some(&"**") => {
            // `**` matches zero or more path segments.
            (0..=path.len()).any(|i| seg_match(&pat[1..], &path[i..]))
        }
        Some(seg) => {
            !path.is_empty() && star_match(seg, path[0]) && seg_match(&pat[1..], &path[1..])
        }
    }
}

/// Single-segment match with `*` = any run of non-`/` chars.
fn star_match(pat: &str, s: &str) -> bool {
    match pat.split_once('*') {
        None => pat == s,
        Some((pre, rest)) => {
            if !s.starts_with(pre) {
                return false;
            }
            let s = &s[pre.len()..];
            // try every split point for the `*`.
            (0..=s.len()).any(|i| star_match(rest, &s[i..]))
        }
    }
}

#[starlark::starlark_module]
fn build_rules(b: &mut GlobalsBuilder) {
    fn cc_library<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(require = named)] deps: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kwargs: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        record(
            "cc_library",
            name,
            srcs.map(|l| l.items),
            deps.map(|l| l.items),
        );
        Ok(NoneType)
    }
    fn cc_binary<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(require = named)] deps: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kwargs: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        record(
            "cc_binary",
            name,
            srcs.map(|l| l.items),
            deps.map(|l| l.items),
        );
        Ok(NoneType)
    }
    fn cc_test<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(require = named)] deps: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kwargs: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        record(
            "cc_test",
            name,
            srcs.map(|l| l.items),
            deps.map(|l| l.items),
        );
        Ok(NoneType)
    }
    fn filegroup<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kwargs: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        record("filegroup", name, srcs.map(|l| l.items), None);
        Ok(NoneType)
    }
    fn genrule<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kwargs: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        record("genrule", name, srcs.map(|l| l.items), None);
        Ok(NoneType)
    }
    /// `glob(include, exclude=[])` — match the package's files against the patterns.
    fn glob<'v>(
        #[starlark(require = pos)] include: UnpackList<String>,
        #[starlark(require = named)] exclude: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kwargs: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<Vec<String>> {
        let include = include.items;
        let exclude = exclude.map(|l| l.items).unwrap_or_default();
        let mut out: Vec<String> = CTX.with_borrow(|c| {
            c.files
                .iter()
                .filter(|f| {
                    include.iter().any(|p| glob_match(p, f))
                        && !exclude.iter().any(|p| glob_match(p, f))
                })
                .cloned()
                .collect()
        });
        out.sort();
        Ok(out)
    }
}

/// Evaluate a `BUILD` source into its target declarations. `package_files` is the
/// package's file inventory (for `glob`).
pub fn load_build(
    build_name: &str,
    src: &str,
    package_files: &[&str],
) -> Result<Vec<TargetDecl>, String> {
    CTX.with_borrow_mut(|c| {
        c.files = package_files.iter().map(|s| s.to_string()).collect();
        c.targets.clear();
    });

    let ast = AstModule::parse(build_name, src.to_owned(), &Dialect::Extended)
        .map_err(|e| format!("{e}"))?;
    let globals = GlobalsBuilder::standard().with(build_rules).build();
    let result: Result<(), String> = Module::with_temp_heap(|module| {
        let mut eval = Evaluator::new(&module);
        eval.eval_module(ast, &globals)
            .map_err(|e| format!("{e}"))?;
        Ok(())
    });
    result?;

    Ok(CTX.with_borrow_mut(|c| std::mem::take(&mut c.targets)))
}

/// `query //pkg:*`-lite: the canonical labels of every target in a `BUILD` file.
pub fn query_targets(
    package: &str,
    src: &str,
    package_files: &[&str],
) -> Result<Vec<String>, String> {
    let mut labels: Vec<String> = load_build("BUILD", src, package_files)?
        .into_iter()
        .map(|t| format!("//{package}:{}", t.name))
        .collect();
    labels.sort();
    Ok(labels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matcher_subset() {
        assert!(glob_match("*.cc", "a.cc"));
        assert!(!glob_match("*.cc", "a.h"));
        assert!(glob_match("src/*.cc", "src/a.cc"));
        assert!(!glob_match("src/*.cc", "src/sub/a.cc"));
        assert!(glob_match("**/*.h", "a.h"));
        assert!(glob_match("**/*.h", "src/sub/a.h"));
        assert!(glob_match("src/**/*.cc", "src/a/b/c.cc"));
        assert!(!glob_match("src/**/*.cc", "other/a.cc"));
    }

    #[test]
    fn loads_targets_and_ignores_extra_kwargs() {
        let build = r#"
cc_library(
    name = "lib",
    srcs = ["a.cc", "b.cc"],
    hdrs = ["lib.h"],          # extra kwarg — absorbed
    visibility = ["//visibility:public"],
)
cc_binary(name = "app", srcs = ["main.cc"], deps = [":lib"])
cc_test(name = "lib_test", srcs = ["lib_test.cc"], deps = [":lib"])
"#;
        let targets = load_build("BUILD", build, &[]).unwrap();
        assert_eq!(targets.len(), 3);
        assert_eq!(targets[0].name, "lib");
        assert_eq!(targets[0].srcs, vec!["a.cc", "b.cc"]);
        assert_eq!(targets[0].target_kind(), TargetKind::Library);
        assert_eq!(targets[1].name, "app");
        assert_eq!(targets[1].deps, vec![":lib"]);
        assert_eq!(targets[1].target_kind(), TargetKind::Binary);
        assert_eq!(targets[2].target_kind(), TargetKind::Test);
    }

    #[test]
    fn glob_feeds_srcs() {
        let build = r#"cc_library(name = "g", srcs = glob(["*.cc"], exclude = ["skip.cc"]))"#;
        let files = ["a.cc", "b.cc", "skip.cc", "header.h"];
        let targets = load_build("BUILD", build, &files).unwrap();
        assert_eq!(targets[0].srcs, vec!["a.cc", "b.cc"]); // sorted; .h and skip.cc excluded
    }

    #[test]
    fn legacy_macro_expands() {
        // A plain Starlark function (legacy macro) that instantiates two rules.
        let build = r#"
def my_lib(name):
    cc_library(name = name, srcs = [name + ".cc"])
    cc_test(name = name + "_test", srcs = [name + "_test.cc"], deps = [":" + name])

my_lib(name = "widget")
"#;
        let targets = load_build("BUILD", build, &[]).unwrap();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].name, "widget");
        assert_eq!(targets[1].name, "widget_test");
    }

    #[test]
    fn query_lists_canonical_labels() {
        let build = r#"
cc_library(name = "a", srcs = ["a.cc"])
cc_binary(name = "b", srcs = ["b.cc"])
"#;
        let labels = query_targets("foo/bar", build, &[]).unwrap();
        assert_eq!(labels, vec!["//foo/bar:a", "//foo/bar:b"]);
    }
}
