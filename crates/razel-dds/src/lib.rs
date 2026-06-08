//! `razel-dds` — the **Data Definition System**: razel's typed, in-memory fact store and the
//! spine of V2 (`dev-docs/RazelV2Contracts.md` §0/§1/§2/§3). Rule packs *declare into* it, the
//! assembler *asserts* into it, the engine *queries* it. It is razel's answer to **AD2** (no
//! ambient state): a passed, scoped value — never a global. (The `Session` the loader threads
//! today is its embryo; this crate is the real, typed form it graduates into.)
//!
//! **AD2 ↔ AD3 via the read/write split (§0).** Producers are *pure*: they receive
//! [`DdsRead`] (query only) and *return* facts; only the assembler holds `&mut Dds` and
//! [`Dds::assert`]s them. A producer handed `&dyn DdsRead` physically cannot mutate the store —
//! the easy path is the forced path.
//!
//! **V1 value algebra (§3).** [`FieldValue`] is `Scalar` (confluent: equal-or-conflict) and
//! `Set` (union, dedup). `Map` and the 4-order `OrderedDepset` family are reserved — they land
//! as additive enum variants, so nothing here is a migration. Providers are **atomic facts**:
//! one per `(TargetKey, ProviderTypeId)`, merged field-by-field on re-assert.

use razel_core::Label;
use std::collections::{BTreeMap, BTreeSet};

// ── Identity (§1) ────────────────────────────────────────────────────────────────

/// The analysis instance a fact belongs to. V1 is single-instance ([`InstanceId::SINGLE`]),
/// but every key carries it so multi-instance (**F24**: the same graph instantiated N times
/// for N platforms/configs) is an additive change, not a migration.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct InstanceId(pub u64);

impl InstanceId {
    /// The single default instance (V1). Multi-instance derivation assigns distinct ids.
    pub const SINGLE: InstanceId = InstanceId(0);
}

/// A configured target's identity: `(instance, label)` (§1). The same label under two
/// instances is two distinct keys — no collision.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct TargetKey {
    pub instance: InstanceId,
    pub label: Label,
}

impl TargetKey {
    pub fn new(instance: InstanceId, label: Label) -> Self {
        TargetKey { instance, label }
    }
}

/// Stable identity of a provider *type* (§1, e.g. `CcInfo`). V1 keys by stable name; the
/// versioned `ProviderSchemaId` (schema-compat lookup) is reserved.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct ProviderTypeId(pub String);

impl ProviderTypeId {
    pub fn new(s: impl Into<String>) -> Self {
        ProviderTypeId(s.into())
    }
}

/// A provider field name, namespaced within its provider (§2 `FieldId`).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct FieldId(pub String);

impl FieldId {
    pub fn new(s: impl Into<String>) -> Self {
        FieldId(s.into())
    }
}

// ── Value algebra (§2/§3) ──────────────────────────────────────────────────────────

/// A scalar leaf — the confluent arm of the closed `FieldType` (§2).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Scalar {
    Str(String),
    Int(i64),
    Bool(bool),
}

/// A provider field's value carrying its merge class (§3). V1: `Scalar` (confluent) and `Set`
/// (union). `Map`/`OrderedDepset` are reserved — added later as variants (additive).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FieldValue {
    /// Confluent scalar: merging equal values is a no-op; differing values are a conflict.
    Scalar(Scalar),
    /// Set: merge by union (dedup), order-independent.
    Set(BTreeSet<Scalar>),
}

/// The merge class a provider field *declares* (§2) — the asserted [`FieldValue`] must match.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldKind {
    Scalar,
    Set,
}

