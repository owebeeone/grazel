//! The producer/assembler model — the AD2/AD3 read/write split made type-level.
//!
//! A [`Producer`] is handed a [`ProducerCtx`] giving read-only access to already-asserted facts
//! (`&dyn DdsRead`, including `fold_set` over deps) + this target's declared attrs, and RETURNS
//! its own provider facts. It cannot assert — there is no `&mut Dds` in scope. [`assemble`] is the
//! sole holder of `&mut Dds`: it drives targets in dependency order, calls each producer, and
//! asserts what they return (+ the dep edges). So "no ambient state" (AD2) and "forcing" (AD3)
//! are reconciled structurally — only the assembler writes; producers are pure queries.
//!
//! Unlike [`crate::wire_to_dds`] (which converts already-analyzed, pre-propagated targets and
//! subtracts deps to recover OWN), a producer works from RAW declared attrs — so its `CcInfo` is
//! OWN by construction (the declared `hdrs`), and the transitive closure is `fold_set`.

use razel_core::Label;
use razel_dds::{
    Dds, DdsRead, FieldId, FieldKind, FieldValue, InstanceId, Provider, ProviderSchema,
    ProviderTypeId, Scalar, TargetKey,
};

/// A target's raw declared attributes (the rule call's inputs — not pre-propagated).
#[derive(Debug, Clone, Default)]
pub struct TargetAttrs {
    pub name: String,
    pub srcs: Vec<String>,
    pub hdrs: Vec<String>,
    pub cflags: Vec<String>,
    pub deps: Vec<String>,
}

/// Read-side context for a [`Producer`]: query already-asserted facts (deps, via `dds` — including
/// `fold_set`) + this target's declared attrs. There is no `&mut Dds` — a producer cannot assert.
pub struct ProducerCtx<'a> {
    pub target: TargetKey,
    pub attrs: &'a TargetAttrs,
    pub dds: &'a dyn DdsRead,
}

/// A rule expressed as a pure producer of provider facts.
pub trait Producer {
    /// The provider facts this target contributes — OWN (non-transitive); the closure is the fold.
    fn produce(&self, ctx: &ProducerCtx) -> Vec<(ProviderTypeId, Provider)>;
}

/// The `cc_library` rule as a producer: a `CcInfo` carrying its OWN exported headers + cflags, and
/// a `DefaultInfo` carrying its archive output. (Consumers — the compile action, `cc_binary` —
/// recover transitive headers via `ctx.dds.fold_set`, not from this provider.)
pub struct CcLibrary;

impl Producer for CcLibrary {
    fn produce(&self, ctx: &ProducerCtx) -> Vec<(ProviderTypeId, Provider)> {
        let lib = format!("lib{}.a", ctx.attrs.name);
        vec![
            (
                ProviderTypeId::new("DefaultInfo"),
                Provider::new().with(FieldId::new("files"), str_set(&[lib])),
            ),
            (
                ProviderTypeId::new("CcInfo"),
                Provider::new()
                    .with(FieldId::new("hdrs"), str_set(&ctx.attrs.hdrs))
                    .with(FieldId::new("cflags"), str_set(&ctx.attrs.cflags)),
            ),
        ]
    }
}

/// Assemble a [`Dds`] from `(attrs, producer)` declarations given in dependency order (deps before
/// dependents, so a producer's `dds` queries see its deps' facts). The assembler is the ONLY
/// holder of `&mut Dds`: producers return facts; it asserts them + the dep edges. The registered
/// schemas are the rule-pack's declaration (§2).
pub fn assemble(instance: InstanceId, decls: &[(TargetAttrs, &dyn Producer)]) -> Result<Dds, String> {
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

    for (attrs, producer) in decls {
        let key = target_key(instance, &attrs.name)?;
        // Read-only produce: the producer queries `&dyn DdsRead` and RETURNS facts (no mutation —
        // the borrow ends here, before any assert).
        let facts = producer.produce(&ProducerCtx { target: key.clone(), attrs, dds: &dds });
        // Write: only the assembler asserts.
        for (ptype, prov) in facts {
            dds.assert(key.clone(), ptype, prov).map_err(|e| format!("{e:?}"))?;
        }
        let dep_keys =
            attrs.deps.iter().map(|d| target_key(instance, d)).collect::<Result<Vec<_>, _>>()?;
        dds.assert_deps(key, dep_keys);
    }
    Ok(dds)
}

fn target_key(instance: InstanceId, name: &str) -> Result<TargetKey, String> {
    Label::parse_canonical(name)
        .or_else(|_| Label::parse_canonical(&format!("//:{name}")))
        .map(|label| TargetKey::new(instance, label))
        .map_err(|e| format!("{e}"))
}

fn str_set(xs: &[String]) -> FieldValue {
    FieldValue::Set(xs.iter().map(|s| Scalar::Str(s.clone())).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn set(xs: &[&str]) -> FieldValue {
        FieldValue::Set(xs.iter().map(|s| Scalar::Str(s.to_string())).collect())
    }
    fn key(n: &str) -> TargetKey {
        TargetKey::new(InstanceId::SINGLE, Label::parse_canonical(&format!("//:{n}")).unwrap())
    }

    #[test]
    fn producer_assembler_split_builds_cc_facts_and_fold() {
        let base = TargetAttrs {
            name: "base".into(),
            srcs: vec!["base.cc".into()],
            hdrs: vec!["base.h".into()],
            ..Default::default()
        };
        let util = TargetAttrs {
            name: "util".into(),
            srcs: vec!["util.cc".into()],
            hdrs: vec!["util.h".into()],
            deps: vec!["base".into()],
            ..Default::default()
        };
        let cc = CcLibrary;
        let dds = assemble(InstanceId::SINGLE, &[(base, &cc), (util, &cc)]).unwrap();

        let cci = ProviderTypeId::new("CcInfo");
        let hdrs = FieldId::new("hdrs");
        // OWN CcInfo straight from raw attrs (no dep-subtraction): util.h only.
        assert_eq!(dds.provider(&key("util"), &cci).unwrap().get(&hdrs), Some(&set(&["util.h"])));
        // DefaultInfo = the archive.
        assert_eq!(
            dds.provider(&key("util"), &ProviderTypeId::new("DefaultInfo"))
                .unwrap()
                .get(&FieldId::new("files")),
            Some(&set(&["libutil.a"]))
        );
        // Transitive headers = the fold over deps: {base.h, util.h} (propagation-as-query).
        let want: BTreeSet<Scalar> =
            ["base.h", "util.h"].iter().map(|s| Scalar::Str(s.to_string())).collect();
        assert_eq!(dds.fold_set(&key("util"), &cci, &hdrs), want);
    }
}
