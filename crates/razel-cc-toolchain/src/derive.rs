//! Derive a cc target's DECLARED action graph from DDS facts — the read side of the
//! producer/assembler model. The transitive headers are a `fold_set` query over `CcInfo.hdrs`;
//! each action's argv + output layout come from §8c. So the declared graph is a QUERY over the
//! asserted providers, not a hand-built action list — propagation-as-query end to end.

use crate::{
    FeatureConfig, bazel_archive_inputs, bazel_compile_inputs, cc_archive_argv, cc_compile_argv,
};
use razel_dds::{DdsRead, FieldId, ProviderTypeId, Scalar, TargetKey};

/// A declared action (mnemonic + command line + inputs/outputs) — razel's Bazel-faithful declared
/// graph node, derived from facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredAction {
    pub mnemonic: String,
    pub argv: Vec<String>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

/// Derive a `cc_library` target's declared actions from the DDS: one `CppCompile` per source
/// (inputs = the source + the transitive headers `fold_set`-ed from `CcInfo.hdrs`; argv + output
/// layout via §8c), then one `CppArchive` over the objects. `pkg`/`cfg`/`sdk` are the path-model +
/// host params. The DDS is read-only (`&dyn DdsRead`).
pub fn derive_cc_library_actions(
    dds: &dyn DdsRead,
    config: &FeatureConfig,
    target: &TargetKey,
    pkg: &str,
    name: &str,
    srcs: &[&str],
    cfg: &str,
    sdk: &str,
) -> Vec<DeclaredAction> {
    // Transitive headers (own + deps) via the fold — propagation-as-query — package-qualified.
    let headers: Vec<String> = dds
        .fold_set(target, &ProviderTypeId::new("CcInfo"), &FieldId::new("hdrs"))
        .into_iter()
        .filter_map(|s| match s {
            Scalar::Str(h) => Some(format!("{pkg}/{h}")),
            _ => None,
        })
        .collect();

    let mut actions = Vec::new();
    for src in srcs {
        let ci = bazel_compile_inputs(cfg, pkg, name, src, sdk);
        let mut inputs = headers.clone();
        inputs.push(ci.source_file.clone());
        inputs.sort();
        actions.push(DeclaredAction {
            mnemonic: "CppCompile".into(),
            argv: cc_compile_argv(config, &ci),
            inputs,
            outputs: vec![ci.dependency_file.clone(), ci.output_file.clone()],
        });
    }
    let ai = bazel_archive_inputs(cfg, pkg, name, srcs);
    actions.push(DeclaredAction {
        mnemonic: "CppArchive".into(),
        argv: cc_archive_argv(config, &ai),
        inputs: ai.libraries_to_link.clone(),
        outputs: vec![ai.output_execpath.clone()],
    });
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macos_core_config;
    use razel_core::Label;
    use razel_dds::{Dds, FieldKind, FieldValue, InstanceId, Provider, ProviderSchema};

    fn key(n: &str) -> TargetKey {
        TargetKey::new(InstanceId::SINGLE, Label::parse_canonical(&format!("//:{n}")).unwrap())
    }

    /// A DDS holding util→base CcInfo facts (OWN headers + the dep edge) — as the assembler leaves
    /// it. `derive_*` is the read-only consumer.
    fn cc_facts() -> Dds {
        let mut dds = Dds::new();
        dds.register_schema(
            ProviderTypeId::new("CcInfo"),
            ProviderSchema::new()
                .field(FieldId::new("hdrs"), FieldKind::Set)
                .field(FieldId::new("cflags"), FieldKind::Set),
        );
        let cc = |h: &str| {
            Provider::new()
                .with(FieldId::new("hdrs"), FieldValue::Set([Scalar::Str(h.into())].into_iter().collect()))
                .with(FieldId::new("cflags"), FieldValue::Set(Default::default()))
        };
        dds.assert(key("base"), ProviderTypeId::new("CcInfo"), cc("base.h")).unwrap();
        dds.assert(key("util"), ProviderTypeId::new("CcInfo"), cc("util.h")).unwrap();
        dds.assert_deps(key("util"), vec![key("base")]);
        dds
    }

    #[test]
    fn derives_cc_actions_from_dds_facts() {
        let dds = cc_facts();
        let cfg = macos_core_config().unwrap();
        let actions = derive_cc_library_actions(
            &dds,
            &cfg,
            &key("util"),
            "corpus/cc/transitive",
            "util",
            &["util.cc"],
            "<cfg>",
            "<sdk>",
        );
        assert_eq!(actions.len(), 2);

        let compile = &actions[0];
        assert_eq!(compile.mnemonic, "CppCompile");
        assert_eq!(compile.argv[0], "external/<repo>/cc_wrapper.sh");
        assert!(compile.argv.contains(&"-std=c++17".to_string()));
        // Inputs = source + transitive headers (base.h via the fold), qualified + sorted.
        assert_eq!(
            compile.inputs,
            [
                "corpus/cc/transitive/base.h",
                "corpus/cc/transitive/util.cc",
                "corpus/cc/transitive/util.h",
            ]
        );
        assert_eq!(
            compile.outputs,
            [
                "bazel-out/<cfg>/bin/corpus/cc/transitive/_objs/util/util.d",
                "bazel-out/<cfg>/bin/corpus/cc/transitive/_objs/util/util.o",
            ]
        );

        let archive = &actions[1];
        assert_eq!(archive.mnemonic, "CppArchive");
        assert_eq!(archive.outputs, ["bazel-out/<cfg>/bin/corpus/cc/transitive/libutil.a"]);
    }
}
