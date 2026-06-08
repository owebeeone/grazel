//! Wire Starlark-rule analysis results into the IR's provider + action graph.
//!
//! Live path: `razel_loading::analyze_starlark` → [`wire_to_ir`] → `razel_ir::Graph`,
//! consumed by `razel-build`. (Phase 1 removed the dead parallel `analyze`/`TargetDecl`/
//! `Depset<T>` pipeline — it had no live callers; the live path never used it.)

use razel_core::{ActionId, Digest, FileId, NodeRef, TargetId};
use razel_ir::{ActionNode, FileKind, FileNode, Graph, TargetKind, TargetNode};

/// Infer a coarse target kind from the target name's suffix (the dialect convention:
/// `*_test` → Test, `*_binary` → Binary, else Library). Analysis doesn't carry kind for
/// user rules, so this is the available signal for the impact query's test/deliverable split.
fn infer_kind(name: &str) -> TargetKind {
    if name.ends_with("_test") {
        TargetKind::Test
    } else if name.ends_with("_binary") {
        TargetKind::Binary
    } else {
        TargetKind::Library
    }
}

/// Wire Starlark-rule analysis results ([`razel_loading::AnalyzedTarget`]) into the IR,
/// so user-defined rules join the same action graph + rdep/impact query. Target kind is
/// inferred from the name suffix ([`infer_kind`]) so the impact query can partition tests
/// vs deliverables.
pub fn wire_to_ir(package: &str, targets: &[razel_loading::AnalyzedTarget]) -> Graph {
    let mut g = Graph::new();
    for t in targets {
        let label = format!("//{package}:{}", t.name);
        let tid = TargetId::new(&label);
        g.add_target(TargetNode {
            id: tid.clone(),
            kind: infer_kind(&t.name),
        });
        for (i, act) in t.actions.iter().enumerate() {
            let aid = ActionId::new(format!("{label}#{i}"));
            let key = Digest::of(
                format!("{}|{:?}|{:?}", act.mnemonic, act.inputs, act.outputs).as_bytes(),
            );
            g.add_action(ActionNode {
                id: aid.clone(),
                content_key: key,
            });
            for inp in &act.inputs {
                let fid = FileId::new(format!("{package}/{inp}"));
                g.add_file(FileNode {
                    id: fid.clone(),
                    digest: None,
                    exists: true,
                    kind: FileKind::Source,
                });
                g.add_dep(NodeRef::Action(aid.clone()), NodeRef::File(fid));
            }
            for out in &act.outputs {
                g.add_file(FileNode {
                    id: FileId::new(format!("{package}/{out}")),
                    digest: None,
                    exists: false,
                    kind: FileKind::Generated,
                });
            }
            g.add_dep(NodeRef::Target(tid.clone()), NodeRef::Action(aid.clone()));
        }
    }
    g
}

#[cfg(test)]
mod tests {
    use super::*;
    use razel_loading::analyze_starlark;

    #[test]
    fn starlark_analysis_wires_into_ir_and_impact_query_works() {
        let src = r#"
def _impl(ctx):
    out = ctx.actions.declare_file(ctx.attr.name + ".o")
    ctx.actions.run(executable = "cc", outputs = [out], inputs = [ctx.attr.src], arguments = [])
    return [DefaultInfo(files = [out])]

cc_thing = rule(implementation = _impl, attrs = {"src": 1})
cc_thing(name = "widget", src = "widget.c")
"#;
        let analyzed = analyze_starlark("BUILD", src).unwrap();
        let g = wire_to_ir("pkg", &analyzed);
        // Editing the rule's input affects its target, through the wired IR.
        let (_tests, deliverables) = g.impacted_targets(&FileId::new("pkg/widget.c"));
        assert!(deliverables.contains(&TargetId::new("//pkg:widget")));
    }
}