/// Why an [`Dds::assert`] failed — the value algebra surfacing a violation rather than a silent
/// last-writer-wins (F12: completeness is correctness).
#[derive(Debug, PartialEq, Eq)]
pub enum MergeError {
    /// Two producers asserted incompatible scalars for the same field (confluence violated).
    ScalarConflict { provider: ProviderTypeId, field: FieldId },
    /// Two producers asserted different merge classes for the same field.
    KindMismatch { provider: ProviderTypeId, field: FieldId },
    /// A provider type with no registered schema was asserted (the unregistered-provider forcing).
    UnregisteredProvider(ProviderTypeId),
    /// A field absent from the provider's schema, or whose value's merge class doesn't match
    /// the declared [`FieldKind`].
    SchemaViolation { provider: ProviderTypeId, field: FieldId },
}

impl FieldValue {
    /// This value's merge class — must match the field's declared [`FieldKind`].
    fn kind(&self) -> FieldKind {
        match self {
            FieldValue::Scalar(_) => FieldKind::Scalar,
            FieldValue::Set(_) => FieldKind::Set,
        }
    }

    /// Merge `other` into `self` per the field's monoid (§3). Confluent + commutative.
    fn merge(
        &mut self,
        other: FieldValue,
        prov: &ProviderTypeId,
        field: &FieldId,
    ) -> Result<(), MergeError> {
        match (self, other) {
            (FieldValue::Scalar(a), FieldValue::Scalar(b)) => {
                if *a == b {
                    Ok(())
                } else {
                    Err(MergeError::ScalarConflict { provider: prov.clone(), field: field.clone() })
                }
            }
            (FieldValue::Set(a), FieldValue::Set(b)) => {
                a.extend(b);
                Ok(())
            }
            _ => Err(MergeError::KindMismatch { provider: prov.clone(), field: field.clone() }),
        }
    }
}

// ── Providers (atomic facts) ───────────────────────────────────────────────────────

/// A provider instance — an **atomic fact** (§3), one per `(TargetKey, ProviderTypeId)`. Each
/// field carries its own merge class; re-asserting the same provider merges field-by-field.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Provider {
    pub fields: BTreeMap<FieldId, FieldValue>,
}

impl Provider {
    pub fn new() -> Self {
        Provider::default()
    }
    /// Builder: set a field (overwrites within this in-construction provider).
    pub fn with(mut self, field: FieldId, value: FieldValue) -> Self {
        self.fields.insert(field, value);
        self
    }
    pub fn get(&self, field: &FieldId) -> Option<&FieldValue> {
        self.fields.get(field)
    }
}

/// A provider type's declared **schema** (§2): each field's name + merge class. Rule packs
/// declare these; the DDS rejects any fact for an unregistered provider, or an unknown /
/// wrong-kind field — a typo'd provider/field can't silently land (F12).
#[derive(Clone, Debug, Default)]
pub struct ProviderSchema {
    fields: BTreeMap<FieldId, FieldKind>,
}

impl ProviderSchema {
    pub fn new() -> Self {
        ProviderSchema::default()
    }
    /// Builder: declare a field and its merge class.
    pub fn field(mut self, id: FieldId, kind: FieldKind) -> Self {
        self.fields.insert(id, kind);
        self
    }
}

// ── The store + the read/write split (§0) ───────────────────────────────────────────

/// Read access to the DDS — what **producers** get. Query only: no method can mutate the
/// store, so a producer handed `&dyn DdsRead` physically cannot assert. This is the type-level
/// forcing behind AD2/AD3 — producers are pure (read + *return* facts); only the assembler writes.
pub trait DdsRead {
    /// The provider of type `ty` on `target`, if any has been asserted.
    fn provider(&self, target: &TargetKey, ty: &ProviderTypeId) -> Option<&Provider>;
}

/// The fact store: atomic provider facts keyed by `(TargetKey, ProviderTypeId)`. Only code
/// holding `&mut Dds` (the assembler) can [`assert`](Dds::assert); producers get `&dyn DdsRead`.
#[derive(Default)]
pub struct Dds {
    providers: BTreeMap<(TargetKey, ProviderTypeId), Provider>,
    schemas: BTreeMap<ProviderTypeId, ProviderSchema>,
}

