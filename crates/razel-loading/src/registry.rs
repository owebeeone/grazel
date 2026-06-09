//! The provider-schema registry (Phase C3 / C3a) — the single source of truth for provider schemas,
//! which fields propagate to dependents (and how), and the dep-struct projection names. Per-language
//! ruleset modules register here; the generic engine (`to_dds`, the two dep-folds, `razel_build.info`)
//! reads it — so adding a language becomes a registration, not an edit to the engine core.
//!
//! C3a.1 defines + unit-tests the registry. It gets wired into `to_dds` + capture (C3a.2) and the two
//! dep-folds (C3a.3); until then it is `#![allow(dead_code)]` — about-to-be-wired, not abandoned. AD2:
//! built per analysis, never a process global.
#![allow(dead_code)]

use razel_dds::{FieldId, FieldKind, ProviderSchema, ProviderTypeId};
use std::collections::BTreeMap;

/// How a dep-propagated field folds over the transitive dep graph.
pub(crate) enum FoldPolicy {
    /// `Set` union / `OrderedDepset` preorder — the default.
    Plain,
    /// Prune a node AND its subtree when its `<FieldId>` `Scalar(Bool(true))` holds — java `neverlink`
    /// (compile-only deps drop from the runtime closure). Drives `DdsRead::fold_depset_pruned`.
    PrunedBy(FieldId),
}

/// How a provider field propagates to dependents.
pub(crate) struct DepFold {
    /// The name the dep struct exposes this field under — the `.bzl` ABI. `CcInfo.hdrs` is read as
    /// `dep.headers`, so `hdrs`'s projection is `"headers"`: the rename lives HERE, not in a literal.
    pub projection: &'static str,
    pub policy: FoldPolicy,
}

/// One provider field: its merge kind + (if it propagates) how it folds onto dependents.
pub(crate) struct FieldSpec {
    pub kind: FieldKind,
    /// `Some` ⇒ folded + exposed to dependents; `None` ⇒ own-only (e.g. `neverlink`, a prune driver).
    pub dep_fold: Option<DepFold>,
}

/// Provider type → its fields. The generic engine reads this instead of hardcoding cc/java.
#[derive(Default)]
pub(crate) struct ProviderRegistry {
    providers: BTreeMap<ProviderTypeId, BTreeMap<FieldId, FieldSpec>>,
}

impl ProviderRegistry {
    pub(crate) fn register(&mut self, provider: &str, field: &str, spec: FieldSpec) {
        self.providers
            .entry(ProviderTypeId::new(provider))
            .or_default()
            .insert(FieldId::new(field), spec);
    }
    /// Every registered provider type — so `to_dds` registers each schema.
    pub(crate) fn provider_types(&self) -> impl Iterator<Item = &ProviderTypeId> {
        self.providers.keys()
    }
    /// The DDS schema (field → kind) for a provider — for `to_dds` registration.
    pub(crate) fn schema(&self, provider: &ProviderTypeId) -> Option<ProviderSchema> {
        self.providers.get(provider).map(|fields| {
            fields.iter().fold(ProviderSchema::new(), |s, (f, spec)| s.field(f.clone(), spec.kind))
        })
    }
    /// A provider's dep-propagated fields: `(field, &DepFold)` — drives the two dep-folds (C3a.3).
    pub(crate) fn dep_fields<'a>(
        &'a self,
        provider: &ProviderTypeId,
    ) -> impl Iterator<Item = (&'a FieldId, &'a DepFold)> {
        self.providers
            .get(provider)
            .into_iter()
            .flat_map(|m| m.iter().filter_map(|(f, s)| s.dep_fold.as_ref().map(|d| (f, d))))
    }
}

/// razel's bundled providers (the universal `DefaultInfo` + cc + java). A real per-language
/// registration seam (the ruleset modules registering) lands as the engine reads this (C3a.2/3); py's
/// `PyInfo` joins with its untangle off the `CcInfo.hdrs` channel (C3a.2).
pub(crate) fn builtin_registry() -> ProviderRegistry {
    let mut r = ProviderRegistry::default();
    let folded = |projection, policy| Some(DepFold { projection, policy });
    // DefaultInfo — every target's output files.
    r.register("DefaultInfo", "files", FieldSpec { kind: FieldKind::Set, dep_fold: folded("files", FoldPolicy::Plain) });
    // cc — exported headers + compile flags (Sets); `CcInfo.hdrs` is read as `dep.headers`.
    r.register("CcInfo", "hdrs", FieldSpec { kind: FieldKind::Set, dep_fold: folded("headers", FoldPolicy::Plain) });
    r.register("CcInfo", "cflags", FieldSpec { kind: FieldKind::Set, dep_fold: folded("cflags", FoldPolicy::Plain) });
    // java — ordered compile/runtime classpaths + the neverlink prune flag (own-only).
    r.register("JavaInfo", "compile_jars", FieldSpec { kind: FieldKind::OrderedDepset, dep_fold: folded("compile_jars", FoldPolicy::Plain) });
    r.register("JavaInfo", "runtime_jars", FieldSpec { kind: FieldKind::OrderedDepset, dep_fold: folded("runtime_jars", FoldPolicy::PrunedBy(FieldId::new("neverlink"))) });
    r.register("JavaInfo", "neverlink", FieldSpec { kind: FieldKind::Scalar, dep_fold: None });
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_models_cc_and_java() {
        let r = builtin_registry();
        // Schemas exist for every provider to_dds will register.
        for ty in ["DefaultInfo", "CcInfo", "JavaInfo"] {
            assert!(r.schema(&ProviderTypeId::new(ty)).is_some(), "schema for {ty}");
        }
        // Dep-folded fields: cc has hdrs+cflags; java has compile_jars+runtime_jars (neverlink own-only).
        assert_eq!(r.dep_fields(&ProviderTypeId::new("CcInfo")).count(), 2);
        let java = ProviderTypeId::new("JavaInfo");
        assert_eq!(r.dep_fields(&java).count(), 2, "neverlink is own-only, not dep-folded");
        // The neverlink subtree-prune is registered on runtime_jars; CcInfo.hdrs projects to "headers".
        assert!(r.dep_fields(&java).any(|(_, d)| matches!(d.policy, FoldPolicy::PrunedBy(_))));
        assert!(r
            .dep_fields(&ProviderTypeId::new("CcInfo"))
            .any(|(f, d)| *f == FieldId::new("hdrs") && d.projection == "headers"));
    }
}
