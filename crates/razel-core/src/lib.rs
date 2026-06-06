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

/// A parsed Bazel **canonical** label: `@@repo//package:name` (repo `""` = main repo).
/// Edge cases ported from Bazel `cmdline/LabelTest.java` (Class E). Canonical form only;
/// repo-context resolution (`parseWithRepoContext`, relative `:t`/`pkg`) is deferred —
/// it needs a repo map (Phase 2 follow-up / F6).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Label {
    repo: String,
    package: String,
    name: String,
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
        &self.repo
    }
    pub fn package_name(&self) -> &str {
        &self.package
    }
    pub fn name(&self) -> &str {
        &self.name
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
                    repo: rest.to_string(),
                    package: String::new(),
                    name: rest.to_string(),
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
            repo: repo.to_string(),
            package,
            name,
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
