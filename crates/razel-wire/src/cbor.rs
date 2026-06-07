//! Minimal deterministic CBOR — the frozen wire substrate.
//!
//! Ported byte-for-byte from taut's `wire/cbor.py`: a tiny subset of RFC 8949 in
//! core deterministic encoding (§4.2.1) — definite-length items, shortest-form
//! integer arguments, map keys emitted in ascending order. Major types:
//!
//!   0/1  unsigned / negative integer
//!   2    byte string
//!   3    text string (utf-8)
//!   4    array
//!   5    map (non-negative integer keys only — field tags)
//!   7    simple: false (0xf4), true (0xf5), null (0xf6)
//!
//! This is the whole vocabulary taut freezes — no floats, tags, indefinite
//! lengths, or big-nums. The Python/TS/C++ bindings encode the *same* subset; the
//! golden corpus is byte-exact across all of them.
//!
//! HAND-ROLLED, NO serde/ciborium — and deliberately so. taut's contract is
//! byte-exact cross-language parity pinned by the corpus; serde's derive encodes
//! by field *name* and would not reproduce the other languages' bytes. Do **not**
//! "modernize" this with a derive macro: it would silently break the corpus.

/// A decoded CBOR value, restricted to taut's frozen subset.
#[derive(Clone, Debug, PartialEq)]
pub enum Cbor {
    Int(i64),
    Text(String),
    Bytes(Vec<u8>),
    Bool(bool),
    Array(Vec<Cbor>),
    /// Map with non-negative integer keys (field tags). Order is irrelevant in
    /// memory; `encode` emits keys ascending for determinism.
    Map(Vec<(i64, Cbor)>),
    Null,
}

/// Shared `Null` so `get` can hand back a reference for an absent tag.
static NULL: Cbor = Cbor::Null;

impl Cbor {
    /// Map lookup by field tag. A missing tag returns `&Null` — so a decoder
    /// treats "absent" and "explicit null" identically (forward/backward compat).
    pub fn get(&self, tag: i64) -> &Cbor {
        match self {
            Cbor::Map(items) => items
                .iter()
                .find(|(k, _)| *k == tag)
                .map(|(_, v)| v)
                .unwrap_or(&NULL),
            _ => &NULL,
        }
    }

    pub fn int(&self) -> i64 {
        match self {
            Cbor::Int(n) => *n,
            _ => panic!("cbor: expected int, got {self:?}"),
        }
    }

    pub fn text(&self) -> String {
        match self {
            Cbor::Text(s) => s.clone(),
            _ => panic!("cbor: expected text, got {self:?}"),
        }
    }

    pub fn bytes(&self) -> Vec<u8> {
        match self {
            Cbor::Bytes(b) => b.clone(),
            _ => panic!("cbor: expected bytes, got {self:?}"),
        }
    }

    pub fn boolean(&self) -> bool {
        match self {
            Cbor::Bool(b) => *b,
            _ => panic!("cbor: expected bool, got {self:?}"),
        }
    }

    pub fn array(&self) -> &[Cbor] {
        match self {
            Cbor::Array(a) => a,
            _ => panic!("cbor: expected array, got {self:?}"),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Cbor::Null)
    }
}

// --- encode -----------------------------------------------------------------

