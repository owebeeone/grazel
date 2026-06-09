//! The loader → DDS bridge (C2). razel's provider model IS the `razel-dds` value algebra: each
//! analyzed target's OWN providers become `FieldValue` facts (`Set` for cc headers/cflags,
//! `OrderedDepset` for java jars, `Scalar` for neverlink), and the TRANSITIVE closure a dependent
//! sees is recovered by `DdsRead::{fold_set, fold_depset}` — the ONE fold, replacing the loader's
//! parallel `providers::fold_field` (the F3 spine reconciliation).
//!
//! C2a establishes the bridge + proves `DdsRead` reproduces the loader fold (cc `Set` + java compile
//! `OrderedDepset`). The runtime-jar `neverlink` subtree-skip needs a `fold_depset` extension in
//! razel-dds (C2b); switching the live dep-resolution onto this + deleting `fold_field` is C2c.

use crate::state::AnalyzedTarget;
use razel_core::Label;
use razel_dds::{
    Dds, FieldId, FieldKind, FieldValue, InstanceId, Provider, ProviderSchema, ProviderTypeId,
    Scalar, TargetKey,
};

/// Canonicalize a loader target name into a [`TargetKey`] (single-package bare names → `//:name`).
pub fn target_key(instance: InstanceId, name: &str) -> Result<TargetKey, String> {
    Label::parse_canonical(name)
        .or_else(|_| Label::parse_canonical(&format!("//:{name}")))
        .map(|l| TargetKey::new(instance, l))
        .map_err(|e| format!("{e}"))
}

fn set(xs: &[String]) -> FieldValue {
    FieldValue::Set(xs.iter().map(|s| Scalar::Str(s.clone())).collect())
}
fn ordered(xs: &[String]) -> FieldValue {
    FieldValue::OrderedDepset(xs.iter().map(|s| Scalar::Str(s.clone())).collect())
}

/// Build a [`Dds`] fact store from the analyzed targets: register the cc/java provider schemas, then
/// assert each target's OWN providers + its dep edges. Transitive provider queries are then folds
/// over this store (`DdsRead`), not a bespoke loader traversal.
pub fn to_dds(targets: &[AnalyzedTarget], instance: InstanceId) -> Result<Dds, String> {
    let mut dds = Dds::new();
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
    dds.register_schema(
        ProviderTypeId::new("JavaInfo"),
        ProviderSchema::new()
            .field(FieldId::new("compile_jars"), FieldKind::OrderedDepset)
            .field(FieldId::new("runtime_jars"), FieldKind::OrderedDepset)
            .field(FieldId::new("neverlink"), FieldKind::Scalar),
    );

    for t in targets {
        let key = target_key(instance, &t.name)?;
        let e = |m: razel_dds::MergeError| format!("{m:?}");
        dds.assert(
            key.clone(),
            ProviderTypeId::new("DefaultInfo"),
            Provider::new().with(FieldId::new("files"), set(&t.default_info)),
        )
        .map_err(e)?;
        if !t.hdrs.is_empty() || !t.cflags.is_empty() {
            dds.assert(
                key.clone(),
                ProviderTypeId::new("CcInfo"),
                Provider::new()
                    .with(FieldId::new("hdrs"), set(&t.hdrs))
                    .with(FieldId::new("cflags"), set(&t.cflags)),
            )
            .map_err(e)?;
        }
        if !t.compile_jars.is_empty() || !t.runtime_jars.is_empty() || t.neverlink {
            dds.assert(
                key.clone(),
                ProviderTypeId::new("JavaInfo"),
                Provider::new()
                    .with(FieldId::new("compile_jars"), ordered(&t.compile_jars))
                    .with(FieldId::new("runtime_jars"), ordered(&t.runtime_jars))
                    .with(FieldId::new("neverlink"), FieldValue::Scalar(Scalar::Bool(t.neverlink))),
            )
            .map_err(e)?;
        }
        let deps = t.deps.iter().map(|d| target_key(instance, d)).collect::<Result<Vec<_>, _>>()?;
        dds.assert_deps(key, deps);
    }
    Ok(dds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{fold_compile_jars, fold_headers};
    use razel_dds::DdsRead;
    use std::collections::BTreeMap;

    fn at(name: &str, deps: &[&str], hdrs: &[&str], cj: &[&str]) -> AnalyzedTarget {
        AnalyzedTarget {
            name: name.into(),
            deps: deps.iter().map(|s| s.to_string()).collect(),
            hdrs: hdrs.iter().map(|s| s.to_string()).collect(),
            compile_jars: cj.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }
    fn strs(xs: Vec<Scalar>) -> Vec<String> {
        xs.into_iter().map(|s| if let Scalar::Str(x) = s { x } else { unreachable!() }).collect()
    }

    #[test]
    fn dds_fold_reproduces_the_loader_fold() {
        // diamond: app -> {a, b} -> base. The DDS fold must equal the loader's fold_field.
        let ts = vec![
            at("base", &[], &["base.h"], &["base.jar"]),
            at("a", &["base"], &["a.h"], &["a.jar"]),
            at("b", &["base"], &["b.h"], &["b.jar"]),
            at("app", &["a", "b"], &["app.h"], &["app.jar"]),
        ];
        let results: BTreeMap<String, AnalyzedTarget> =
            ts.iter().map(|t| (t.name.clone(), t.clone())).collect();
        let dds = to_dds(&ts, InstanceId::SINGLE).unwrap();
        let key = target_key(InstanceId::SINGLE, "app").unwrap();

        // Set (cc headers): both deduped over the diamond (base.h once); compare as sorted sets.
        let mut dds_h = strs(dds.fold_set(&key, &ProviderTypeId::new("CcInfo"), &FieldId::new("hdrs")).into_iter().collect());
        dds_h.sort();
        let mut loader_h = fold_headers(&results, "app");
        loader_h.sort();
        assert_eq!(dds_h, loader_h, "DDS fold_set must reproduce the loader's header fold");

        // OrderedDepset (java compile jars): preorder + first-occurrence dedup.
        let dds_cj = strs(dds.fold_depset(&key, &ProviderTypeId::new("JavaInfo"), &FieldId::new("compile_jars")));
        assert_eq!(dds_cj, fold_compile_jars(&results, "app"), "DDS fold_depset must reproduce the loader's compile-jar fold");
    }
}
