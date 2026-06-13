//! Taut fact codec: round-trip fidelity, and the content `Digest` as a stable identity.

use razel_dds::{FieldId, FieldValue, ProviderTypeId, Scalar};
use razel_deps_engine::{decode_target, encode_target, snapshot_fingerprint, target_fingerprint};
use razel_loading::{AnalyzedAction, AnalyzedTarget};
use std::collections::{BTreeMap, BTreeSet};

/// A fact exercising every arm: actions, deps, default_info, and all `FieldValue`/`Scalar` shapes.
fn sample() -> AnalyzedTarget {
    let mut providers = BTreeMap::new();
    providers.insert(
        (ProviderTypeId::new("CcInfo"), FieldId::new("hdrs")),
        FieldValue::Set(BTreeSet::from([
            Scalar::Str("a.h".into()),
            Scalar::Str("b.h".into()),
        ])),
    );
    providers.insert(
        (ProviderTypeId::new("JavaInfo"), FieldId::new("compile_jars")),
        FieldValue::OrderedDepset(vec![Scalar::Str("x.jar".into()), Scalar::Str("y.jar".into())]),
    );
    providers.insert(
        (ProviderTypeId::new("JavaInfo"), FieldId::new("neverlink")),
        FieldValue::Scalar(Scalar::Bool(true)),
    );
    providers.insert(
        (ProviderTypeId::new("MyInfo"), FieldId::new("count")),
        FieldValue::Scalar(Scalar::Int(42)),
    );
    AnalyzedTarget {
        name: "//app:widget".into(),
        deps: vec!["//lib:a".into(), "//lib:b".into()],
        actions: vec![AnalyzedAction {
            mnemonic: "CppCompile".into(),
            argv: vec!["clang".into(), "-c".into(), "widget.c".into()],
            inputs: vec!["widget.c".into()],
            outputs: vec!["widget.o".into()],
        }],
        default_info: vec!["widget.o".into()],
        providers,
    }
}

#[test]
fn fact_round_trips_through_taut_bytes() {
    let t = sample();
    let bytes = encode_target(&t);
    let back = decode_target(&bytes).expect("decode");
    assert_eq!(back, t, "the fact survives encode→decode unchanged");
}

#[test]
fn equal_facts_have_equal_fingerprints_distinct_facts_differ() {
    // Determinism: the same fact, encoded twice, is byte- and digest-identical.
    let t = sample();
    assert_eq!(encode_target(&t), encode_target(&sample()));
    assert_eq!(target_fingerprint(&t), target_fingerprint(&sample()));

    // Any semantic change moves the digest (it IS the content identity).
    let mut renamed = sample();
    renamed.name = "//app:other".into();
    assert_ne!(target_fingerprint(&t), target_fingerprint(&renamed));

    let mut more_provider = sample();
    more_provider.set_provider("MyInfo", "extra", FieldValue::Scalar(Scalar::Int(1)));
    assert_ne!(target_fingerprint(&t), target_fingerprint(&more_provider));
}

#[test]
fn snapshot_fingerprint_is_order_sensitive_and_stable() {
    let a = sample();
    let mut b = sample();
    b.name = "//app:b".into();

    // Stable for the same ordered set…
    assert_eq!(
        snapshot_fingerprint(&[a.clone(), b.clone()]),
        snapshot_fingerprint(&[a.clone(), b.clone()])
    );
    // …and sensitive to order (length-prefixing prevents fact-boundary aliasing).
    assert_ne!(
        snapshot_fingerprint(&[a.clone(), b.clone()]),
        snapshot_fingerprint(&[b, a])
    );
}

#[test]
fn decode_rejects_wrong_shape() {
    use razel_wire::{encode, Cbor};
    // Well-formed taut, but not a fact (a version-skew guard). Byte-level corruption is NOT this
    // codec's job — the cache verifies the content `Digest` before ever decoding, so only
    // well-formed, correctly-addressed bytes reach here.
    assert!(decode_target(&encode(&Cbor::Int(5))).is_err());
    assert!(decode_target(&encode(&Cbor::Array(vec![Cbor::Text("name-only".into())]))).is_err());
}
