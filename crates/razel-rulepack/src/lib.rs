//! `razel-rulepack` — the yidl-lite rule-pack layer (`dev-docs/RazelV2Contracts.md` §8): a rule
//! expressed as **data** (a [`RuleDecl`]) — its provider schemas (§8a) and its `provides`
//! propagation query (§8b) — interpreted by a *generic engine* over the DDS, not hand-written
//! per rule. The hand-coded cc/rust/py/sh logic becomes declarations like [`cc_decl`].
//!
//! First slice: §8a (schemas) + §8b (the `Own`/`Fold` propagation query). The actions UDF
//! (§8c — the `Args` invocation builder), matchers (§8d), and capability declaration (§8e)
//! are reserved for later slices.

pub mod constrain;

use razel_dds::{
    Dds, DdsRead, FieldId, FieldKind, FieldValue, ProviderSchema, ProviderTypeId, Scalar, TargetKey,
};
use std::collections::BTreeSet;

/// How a provider field's value is produced (§8b — the propagation query, as data).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldSource {
    /// The target's own value (from its attrs) — read directly, not propagated.
    Own,
    /// The transitive closure over deps of the same provider+field (a Set fold over the graph).
    Fold,
}

/// One field's propagation rule within a provider (§8b).
#[derive(Clone, Debug)]
pub struct Provides {
    pub provider: ProviderTypeId,
    pub field: FieldId,
    pub source: FieldSource,
}

/// A rule's declaration (§8a+b): the providers it produces (schemas) + how each field
/// propagates. Rules are data; the generic engine ([`register`] + [`query_set`]) interprets them
/// — so a new rule is a `RuleDecl`, not new control flow.
#[derive(Clone, Debug, Default)]
pub struct RuleDecl {
    pub kind: String,
    pub providers: Vec<(ProviderTypeId, ProviderSchema)>,
    pub provides: Vec<Provides>,
}

/// Register a rule's declared provider schemas into the DDS (§8a) — replaces the hand-coded
/// `register_schema` calls.
pub fn register(decl: &RuleDecl, dds: &mut Dds) {
    for (ty, schema) in &decl.providers {
        dds.register_schema(ty.clone(), schema.clone());
    }
}

/// Query a `Set`-valued field per the rule's `provides` declaration (§8b): `Fold` recomputes
/// the transitive closure via the DDS fold; `Own` (or an undeclared field) reads the target's
/// own provider field directly. The declaration — not bespoke code — decides which.
pub fn query_set(
    decl: &RuleDecl,
    dds: &Dds,
    target: &TargetKey,
    provider: &ProviderTypeId,
    field: &FieldId,
) -> BTreeSet<Scalar> {
    let source = decl
        .provides
        .iter()
        .find(|p| &p.provider == provider && &p.field == field)
        .map(|p| p.source)
        .unwrap_or(FieldSource::Own);
    match source {
        FieldSource::Fold => dds.fold_set(target, provider, field),
        FieldSource::Own => match dds.provider(target, provider).and_then(|p| p.get(field)) {
            Some(FieldValue::Set(s)) => s.clone(),
            _ => BTreeSet::new(),
        },
    }
}

/// The `cc_library`/`cc_binary` rule as data: a `DefaultInfo` (own `files`) + a `CcInfo` whose
/// `hdrs`/`cflags` propagate transitively (`Fold`). This is the hand-written `wire_to_dds`
/// behaviour expressed declaratively.
pub fn cc_decl() -> RuleDecl {
    let p = ProviderTypeId::new;
    let f = FieldId::new;
    RuleDecl {
        kind: "cc".into(),
        providers: vec![
            (p("DefaultInfo"), ProviderSchema::new().field(f("files"), FieldKind::Set)),
            (
                p("CcInfo"),
                ProviderSchema::new()
                    .field(f("hdrs"), FieldKind::Set)
                    .field(f("cflags"), FieldKind::Set),
            ),
        ],
        provides: vec![
            Provides { provider: p("DefaultInfo"), field: f("files"), source: FieldSource::Own },
            Provides { provider: p("CcInfo"), field: f("hdrs"), source: FieldSource::Fold },
            Provides { provider: p("CcInfo"), field: f("cflags"), source: FieldSource::Fold },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use razel_core::Label;
    use razel_dds::{InstanceId, Provider};

    fn tk(l: &str) -> TargetKey {
        TargetKey::new(InstanceId::SINGLE, Label::parse_canonical(l).unwrap())
    }
    fn cc() -> ProviderTypeId {
        ProviderTypeId::new("CcInfo")
    }
    fn di() -> ProviderTypeId {
        ProviderTypeId::new("DefaultInfo")
    }
    fn set(xs: &[&str]) -> FieldValue {
        FieldValue::Set(xs.iter().map(|s| Scalar::Str(s.to_string())).collect())
    }
    fn want(xs: &[&str]) -> BTreeSet<Scalar> {
        xs.iter().map(|s| Scalar::Str(s.to_string())).collect()
    }

    #[test]
    fn cc_decl_drives_register_and_own_vs_fold_query() {
        let decl = cc_decl();
        let mut dds = Dds::new();
        register(&decl, &mut dds); // schemas come from the declaration, not hand-coded
        let (hdrs, files) = (FieldId::new("hdrs"), FieldId::new("files"));
        let (base, util) = (tk("//:base"), tk("//:util"));
        // own facts: base owns base.h; util owns util.h + dep base; util's DefaultInfo = libutil.a
        dds.assert(base.clone(), cc(), Provider::new().with(hdrs.clone(), set(&["base.h"]))).unwrap();
        dds.assert(util.clone(), cc(), Provider::new().with(hdrs.clone(), set(&["util.h"]))).unwrap();
        dds.assert(util.clone(), di(), Provider::new().with(files.clone(), set(&["libutil.a"]))).unwrap();
        dds.assert_deps(util.clone(), vec![base]);
        // The DECLARATION decides: CcInfo.hdrs is Fold → transitive; DefaultInfo.files is Own → direct.
        assert_eq!(query_set(&decl, &dds, &util, &cc(), &hdrs), want(&["base.h", "util.h"]));
        assert_eq!(query_set(&decl, &dds, &util, &di(), &files), want(&["libutil.a"]));
    }

    #[test]
    fn undeclared_or_absent_field_reads_as_own_empty() {
        let decl = cc_decl();
        let dds = Dds::new();
        assert!(query_set(&decl, &dds, &tk("//:x"), &cc(), &FieldId::new("hdrs")).is_empty());
    }
}
