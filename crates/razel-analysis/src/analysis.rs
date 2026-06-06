//! Analysis (Phase 3, built-in rules): turn loaded [`TargetDecl`]s into the IR's
//! provider + action graph.
//!
//! For each built-in rule target this produces a `DefaultInfo` (its output files) and
//! registers a content-addressed [`ActionNode`] (compile/link) into the [`Graph`], wiring
//! `action ← source files`, `target ← action`, `target ← dep targets` so the rdep/impact
//! query spans the whole loading→analysis pipeline.
//!
//! SCOPE: built-in rule kinds only. Starlark-defined `rule()`/`ctx` (running user rule
//! implementations — the `no_prelude` gate) is the remaining Phase-3 piece: it needs a
//! custom callable Starlark value + rule-impl invocation, tracked separately.

use razel_core::{ActionId, Digest, FileId, NodeRef, TargetId};
use razel_ir::{ActionNode, FileKind, FileNode, Graph, TargetNode};
use razel_loading::TargetDecl;
use std::collections::BTreeMap;

/// The `DefaultInfo` provider: a target's declared output files (package-relative).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultInfo {
    pub files: Vec<String>,
}

/// Result of analyzing a package: the action graph + per-target providers.
pub struct Analysis {
    pub graph: Graph,
    pub providers: BTreeMap<String, DefaultInfo>,
}

/// Resolve a dep string (`:lib` or `//other:x`) to a canonical target label.
fn resolve_dep(package: &str, dep: &str) -> String {
    match dep.strip_prefix(':') {
        Some(rest) => format!("//{package}:{rest}"),
        None => dep.to_string(),
    }
}

/// The output file a built-in rule produces.
fn output_path(package: &str, t: &TargetDecl) -> String {
    match t.kind.as_str() {
        "cc_library" => format!("{package}/lib{}.a", t.name),
        "cc_binary" | "cc_test" => format!("{package}/{}", t.name),
        _ => format!("{package}/{}.out", t.name),
    }
}

/// Analyze the targets of one package into providers + an action graph.
pub fn analyze(package: &str, targets: &[TargetDecl]) -> Analysis {
    let mut g = Graph::new();
    let mut providers = BTreeMap::new();

    for t in targets {
        let label = format!("//{package}:{}", t.name);
        let tid = TargetId::new(&label);
        g.add_target(TargetNode {
            id: tid.clone(),
            kind: t.target_kind(),
        });

        // One action per target. Content key folds in everything that defines it
        // (F12: completeness is correctness — a missing input = silent wrong build).
        let aid = ActionId::new(format!("{label}#compile"));
        let key = Digest::of(format!("{}|{}|{:?}|{:?}", t.kind, t.name, t.srcs, t.deps).as_bytes());
        g.add_action(ActionNode {
            id: aid.clone(),
            content_key: key,
        });

        // action ← source files.
        for s in &t.srcs {
            let sfid = FileId::new(format!("{package}/{s}"));
            g.add_file(FileNode {
                id: sfid.clone(),
                digest: None,
                exists: true,
                kind: FileKind::Source,
            });
            g.add_dep(NodeRef::Action(aid.clone()), NodeRef::File(sfid));
        }

        // declared output (generated file node; DefaultInfo carries it).
        let out = output_path(package, t);
        g.add_file(FileNode {
            id: FileId::new(&out),
            digest: None,
            exists: false,
            kind: FileKind::Generated,
        });

        // target ← action.
        g.add_dep(NodeRef::Target(tid.clone()), NodeRef::Action(aid.clone()));
        // target ← dep targets.
        for d in &t.deps {
            g.add_dep(
                NodeRef::Target(tid.clone()),
                NodeRef::Target(TargetId::new(resolve_dep(package, d))),
            );
        }

        providers.insert(label, DefaultInfo { files: vec![out] });
    }

    Analysis {
        graph: g,
        providers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use razel_loading::load_build;

    fn analyze_build(pkg: &str, src: &str) -> Analysis {
        let targets = load_build("BUILD", src, &[]).unwrap();
        analyze(pkg, &targets)
    }

    const BUILD: &str = r#"
cc_library(name = "lib", srcs = ["a.cc", "b.cc"])
cc_binary(name = "app", srcs = ["main.cc"], deps = [":lib"])
cc_test(name = "lib_test", srcs = ["lib_test.cc"], deps = [":lib"])
"#;

    #[test]
    fn analysis_produces_providers_with_outputs() {
        let a = analyze_build("pkg", BUILD);
        assert_eq!(a.providers["//pkg:lib"].files, vec!["pkg/liblib.a"]);
        assert_eq!(a.providers["//pkg:app"].files, vec!["pkg/app"]);
        assert_eq!(a.providers["//pkg:lib_test"].files, vec!["pkg/lib_test"]);
    }

    #[test]
    fn impact_spans_loading_to_analysis() {
        // Editing lib's source affects lib's action+target and everything depending on lib.
        let a = analyze_build("pkg", BUILD);
        let (tests, deliverables) = a.graph.impacted_targets(&FileId::new("pkg/a.cc"));
        assert_eq!(
            tests,
            std::collections::BTreeSet::from([TargetId::new("//pkg:lib_test")])
        );
        assert_eq!(
            deliverables,
            std::collections::BTreeSet::from([
                TargetId::new("//pkg:lib"),
                TargetId::new("//pkg:app")
            ])
        );
    }

    #[test]
    fn editing_a_leaf_source_does_not_affect_siblings() {
        // app's own source affects only app, not lib or lib_test.
        let a = analyze_build("pkg", BUILD);
        let (tests, deliverables) = a.graph.impacted_targets(&FileId::new("pkg/main.cc"));
        assert!(tests.is_empty());
        assert_eq!(
            deliverables,
            std::collections::BTreeSet::from([TargetId::new("//pkg:app")])
        );
    }

    #[test]
    fn action_keys_are_deterministic_and_unique() {
        let a1 = analyze_build("pkg", BUILD);
        let a2 = analyze_build("pkg", BUILD);
        let k = |a: &Analysis, lbl: &str| {
            a.graph
                .action(&ActionId::new(format!("{lbl}#compile")))
                .unwrap()
                .content_key
        };
        // deterministic across runs
        assert_eq!(k(&a1, "//pkg:lib"), k(&a2, "//pkg:lib"));
        // unique across distinct targets
        assert_ne!(k(&a1, "//pkg:lib"), k(&a1, "//pkg:app"));
    }
}
