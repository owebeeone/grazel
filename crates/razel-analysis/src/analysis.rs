//! Wire Starlark-rule analysis results into the IR's provider + action graph.
//!
//! Live path: `razel_loading::analyze_starlark` → [`wire_to_ir`] → `razel_ir::Graph`,
//! consumed by `razel-build`. (Phase 1 removed the dead parallel `analyze`/`TargetDecl`/
//! `Depset<T>` pipeline — it had no live callers; the live path never used it.)

use razel_core::{ActionId, Digest, FileId, Label, NodeRef, TargetId};
use razel_dds::{
    Dds, FieldId, FieldKind, FieldValue, InstanceId, Provider, ProviderSchema, ProviderTypeId,
    Scalar, TargetKey,
};
use razel_ir::{ActionNode, FileKind, FileNode, Graph, TargetKind, TargetNode};
use std::collections::{BTreeMap, BTreeSet};

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

/// Assemble the analyzed targets' providers into a [`Dds`] (the assembler step — §0): each
/// target contributes a `DefaultInfo` (its output `files`); a cc target (one that exports
/// `hdrs`/`cflags`) also contributes a `CcInfo`. String members are Set-valued (the V1 algebra;
/// ordered link semantics are the reserved `OrderedDepset`). This is `Session.results`
/// graduating into typed DDS facts — the spine going load-bearing on the live path.
pub fn wire_to_dds(
    targets: &[razel_loading::AnalyzedTarget],
    instance: InstanceId,
) -> Result<Dds, String> {
    let mut dds = Dds::new();
    // Declare the provider schemas this assembler produces (the rule-pack's §2 declaration).
    dds.register_schema(
        ProviderTypeId::new("DefaultInfo"),
        ProviderSchema::new().field(FieldId::new("files"), FieldKind::Set),
    );
    dds.register_schema(
        ProviderTypeId::new("CcInfo"),
        ProviderSchema::new()
            .field(FieldId::new("hdrs"), FieldKind::Set)
            .field(FieldId::new("cflags"), FieldKind::Set),
    );
    // The loader's pre-propagated (transitive) hdrs/cflags by target name — used to derive each
    // target's OWN (non-transitive) set, so propagation moves OFF the loader and ONTO the DDS
    // fold: CcInfo stores OWN; the transitive set is recovered by `Dds::fold_set` over deps.
    let trans: BTreeMap<&str, (&Vec<String>, &Vec<String>)> = targets
        .iter()
        .map(|t| (t.name.as_str(), (&t.hdrs, &t.cflags)))
        .collect();

    for t in targets {
        let key = target_key(instance, &t.name)?;

        dds.assert(
            key.clone(),
            ProviderTypeId::new("DefaultInfo"),
            Provider::new().with(FieldId::new("files"), str_set(&t.default_info)),
        )
        .map_err(|e| format!("{e:?}"))?;

        if !t.hdrs.is_empty() || !t.cflags.is_empty() {
            // own = transitive − ∪(deps' transitive); fold_set recovers the closure. (A header
            // shared between own and a dep is attributed to the dep — the closure is unaffected.)
            let dep_hdrs: BTreeSet<&str> = t
                .deps
                .iter()
                .filter_map(|d| trans.get(d.as_str()))
                .flat_map(|(h, _)| h.iter())
                .map(String::as_str)
                .collect();
            let dep_cflags: BTreeSet<&str> = t
                .deps
                .iter()
                .filter_map(|d| trans.get(d.as_str()))
                .flat_map(|(_, c)| c.iter())
                .map(String::as_str)
                .collect();
            let own_hdrs: Vec<String> =
                t.hdrs.iter().filter(|h| !dep_hdrs.contains(h.as_str())).cloned().collect();
            let own_cflags: Vec<String> =
                t.cflags.iter().filter(|c| !dep_cflags.contains(c.as_str())).cloned().collect();
            dds.assert(
                key.clone(),
                ProviderTypeId::new("CcInfo"),
                Provider::new()
                    .with(FieldId::new("hdrs"), str_set(&own_hdrs))
                    .with(FieldId::new("cflags"), str_set(&own_cflags)),
            )
            .map_err(|e| format!("{e:?}"))?;
        }

        // Dep edges — the graph `fold_set` traverses to recompute the transitive closure.
        let dep_keys = t
            .deps
            .iter()
            .map(|d| target_key(instance, d))
            .collect::<Result<Vec<_>, _>>()?;
        dds.assert_deps(key, dep_keys);
    }
    Ok(dds)
}

/// Canonicalize a target name into a [`TargetKey`] (single-package bare names → `//:name`).
fn target_key(instance: InstanceId, name: &str) -> Result<TargetKey, String> {
    Label::parse_canonical(name)
        .or_else(|_| Label::parse_canonical(&format!("//:{name}")))
        .map(|label| TargetKey::new(instance, label))
        .map_err(|e| format!("{e}"))
}

/// A `Set`-valued field of string scalars (the V1 set-valued provider field).
fn str_set(xs: &[String]) -> FieldValue {
    FieldValue::Set(xs.iter().map(|s| Scalar::Str(s.clone())).collect())
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

    #[test]
    fn wire_to_dds_captures_cc_providers() {
        use razel_dds::DdsRead;
        let src = r#"
load("@rules_cc//cc:defs.bzl", "cc_library")
cc_library(name = "greet", srcs = ["greet.cc"], hdrs = ["greet.h"])
"#;
        let targets = razel_loading::analyze_bazel(src).unwrap();
        let dds = wire_to_dds(&targets, InstanceId::SINGLE).unwrap();
        let key = TargetKey::new(InstanceId::SINGLE, Label::parse_canonical("//:greet").unwrap());

        let want_set = |x: &str| FieldValue::Set([Scalar::Str(x.to_string())].into_iter().collect());
        // DefaultInfo.files = the archive; CcInfo.hdrs = the exported header.
        let di = dds.provider(&key, &ProviderTypeId::new("DefaultInfo")).unwrap();
        assert_eq!(di.get(&FieldId::new("files")), Some(&want_set("libgreet.a")));
        let cc = dds.provider(&key, &ProviderTypeId::new("CcInfo")).unwrap();
        assert_eq!(cc.get(&FieldId::new("hdrs")), Some(&want_set("greet.h")));
    }

    #[test]
    fn fold_recovers_transitive_hdrs_from_own() {
        use razel_dds::DdsRead;
        let src = r#"
load("@rules_cc//cc:defs.bzl", "cc_library")
cc_library(name = "base", srcs = ["base.cc"], hdrs = ["base.h"])
cc_library(name = "util", srcs = ["util.cc"], hdrs = ["util.h"], deps = [":base"])
"#;
        let targets = razel_loading::analyze_bazel(src).unwrap();
        let dds = wire_to_dds(&targets, InstanceId::SINGLE).unwrap();
        let util = TargetKey::new(InstanceId::SINGLE, Label::parse_canonical("//:util").unwrap());
        let cc = ProviderTypeId::new("CcInfo");
        let hdrs = FieldId::new("hdrs");
        // CcInfo stores OWN (util.h only); the transitive closure (base.h + util.h) is the fold.
        let own = dds.provider(&util, &cc).unwrap().get(&hdrs).unwrap();
        assert_eq!(*own, FieldValue::Set([Scalar::Str("util.h".into())].into_iter().collect()));
        let want: BTreeSet<Scalar> =
            ["base.h", "util.h"].iter().map(|s| Scalar::Str(s.to_string())).collect();
        assert_eq!(dds.fold_set(&util, &cc, &hdrs), want);
    }
}