impl Dds {
    pub fn new() -> Self {
        Dds::default()
    }

    /// Declare a provider type's schema (§2). Rule packs call this before asserting; asserting
    /// an unregistered provider (or an unknown/wrong-kind field) is rejected.
    pub fn register_schema(&mut self, ty: ProviderTypeId, schema: ProviderSchema) {
        self.schemas.insert(ty, schema);
    }

    /// Assert a provider fact (the WRITE capability — assembler only). If a provider of the
    /// same type already exists on the target, the two merge field-by-field per the value
    /// algebra (§3); a confluence violation is a [`MergeError`], surfaced — never a silent
    /// last-writer-wins (F12).
    pub fn assert(
        &mut self,
        target: TargetKey,
        ty: ProviderTypeId,
        provider: Provider,
    ) -> Result<(), MergeError> {
        // Schema check (the forcing): the provider must be registered and every field must
        // match its declared kind — else the fact is rejected, never silently stored (F12).
        match self.schemas.get(&ty) {
            None => return Err(MergeError::UnregisteredProvider(ty.clone())),
            Some(schema) => {
                for (field, value) in &provider.fields {
                    if schema.fields.get(field).copied() != Some(value.kind()) {
                        return Err(MergeError::SchemaViolation {
                            provider: ty.clone(),
                            field: field.clone(),
                        });
                    }
                }
            }
        }
        match self.providers.get_mut(&(target.clone(), ty.clone())) {
            None => {
                self.providers.insert((target, ty), provider);
                Ok(())
            }
            Some(existing) => {
                for (field, value) in provider.fields {
                    match existing.fields.get_mut(&field) {
                        None => {
                            existing.fields.insert(field, value);
                        }
                        Some(cur) => cur.merge(value, &ty, &field)?,
                    }
                }
                Ok(())
            }
        }
    }

