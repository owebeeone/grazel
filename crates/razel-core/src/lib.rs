//! Shared primitives: a content `Digest` (blake3, behind a swappable newtype — F3)
//! and the logical id newtypes used as stable node identities (§2.1 / F11).

use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

// ── Istr: shared string slice (Arc<str>, value semantics) ────────────────────────────
//
// The analysis hot path (DDS folds, dep-DAG traversal) CLONES the same path / label strings
// millions of times — a `sample` profile of the TF sweep put ~26% of eval in malloc/free,
// almost all of it String churn. `Istr` wraps `Arc<str>`: `Clone` is a refcount bump (no
// alloc), so the fold's per-step clones stop hitting malloc. Eq/Hash/Ord are VALUE-based
// (derived) — no hash-cons table, hence NO process-global ambient state (AD2/F13) and
// determinism is trivially preserved (`Ord` is the same byte order `String` had, so sorted
// sets / the taut content `Digest` are unchanged). A per-Session interner could additionally
// make Eq/Hash pointer ops (killing the memcmp half), but that needs threading the interner
// through every construction site — deferred; not worth the ambient-state cost yet.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Istr(Arc<str>);

impl Istr {
    pub fn new(s: &str) -> Self {
        Istr(Arc::from(s))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for Istr {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Istr {
    fn from(s: &str) -> Self {
        Istr(Arc::from(s))
    }
}
impl From<String> for Istr {
    fn from(s: String) -> Self {
        Istr(Arc::from(s))
    }
}
impl fmt::Display for Istr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl fmt::Debug for Istr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", &*self.0)
    }
}

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

/// A parsed Bazel **canonical** label: `@@repo//package:name` (repo `""` = main repo).
/// Edge cases ported from Bazel `cmdline/LabelTest.java` (Class E). Canonical form only;
/// repo-context resolution (`parseWithRepoContext`, relative `:t`/`pkg`) is deferred —
/// it needs a repo map (Phase 2 follow-up / F6).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Label {
    repo: Istr,
    package: Istr,
    name: Istr,
}

#[derive(Debug, PartialEq, Eq)]
pub struct LabelError(pub String);

impl fmt::Display for LabelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid label: {}", self.0)
    }
}
impl std::error::Error for LabelError {}

impl Label {
    /// `""` for the main repository.
    pub fn repository(&self) -> &str {
        self.repo.as_str()
    }
    pub fn package_name(&self) -> &str {
        self.package.as_str()
    }
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    /// Parse a canonical label: `@@repo//pkg:name`, `@repo//pkg:name`, `@repo`
    /// (= `@repo//:repo`), `//pkg:name`, `//pkg` (name defaults to the last package
    /// segment), `//:name` (empty package).
    pub fn parse_canonical(s: &str) -> Result<Label, LabelError> {
        let err = |m: &str| LabelError(format!("{m}: `{s}`"));

        let (rest, had_repo) = match s.strip_prefix("@@").or_else(|| s.strip_prefix('@')) {
            Some(r) => (r, true),
            None => (s, false),
        };

        if had_repo {
            match rest.find("//") {
                // `@foo` shorthand → repo=foo, pkg="", name="foo".
                None if rest.is_empty() => Err(err("empty repository name")),
                None => Ok(Label {
                    repo: rest.into(),
                    package: "".into(),
                    name: rest.into(),
                }),
                Some(idx) => {
                    let repo_name = &rest[..idx];
                    if repo_name.is_empty() {
                        return Err(err("empty repository name"));
                    }
                    Self::finish(repo_name, &rest[idx + 2..])
                        .ok_or_else(|| err("invalid label body"))
                }
            }
        } else {
            let body = s
                .strip_prefix("//")
                .ok_or_else(|| err("label must start with `//` or `@`"))?;
            Self::finish("", body).ok_or_else(|| err("invalid label body"))
        }
    }

    /// Parse the `package[:name]` body. Name defaults to the package's last segment.
    fn finish(repo: &str, body: &str) -> Option<Label> {
        let (package, name) = match body.split_once(':') {
            Some((pkg, name)) => (pkg.to_string(), name.to_string()),
            None => (
                body.to_string(),
                body.rsplit('/').next().unwrap_or("").to_string(),
            ),
        };
        if name.is_empty() {
            return None;
        }
        Some(Label {
            repo: repo.into(),
            package: package.into(),
            name: name.into(),
        })
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.repo.is_empty() {
            write!(f, "//{}:{}", self.package, self.name)
        } else {
            write!(f, "@@{}//{}:{}", self.repo, self.package, self.name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn istr_value_semantics_eq_ord_and_hashset_dedup() {
        let a = Istr::new("tensorflow/core/lib");
        let b = Istr::new(&String::from("tensorflow/core/lib")); // distinct source String
        let c = Istr::new("tensorflow/core");
        // Value equality (Arc<str> derives) — independent allocations, equal content.
        assert_eq!(a, b);
        assert_ne!(a, c);
        // Deref reads as the underlying str.
        assert_eq!(&*a, "tensorflow/core/lib");
        // Ord is VALUE-based — determinism for sorted sets / the taut content Digest.
        assert!(c < a, "'tensorflow/core' < 'tensorflow/core/lib' by value");
        // Hash agrees with Eq: equal Istrs collapse in a HashSet.
        let set: std::collections::HashSet<Istr> = [a.clone(), b, c].into_iter().collect();
        assert_eq!(set.len(), 2);
    }

    fn lbl(s: &str) -> Label {
        Label::parse_canonical(s).unwrap()
    }

    #[test]
    fn label_canonical_cases_from_bazel_labeltest() {
        let l = lbl("//foo/bar:baz");
        assert_eq!(
            (l.repository(), l.package_name(), l.name()),
            ("", "foo/bar", "baz")
        );
        assert_eq!(
            (lbl("//foo/bar").package_name(), lbl("//foo/bar").name()),
            ("foo/bar", "bar")
        );
        assert_eq!(
            (lbl("//:bar").package_name(), lbl("//:bar").name()),
            ("", "bar")
        );
        let l = lbl("@foo");
        assert_eq!(
            (l.repository(), l.package_name(), l.name()),
            ("foo", "", "foo")
        );
        let l = lbl("@foo//bar");
        assert_eq!(
            (l.repository(), l.package_name(), l.name()),
            ("foo", "bar", "bar")
        );
        let l = lbl("@@foo//bar");
        assert_eq!(
            (l.repository(), l.package_name(), l.name()),
            ("foo", "bar", "bar")
        );
        let l = lbl("//@foo");
        assert_eq!(
            (l.repository(), l.package_name(), l.name()),
            ("", "@foo", "@foo")
        );
        let l = lbl("//xyz/@foo:abc");
        assert_eq!((l.package_name(), l.name()), ("xyz/@foo", "abc"));
    }

    #[test]
    fn label_rejects_invalid() {
        assert!(Label::parse_canonical("").is_err());
        assert!(Label::parse_canonical("foo").is_err()); // relative — needs repo context
        assert!(Label::parse_canonical(":foo").is_err());
        assert!(Label::parse_canonical("//foo:").is_err()); // empty name
    }

    #[test]
    fn label_display_roundtrips() {
        assert_eq!(lbl("//foo/bar:baz").to_string(), "//foo/bar:baz");
        assert_eq!(lbl("@@foo//bar:bar").to_string(), "@@foo//bar:bar");
    }

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