/// Major-type byte + shortest-form argument for a non-negative `n`.
fn head(major: u8, n: u64, out: &mut Vec<u8>) {
    let mt = major << 5;
    if n < 24 {
        out.push(mt | n as u8);
    } else if n < 0x100 {
        out.push(mt | 24);
        out.push(n as u8);
    } else if n < 0x1_0000 {
        out.push(mt | 25);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else if n < 0x1_0000_0000 {
        out.push(mt | 26);
        out.extend_from_slice(&(n as u32).to_be_bytes());
    } else {
        out.push(mt | 27);
        out.extend_from_slice(&n.to_be_bytes());
    }
}

/// Encode a [`Cbor`] value to deterministic CBOR bytes.
pub fn encode(value: &Cbor) -> Vec<u8> {
    let mut out = Vec::new();
    enc(value, &mut out);
    out
}

fn enc(value: &Cbor, out: &mut Vec<u8>) {
    match value {
        Cbor::Null => out.push(0xF6),
        Cbor::Bool(true) => out.push(0xF5),
        Cbor::Bool(false) => out.push(0xF4),
        Cbor::Int(n) => {
            if *n >= 0 {
                head(0, *n as u64, out);
            } else {
                head(1, (-1 - *n) as u64, out);
            }
        }
        Cbor::Bytes(b) => {
            head(2, b.len() as u64, out);
            out.extend_from_slice(b);
        }
        Cbor::Text(s) => {
            let b = s.as_bytes();
            head(3, b.len() as u64, out);
            out.extend_from_slice(b);
        }
        Cbor::Array(items) => {
            head(4, items.len() as u64, out);
            for it in items {
                enc(it, out);
            }
        }
        Cbor::Map(entries) => {
            // Deterministic: integer keys ascending, shortest form.
            let mut sorted: Vec<&(i64, Cbor)> = entries.iter().collect();
            sorted.sort_by_key(|(k, _)| *k);
            head(5, sorted.len() as u64, out);
            for (k, v) in sorted {
                assert!(
                    *k >= 0,
                    "cbor: map keys (field tags) must be non-negative, got {k}"
                );
                head(0, *k as u64, out);
                enc(v, out);
            }
        }
    }
}

// --- decode -----------------------------------------------------------------

/// Decode deterministic CBOR bytes into a [`Cbor`] value. Panics on bytes
/// outside the frozen subset or trailing data — the wire is ours, not arbitrary.
pub fn decode(data: &[u8]) -> Cbor {
    let (v, off) = dec(data, 0);
    assert_eq!(off, data.len(), "cbor: trailing bytes after top-level item");
    v
}

fn read_arg(data: &[u8], off: usize, info: u8) -> (u64, usize) {
    match info {
        n if n < 24 => (n as u64, off),
        24 => (data[off] as u64, off + 1),
        25 => (
            u16::from_be_bytes([data[off], data[off + 1]]) as u64,
            off + 2,
        ),
        26 => (
            u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]) as u64,
            off + 4,
        ),
        27 => {
            let mut b = [0u8; 8];
            b.copy_from_slice(&data[off..off + 8]);
            (u64::from_be_bytes(b), off + 8)
        }
        _ => panic!("cbor: unsupported additional-info {info}"),
    }
}

