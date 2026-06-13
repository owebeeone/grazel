//! Taut serialization of the analysis FACT (`AnalyzedTarget`) — the keystone primitive behind
//! the V2 cache and cross-worker sharing. This is the Bazel move, verified in their source:
//! `ObjectCodec.serialize(.., CodedOutputStream)` → `PackedFingerprint` (serialize via the wire
//! codec, content-address by the hash of the bytes). Here razel's **taut/CBOR** substrate
//! (`razel-wire`) is the encoder and razel-core's blake3 [`Digest`] is the fingerprint.
//!
//! Why it's the keystone: a serialized fact is `Send` **bytes** — live Starlark `Value`s are not.
//! So it can (a) cross worker boundaries, letting a worker read another's frozen result instead of
//! re-analyzing the shared spine (the 6-thread re-analysis cap measured at ~5000 fallbacks), and
//! (b) persist across invocations keyed by its content `Digest` (the incremental-cache win — the
//! reason a second Bazel build is fast). Both build directly on this round-trip.
//!
//! Encoding is deterministic — `providers` is a `BTreeMap` and `Set` a `BTreeSet`, and taut's CBOR
//! is canonical (definite length, shortest-form ints) — so equal facts produce equal bytes, hence
//! equal `Digest`s. That determinism is the whole point: the digest IS the identity.

use razel_core::Digest;
use razel_dds::{FieldId, FieldValue, ProviderTypeId, Scalar};
use razel_loading::{AnalyzedAction, AnalyzedTarget};
use razel_wire::{decode, encode, Cbor};

// ---- encode ---------------------------------------------------------------------

fn texts(xs: &[String]) -> Cbor {
    Cbor::Array(xs.iter().map(|s| Cbor::Text(s.clone())).collect())
}

fn scalar_cbor(s: &Scalar) -> Cbor {
    match s {
        Scalar::Str(x) => Cbor::Array(vec![Cbor::Int(0), Cbor::Text(x.clone())]),
        Scalar::Int(n) => Cbor::Array(vec![Cbor::Int(1), Cbor::Int(*n)]),
        Scalar::Bool(b) => Cbor::Array(vec![Cbor::Int(2), Cbor::Bool(*b)]),
    }
}

fn field_value_cbor(v: &FieldValue) -> Cbor {
    match v {
        FieldValue::Scalar(s) => Cbor::Array(vec![Cbor::Int(0), scalar_cbor(s)]),
        FieldValue::Set(set) => {
            Cbor::Array(vec![Cbor::Int(1), Cbor::Array(set.iter().map(scalar_cbor).collect())])
        }
        FieldValue::OrderedDepset(xs) => {
            Cbor::Array(vec![Cbor::Int(2), Cbor::Array(xs.iter().map(scalar_cbor).collect())])
        }
    }
}

fn action_cbor(a: &AnalyzedAction) -> Cbor {
    Cbor::Array(vec![
        Cbor::Text(a.mnemonic.clone()),
        texts(&a.argv),
        texts(&a.inputs),
        texts(&a.outputs),
    ])
}

fn target_cbor(t: &AnalyzedTarget) -> Cbor {
    let providers = Cbor::Array(
        t.providers
            .iter()
            .map(|((ty, field), val)| {
                Cbor::Array(vec![
                    Cbor::Text(ty.0.clone()),
                    Cbor::Text(field.0.clone()),
                    field_value_cbor(val),
                ])
            })
            .collect(),
    );
    Cbor::Array(vec![
        Cbor::Text(t.name.clone()),
        texts(&t.deps),
        Cbor::Array(t.actions.iter().map(action_cbor).collect()),
        texts(&t.default_info),
        providers,
    ])
}

/// Canonical taut bytes for one analysis fact.
pub fn encode_target(t: &AnalyzedTarget) -> Vec<u8> {
    encode(&target_cbor(t))
}

/// Content fingerprint of one fact — blake3 over its taut bytes. Its content-addressed identity.
pub fn target_fingerprint(t: &AnalyzedTarget) -> Digest {
    Digest::of(&encode_target(t))
}

/// Canonical taut bytes for a whole committed snapshot (the ordered fact set) — the stored form
/// of the content-addressed cache. Length-prefixed so distinct fact boundaries cannot alias.
pub fn encode_snapshot(targets: &[AnalyzedTarget]) -> Vec<u8> {
    let mut buf = Vec::new();
    for t in targets {
        let bytes = encode_target(t);
        buf.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        buf.extend_from_slice(&bytes);
    }
    buf
}

