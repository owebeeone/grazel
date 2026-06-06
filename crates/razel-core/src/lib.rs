//! Shared primitives: a content `Digest` (blake3, behind a swappable newtype — F3)
//! and the logical id newtypes used as stable node identities (§2.1 / F11).

use std::fmt;

/// Content-addressed digest. F3: blake3 behind a newtype so the algorithm is swappable.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest([u8; 32]);

impl Digest {
    /// Digest of a byte slice.
    pub fn of(bytes: &[u8]) -> Self {
        Digest(blake3::hash(bytes).into())
    }
    pub fn from_bytes(b: [u8; 32]) -> Self {
        Digest(b)
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
    pub fn to_hex(&self) -> String {
        use fmt::Write as _;
        self.0.iter().fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({}…)", &self.to_hex()[..8])
    }
}

/// Logical, stable ids (§2.1): stable across edits — the content `Digest` is the value
/// that changes. Clients/agents reference these; the cache keys on the digest.
macro_rules! id_newtype {
    ($name:ident) => {
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
        pub struct $name(pub String);
        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

id_newtype!(FileId);
id_newtype!(ActionId);
id_newtype!(TargetId);

/// A reference to any node — used as the key for graph edges.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum NodeRef {
    File(FileId),
    Action(ActionId),
    Target(TargetId),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_deterministic_and_content_sensitive() {
        assert_eq!(Digest::of(b"hello"), Digest::of(b"hello"));
        assert_ne!(Digest::of(b"hello"), Digest::of(b"world"));
        assert_eq!(Digest::of(b"hello").to_hex().len(), 64);
    }

    #[test]
    fn ids_are_distinct_types_with_value_equality() {
        assert_eq!(FileId::new("a/b.rs"), FileId::new("a/b.rs"));
        assert_ne!(FileId::new("a"), FileId::new("b"));
        assert_eq!(TargetId::new("//x:y").to_string(), "//x:y");
    }
}