fn dec(data: &[u8], off: usize) -> (Cbor, usize) {
    let initial = data[off];
    let major = initial >> 5;
    let info = initial & 0x1F;
    let off = off + 1;
    match major {
        0 => {
            let (n, o) = read_arg(data, off, info);
            (Cbor::Int(n as i64), o)
        }
        1 => {
            let (n, o) = read_arg(data, off, info);
            (Cbor::Int(-1 - n as i64), o)
        }
        2 => {
            let (n, o) = read_arg(data, off, info);
            let n = n as usize;
            (Cbor::Bytes(data[o..o + n].to_vec()), o + n)
        }
        3 => {
            let (n, o) = read_arg(data, off, info);
            let n = n as usize;
            let s = String::from_utf8(data[o..o + n].to_vec()).expect("cbor: invalid utf-8");
            (Cbor::Text(s), o + n)
        }
        4 => {
            let (n, mut o) = read_arg(data, off, info);
            let mut items = Vec::with_capacity(n as usize);
            for _ in 0..n {
                let (it, no) = dec(data, o);
                items.push(it);
                o = no;
            }
            (Cbor::Array(items), o)
        }
        5 => {
            let (n, mut o) = read_arg(data, off, info);
            let mut entries = Vec::with_capacity(n as usize);
            for _ in 0..n {
                let (k, ko) = dec(data, o);
                let (v, vo) = dec(data, ko);
                entries.push((k.int(), v));
                o = vo;
            }
            (Cbor::Map(entries), o)
        }
        7 => match info {
            20 => (Cbor::Bool(false), off),
            21 => (Cbor::Bool(true), off),
            22 => (Cbor::Null, off),
            _ => panic!("cbor: unsupported simple value {info}"),
        },
        _ => panic!("cbor: unsupported major type {major}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(v: &Cbor) -> String {
        encode(v).iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn rfc8949_appendix_a_vectors() {
        // Canonical RFC 8949 Appendix A vectors for the frozen subset.
        assert_eq!(hex(&Cbor::Int(0)), "00");
        assert_eq!(hex(&Cbor::Int(1)), "01");
        assert_eq!(hex(&Cbor::Int(10)), "0a");
        assert_eq!(hex(&Cbor::Int(23)), "17");
        assert_eq!(hex(&Cbor::Int(24)), "1818");
        assert_eq!(hex(&Cbor::Int(100)), "1864");
        assert_eq!(hex(&Cbor::Int(1000)), "1903e8");
        assert_eq!(hex(&Cbor::Int(1_000_000)), "1a000f4240");
        assert_eq!(hex(&Cbor::Int(-1)), "20");
        assert_eq!(hex(&Cbor::Int(-100)), "3863");
        assert_eq!(hex(&Cbor::Text(String::new())), "60");
        assert_eq!(hex(&Cbor::Text("a".into())), "6161");
        assert_eq!(hex(&Cbor::Text("IETF".into())), "6449455446");
        assert_eq!(hex(&Cbor::Bytes(vec![])), "40");
        assert_eq!(hex(&Cbor::Bytes(vec![1, 2, 3, 4])), "4401020304");
        assert_eq!(hex(&Cbor::Bool(false)), "f4");
        assert_eq!(hex(&Cbor::Bool(true)), "f5");
        assert_eq!(hex(&Cbor::Null), "f6");
        assert_eq!(hex(&Cbor::Array(vec![])), "80");
        assert_eq!(
            hex(&Cbor::Array(vec![Cbor::Int(1), Cbor::Int(2), Cbor::Int(3)])),
            "83010203"
        );
        // map {1: 2} -> a1 01 02
        assert_eq!(hex(&Cbor::Map(vec![(1, Cbor::Int(2))])), "a10102");
    }

    #[test]
    fn map_keys_are_emitted_ascending() {
        // Out-of-order entries must encode identically to sorted ones.
        let unsorted = Cbor::Map(vec![
            (3, Cbor::Int(30)),
            (1, Cbor::Int(10)),
            (2, Cbor::Int(20)),
        ]);
        let sorted = Cbor::Map(vec![
            (1, Cbor::Int(10)),
            (2, Cbor::Int(20)),
            (3, Cbor::Int(30)),
        ]);
        assert_eq!(encode(&unsorted), encode(&sorted));
        assert_eq!(hex(&sorted), "a3010a021403181e"); // {1:10, 2:20, 3:30}
    }

    #[test]
    fn roundtrips_every_kind() {
        let values = vec![
            Cbor::Int(0),
            Cbor::Int(-42),
            Cbor::Int(1_000_000),
            Cbor::Text("héllo".into()),
            Cbor::Bytes(vec![0, 255, 128]),
            Cbor::Bool(true),
            Cbor::Bool(false),
            Cbor::Null,
            Cbor::Array(vec![Cbor::Int(1), Cbor::Text("x".into())]),
            Cbor::Map(vec![
                (1, Cbor::Text("a".into())),
                (2, Cbor::Array(vec![Cbor::Int(9)])),
            ]),
        ];
        for v in values {
            assert_eq!(decode(&encode(&v)), v, "roundtrip failed for {v:?}");
        }
    }

    #[test]
    fn negative_int_boundaries() {
        assert_eq!(hex(&Cbor::Int(-24)), "37");
        assert_eq!(hex(&Cbor::Int(-25)), "3818");
        assert_eq!(
            decode(&encode(&Cbor::Int(i64::MIN + 1))),
            Cbor::Int(i64::MIN + 1)
        );
    }
}