/// Content fingerprint of a committed snapshot — the basis for a content-addressed `SnapshotId`
/// and the cache key.
pub fn snapshot_fingerprint(targets: &[AnalyzedTarget]) -> Digest {
    Digest::of(&encode_snapshot(targets))
}

/// Reconstruct a whole snapshot from its taut bytes (the cache-hit / cross-worker-load path).
pub fn decode_snapshot(bytes: &[u8]) -> R<Vec<AnalyzedTarget>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let len_end = i + 8;
        let len_bytes = bytes.get(i..len_end).ok_or("snapshot: truncated length")?;
        let len = u64::from_le_bytes(len_bytes.try_into().unwrap()) as usize;
        let fact_end = len_end + len;
        let fact = bytes.get(len_end..fact_end).ok_or("snapshot: truncated fact")?;
        out.push(decode_target(fact)?);
        i = fact_end;
    }
    Ok(out)
}

// ---- decode (the round-trip inverse) --------------------------------------------

type R<T> = Result<T, String>;

fn at(a: &[Cbor], i: usize) -> R<&Cbor> {
    a.get(i).ok_or_else(|| format!("fact: missing field {i}"))
}
fn as_array(c: &Cbor) -> R<&[Cbor]> {
    match c {
        Cbor::Array(a) => Ok(a),
        _ => Err("fact: expected array".into()),
    }
}
fn as_text(c: &Cbor) -> R<String> {
    match c {
        Cbor::Text(s) => Ok(s.clone()),
        _ => Err("fact: expected text".into()),
    }
}
fn as_int(c: &Cbor) -> R<i64> {
    match c {
        Cbor::Int(n) => Ok(*n),
        _ => Err("fact: expected int".into()),
    }
}
fn as_bool(c: &Cbor) -> R<bool> {
    match c {
        Cbor::Bool(b) => Ok(*b),
        _ => Err("fact: expected bool".into()),
    }
}
fn texts_of(c: &Cbor) -> R<Vec<String>> {
    as_array(c)?.iter().map(as_text).collect()
}

fn scalar_of(c: &Cbor) -> R<Scalar> {
    let a = as_array(c)?;
    match as_int(at(a, 0)?)? {
        0 => Ok(Scalar::Str(as_text(at(a, 1)?)?)),
        1 => Ok(Scalar::Int(as_int(at(a, 1)?)?)),
        2 => Ok(Scalar::Bool(as_bool(at(a, 1)?)?)),
        t => Err(format!("fact: bad scalar tag {t}")),
    }
}

fn field_value_of(c: &Cbor) -> R<FieldValue> {
    let a = as_array(c)?;
    let payload = at(a, 1)?;
    match as_int(at(a, 0)?)? {
        0 => Ok(FieldValue::Scalar(scalar_of(payload)?)),
        1 => Ok(FieldValue::Set(as_array(payload)?.iter().map(scalar_of).collect::<R<_>>()?)),
        2 => Ok(FieldValue::OrderedDepset(
            as_array(payload)?.iter().map(scalar_of).collect::<R<_>>()?,
        )),
        t => Err(format!("fact: bad field-value tag {t}")),
    }
}

fn action_of(c: &Cbor) -> R<AnalyzedAction> {
    let a = as_array(c)?;
    Ok(AnalyzedAction {
        mnemonic: as_text(at(a, 0)?)?,
        argv: texts_of(at(a, 1)?)?,
        inputs: texts_of(at(a, 2)?)?,
        outputs: texts_of(at(a, 3)?)?,
    })
}

/// Reconstruct a fact from its taut bytes. `Err` on malformed/incompatible bytes (never panics).
pub fn decode_target(bytes: &[u8]) -> R<AnalyzedTarget> {
    let c = decode(bytes);
    let a = as_array(&c)?;
    let actions = as_array(at(a, 2)?)?.iter().map(action_of).collect::<R<_>>()?;
    let mut providers = std::collections::BTreeMap::new();
    for p in as_array(at(a, 4)?)? {
        let pa = as_array(p)?;
        let ty = ProviderTypeId(as_text(at(pa, 0)?)?);
        let field = FieldId(as_text(at(pa, 1)?)?);
        providers.insert((ty, field), field_value_of(at(pa, 2)?)?);
    }
    Ok(AnalyzedTarget {
        name: as_text(at(a, 0)?)?,
        deps: texts_of(at(a, 1)?)?,
        actions,
        default_info: texts_of(at(a, 3)?)?,
        providers,
    })
}