    /// Number of provider facts asserted (across all targets/instances).
    pub fn len(&self) -> usize {
        self.providers.len()
    }
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

impl DdsRead for Dds {
    fn provider(&self, target: &TargetKey, ty: &ProviderTypeId) -> Option<&Provider> {
        self.providers.get(&(target.clone(), ty.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tk(label: &str) -> TargetKey {
        TargetKey::new(InstanceId::SINGLE, Label::parse_canonical(label).unwrap())
    }
    fn cc() -> ProviderTypeId {
        ProviderTypeId::new("CcInfo")
    }
    fn set(xs: &[&str]) -> FieldValue {
        FieldValue::Set(xs.iter().map(|s| Scalar::Str(s.to_string())).collect())
    }
    /// A `Dds` with a `CcInfo` schema registered for the fields the tests assert.
    fn dds() -> Dds {
        let mut d = Dds::new();
        d.register_schema(
            cc(),
            ProviderSchema::new()
                .field(FieldId::new("lib"), FieldKind::Scalar)
                .field(FieldId::new("hdrs"), FieldKind::Set)
                .field(FieldId::new("k"), FieldKind::Scalar)
                .field(FieldId::new("derived"), FieldKind::Scalar),
        );
        d
    }

    #[test]
    fn assert_and_query_roundtrip() {
        let mut dds = dds();
        let p = Provider::new()
            .with(FieldId::new("lib"), FieldValue::Scalar(Scalar::Str("libfoo.a".into())));
        dds.assert(tk("//foo:bar"), cc(), p.clone()).unwrap();
        assert_eq!(dds.provider(&tk("//foo:bar"), &cc()), Some(&p));
        assert_eq!(dds.provider(&tk("//foo:other"), &cc()), None);
    }

    #[test]
    fn set_field_merges_by_union() {
        let mut dds = dds();
        dds.assert(tk("//p:t"), cc(), Provider::new().with(FieldId::new("hdrs"), set(&["a.h"])))
            .unwrap();
        dds.assert(tk("//p:t"), cc(), Provider::new().with(FieldId::new("hdrs"), set(&["b.h", "a.h"])))
            .unwrap();
        assert_eq!(
            dds.provider(&tk("//p:t"), &cc()).unwrap().get(&FieldId::new("hdrs")),
            Some(&set(&["a.h", "b.h"]))
        );
    }

    #[test]
    fn conflicting_scalar_is_a_merge_error_not_silent() {
        let mut dds = dds();
        let s = |v: &str| {
            Provider::new().with(FieldId::new("k"), FieldValue::Scalar(Scalar::Str(v.into())))
        };
        dds.assert(tk("//p:t"), cc(), s("one")).unwrap();
        let err = dds.assert(tk("//p:t"), cc(), s("two")).unwrap_err();
        assert!(matches!(err, MergeError::ScalarConflict { .. }));
    }

    #[test]
    fn same_label_two_instances_no_collision() {
        let mut dds = dds();
        let l = || Label::parse_canonical("//p:t").unwrap();
        let k1 = TargetKey::new(InstanceId::SINGLE, l());
        let k2 = TargetKey::new(InstanceId(1), l());
        let v = |n| Provider::new().with(FieldId::new("k"), FieldValue::Scalar(Scalar::Int(n)));
        dds.assert(k1.clone(), cc(), v(1)).unwrap();
        dds.assert(k2.clone(), cc(), v(2)).unwrap(); // distinct instance → no merge/conflict
        assert_eq!(
            dds.provider(&k1, &cc()).unwrap().get(&FieldId::new("k")),
            Some(&FieldValue::Scalar(Scalar::Int(1)))
        );
        assert_eq!(
            dds.provider(&k2, &cc()).unwrap().get(&FieldId::new("k")),
            Some(&FieldValue::Scalar(Scalar::Int(2)))
        );
    }

    #[test]
    fn producers_are_pure_only_the_assembler_writes() {
        // A producer receives `&dyn DdsRead` — it reads existing facts and *returns* new ones;
        // it cannot assert (no `&mut`). The assembler asserts them. This is §0's read/write split.
        fn producer(dds: &dyn DdsRead, t: &TargetKey) -> Vec<(FieldId, FieldValue)> {
            let _can_read = dds.provider(t, &cc()); // read OK
            vec![(FieldId::new("derived"), FieldValue::Scalar(Scalar::Bool(true)))]
        }
        let mut dds = dds();
        let t = tk("//p:t");
        let facts = producer(&dds, &t); // immutable borrow released here
        let mut p = Provider::new();
        for (f, v) in facts {
            p = p.with(f, v);
        }
        dds.assert(t.clone(), cc(), p).unwrap();
        assert!(dds.provider(&t, &cc()).unwrap().get(&FieldId::new("derived")).is_some());
    }

    #[test]
    fn unregistered_provider_is_rejected() {
        // The forcing: no schema registered → the fact cannot land.
        let mut d = Dds::new();
        let p =
            Provider::new().with(FieldId::new("lib"), FieldValue::Scalar(Scalar::Str("x".into())));
        assert!(matches!(
            d.assert(tk("//p:t"), cc(), p).unwrap_err(),
            MergeError::UnregisteredProvider(_)
        ));
    }

    #[test]
    fn unknown_or_wrong_kind_field_is_rejected() {
        let mut d = dds(); // CcInfo schema: lib=Scalar, hdrs=Set, …
        // a field not in the schema
        let unknown =
            Provider::new().with(FieldId::new("nope"), FieldValue::Scalar(Scalar::Int(1)));
        assert!(matches!(
            d.assert(tk("//p:t"), cc(), unknown).unwrap_err(),
            MergeError::SchemaViolation { .. }
        ));
        // a declared field with the wrong merge class (`lib` is Scalar, asserted as Set)
        let wrong = Provider::new().with(FieldId::new("lib"), set(&["a"]));
        assert!(matches!(
            d.assert(tk("//p:t"), cc(), wrong).unwrap_err(),
            MergeError::SchemaViolation { .. }
        ));
    }
}
