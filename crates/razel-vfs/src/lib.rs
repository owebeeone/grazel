//! Content providers + copy-on-write views (F11 / knob #3).
//!
//! The engine's leaf input is a **content digest for (path, view, version)** supplied
//! by a *provider* — never a raw `std::fs` read. The disk is just one provider; editor
//! buffers and agent speculative edits are overlay providers. A `View` is a COW stack of
//! overlays over a base; an overlay write yields a NEW version and an isolated view, so
//! speculative builds don't touch the base or each other.

use razel_core::Digest;
use std::collections::HashMap;

/// A monotonically increasing snapshot version.
pub type Version = u64;

/// A provider answers "what is the digest of `path`?" for its layer.
pub trait ContentProvider {
    fn digest(&self, path: &str) -> Option<Digest>;
}

/// An in-memory base layer. Stands in for the disk provider (which would stat+hash with
/// the F4 fast-path); semantically identical from the engine's view — it yields digests.
#[derive(Default, Clone)]
pub struct MemBase {
    files: HashMap<String, Digest>,
}

impl MemBase {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set(&mut self, path: &str, content: &[u8]) {
        self.files.insert(path.to_string(), Digest::of(content));
    }
}

impl ContentProvider for MemBase {
    fn digest(&self, path: &str) -> Option<Digest> {
        self.files.get(path).copied()
    }
}

/// A copy-on-write view: a base provider plus overlay edits, stamped with a version.
/// Resolving a path consults overlays first, then the base.
pub struct View<'b> {
    base: &'b dyn ContentProvider,
    overlays: HashMap<String, Digest>,
    version: Version,
}

impl<'b> View<'b> {
    pub fn new(base: &'b dyn ContentProvider) -> Self {
        Self {
            base,
            overlays: HashMap::new(),
            version: 0,
        }
    }

    pub fn version(&self) -> Version {
        self.version
    }

    /// Overlay-write `content` at `path`; returns the new version. Does not mutate the
    /// base or any other view.
    pub fn write(&mut self, path: &str, content: &[u8]) -> Version {
        self.overlays.insert(path.to_string(), Digest::of(content));
        self.version += 1;
        self.version
    }

    /// Fork a child view (COW): shares the base, copies current overlays, inherits the
    /// version. Subsequent writes to either side are isolated — the basis for speculative
    /// per-agent builds.
    pub fn fork(&self) -> View<'b> {
        View {
            base: self.base,
            overlays: self.overlays.clone(),
            version: self.version,
        }
    }
}

impl ContentProvider for View<'_> {
    fn digest(&self, path: &str) -> Option<Digest> {
        self.overlays
            .get(path)
            .copied()
            .or_else(|| self.base.digest(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_reads_through_to_base() {
        let mut base = MemBase::new();
        base.set("foo", b"A");
        let v = View::new(&base);
        assert_eq!(v.digest("foo"), Some(Digest::of(b"A")));
        assert_eq!(v.digest("missing"), None);
    }

    #[test]
    fn overlay_write_bumps_version_and_overrides_base() {
        let mut base = MemBase::new();
        base.set("foo", b"A");
        let mut v = View::new(&base);
        assert_eq!(v.version(), 0);
        let ver = v.write("foo", b"B");
        assert_eq!(ver, 1);
        assert_eq!(v.version(), 1);
        assert_eq!(v.digest("foo"), Some(Digest::of(b"B")));
        // base is untouched.
        assert_eq!(base.digest("foo"), Some(Digest::of(b"A")));
    }

    #[test]
    fn forked_views_are_isolated() {
        let mut base = MemBase::new();
        base.set("foo", b"A");
        let v = View::new(&base);
        let mut speculative = v.fork();
        speculative.write("foo", b"SPEC");

        // speculative sees its overlay; parent + base unchanged (isolation).
        assert_eq!(speculative.digest("foo"), Some(Digest::of(b"SPEC")));
        assert_eq!(v.digest("foo"), Some(Digest::of(b"A")));
        assert_eq!(base.digest("foo"), Some(Digest::of(b"A")));
    }
}
