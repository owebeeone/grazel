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
use std::collections::BTreeMap;
use razel_dds::{
    Dds, FieldId, FieldValue, InstanceId, Provider, ProviderTypeId,
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

/// Build a [`Dds`] fact store from the analyzed targets: register the cc/java provider schemas, then
/// assert each target's OWN providers + its dep edges. Transitive provider queries are then folds
/// over this store (`DdsRead`), not a bespoke loader traversal.
pub fn to_dds(targets: &[AnalyzedTarget], instance: InstanceId) -> Result<Dds, String> {
    let mut dds = Dds::new();
    // C3a.2: schemas come from the provider registry, not a hardcoded cc/java list — the engine no
    // longer enumerates the languages here (the registry is the source of truth).
    let registry = crate::registry::builtin_registry();
    for ty in registry.provider_types() {
        if let Some(schema) = registry.schema(ty) {
            dds.register_schema(ty.clone(), schema);
        }
    }

    for t in targets {
        let key = target_key(instance, &t.name)?;
        let e = |m: razel_dds::MergeError| format!("{m:?}");
        dds.assert(
            key.clone(),
            ProviderTypeId::new("DefaultInfo"),
            Provider::new().with(FieldId::new("files"), set(&t.default_info)),
        )
        .map_err(e)?;
        // C2d: the providers are already DDS values — group the target's OWN map by provider type
        // and assert each. No flat-field reflection; the storage IS the algebra.
        let mut by_ty: BTreeMap<ProviderTypeId, Vec<(FieldId, FieldValue)>> = BTreeMap::new();
        for ((ty, fid), val) in &t.providers {
            by_ty.entry(ty.clone()).or_default().push((fid.clone(), val.clone()));
        }
        for (ty, fields) in by_ty {
            let p = fields.into_iter().fold(Provider::new(), |p, (f, v)| p.with(f, v));
            dds.assert(key.clone(), ty, p).map_err(e)?;
        }
        let deps = t.deps.iter().map(|d| target_key(instance, d)).collect::<Result<Vec<_>, _>>()?;
        dds.assert_deps(key, deps);
    }
    Ok(dds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use razel_dds::DdsRead;

    fn sset(xs: &[&str]) -> FieldValue {
        FieldValue::Set(xs.iter().map(|s| Scalar::Str(s.to_string())).collect())
    }
    fn odep(xs: &[&str]) -> FieldValue {
        FieldValue::OrderedDepset(xs.iter().map(|s| Scalar::Str(s.to_string())).collect())
    }
    fn at(name: &str, deps: &[&str], hdrs: &[&str], cj: &[&str]) -> AnalyzedTarget {
        let mut t = AnalyzedTarget {
            name: name.into(),
            deps: deps.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        t.set_provider("CcInfo", "hdrs", sset(hdrs));
        t.set_provider("JavaInfo", "compile_jars", odep(cj));
        t
    }
    fn strs(xs: Vec<Scalar>) -> Vec<String> {
        xs.into_iter().map(|s| if let Scalar::Str(x) = s { x } else { unreachable!() }).collect()
    }

    #[test]
    fn dds_fold_is_the_provider_fold() {
        // diamond: app -> {a, b} -> base. This is THE loader fold now (C2c) — assert its semantics.
        let ts = vec![
            at("base", &[], &["base.h"], &["base.jar"]),
            at("a", &["base"], &["a.h"], &["a.jar"]),
            at("b", &["base"], &["b.h"], &["b.jar"]),
            at("app", &["a", "b"], &["app.h"], &["app.jar"]),
        ];
        let dds = to_dds(&ts, InstanceId::SINGLE).unwrap();
        let key = target_key(InstanceId::SINGLE, "app").unwrap();

        // Set (cc headers): diamond deduped (base.h once), order-independent → sorted.
        let mut h = strs(
            dds.fold_set(&key, &ProviderTypeId::new("CcInfo"), &FieldId::new("hdrs")).into_iter().collect(),
        );
        h.sort();
        assert_eq!(h, ["a.h", "app.h", "b.h", "base.h"]);

        // OrderedDepset (java compile jars): preorder (app, a's closure, b), base deduped first-win.
        let cj = strs(dds.fold_depset(&key, &ProviderTypeId::new("JavaInfo"), &FieldId::new("compile_jars")));
        assert_eq!(cj, ["app.jar", "a.jar", "base.jar", "b.jar"]);
    }

    #[test]
    fn dds_pruned_fold_drops_the_neverlink_subtree() {
        // app -> api(neverlink) -> hidden: the runtime closure prunes api + its hidden subtree (C2b).
        let mk = |name: &str, deps: &[&str], rj: &[&str], never: bool| {
            let mut t = AnalyzedTarget {
                name: name.into(),
                deps: deps.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            };
            t.set_provider("JavaInfo", "runtime_jars", odep(rj));
            t.set_provider("JavaInfo", "neverlink", FieldValue::Scalar(Scalar::Bool(never)));
            t
        };
        let ts = vec![
            mk("hidden", &[], &["hidden.jar"], false),
            mk("api", &["hidden"], &["api.jar"], true),
            mk("app", &["api"], &["app.jar"], false),
        ];
        let dds = to_dds(&ts, InstanceId::SINGLE).unwrap();
        let key = target_key(InstanceId::SINGLE, "app").unwrap();
        let rj = strs(dds.fold_depset_pruned(
            &key,
            &ProviderTypeId::new("JavaInfo"),
            &FieldId::new("runtime_jars"),
            &FieldId::new("neverlink"),
        ));
        assert_eq!(rj, ["app.jar"], "neverlink api + its hidden subtree pruned from runtime");
    }
}
