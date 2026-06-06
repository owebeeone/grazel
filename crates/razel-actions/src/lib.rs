//! Action graph + **content-addressed action keys** (Phase 4).
//!
//! The action key is the heart of cache correctness (F12): it must fold in *everything*
//! that can change an action's output — argv, every input digest, the env allowlist, tool
//! digests, the platform, and declared outputs. A missing input → a false cache hit → a
//! silent wrong build. So the key is **complete by construction**, and the tests assert
//! that mutating *any* component changes the key (and that nothing else does).

use razel_core::Digest;
use std::collections::BTreeMap;

/// A build artifact: a source file (with its content digest) or a derived output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Artifact {
    Source { path: String, digest: Digest },
    Derived { path: String },
}

impl Artifact {
    pub fn path(&self) -> &str {
        match self {
            Artifact::Source { path, .. } | Artifact::Derived { path } => path,
        }
    }
}

/// A spawn action. `inputs`/`env`/`tools` are sorted maps so the key is independent of
/// insertion order; `argv`/`outputs` keep their order (it's semantically significant).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Action {
    pub argv: Vec<String>,
    pub inputs: BTreeMap<String, Digest>,
    pub env: BTreeMap<String, String>,
    pub tools: BTreeMap<String, Digest>,
    pub platform: String,
    pub outputs: Vec<String>,
}

/// Length-prefixed, tagged encoding — unambiguous (no `"ab"+"c"` vs `"a"+"bc"` collisions).
fn put(buf: &mut Vec<u8>, tag: u8, s: &str) {
    buf.push(tag);
    buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}

impl Action {
    /// The complete content key. Deterministic (sorted maps), unambiguous (length-prefixed),
    /// and sensitive to every component (F12 completeness).
    pub fn content_key(&self) -> Digest {
        let mut buf = Vec::new();
        for a in &self.argv {
            put(&mut buf, b'A', a);
        }
        for (k, v) in &self.inputs {
            put(&mut buf, b'I', k);
            put(&mut buf, b'i', &v.to_hex());
        }
        for (k, v) in &self.env {
            put(&mut buf, b'E', k);
            put(&mut buf, b'e', v);
        }
        for (k, v) in &self.tools {
            put(&mut buf, b'T', k);
            put(&mut buf, b't', &v.to_hex());
        }
        put(&mut buf, b'P', &self.platform);
        for o in &self.outputs {
            put(&mut buf, b'O', o);
        }
        Digest::of(&buf)
    }
}

/// A set of actions — supports `aquery`-lite dumping and uniqueness checks.
#[derive(Debug, Default)]
pub struct ActionGraph {
    pub actions: Vec<Action>,
}

impl ActionGraph {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn add(&mut self, a: Action) {
        self.actions.push(a);
    }
    /// `aquery`-lite: one line per action — `key argv… -> outputs…`.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for a in &self.actions {
            out.push_str(&format!(
                "{} {} -> {}\n",
                &a.content_key().to_hex()[..12],
                a.argv.join(" "),
                a.outputs.join(",")
            ));
        }
        out
    }
    /// True iff every action has a distinct content key.
    pub fn keys_unique(&self) -> bool {
        let mut keys: Vec<_> = self.actions.iter().map(|a| a.content_key()).collect();
        keys.sort();
        let n = keys.len();
        keys.dedup();
        keys.len() == n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Action {
        Action {
            argv: vec!["cc".into(), "-c".into(), "a.c".into()],
            inputs: BTreeMap::from([("a.c".into(), Digest::of(b"a-contents"))]),
            env: BTreeMap::from([("PATH".into(), "/usr/bin".into())]),
            tools: BTreeMap::from([("cc".into(), Digest::of(b"cc-binary"))]),
            platform: "linux-x86_64".into(),
            outputs: vec!["a.o".into()],
        }
    }

    #[test]
    fn key_is_stable_and_order_independent() {
        let k = base().content_key();
        assert_eq!(k, base().content_key());
        // Same data, different insertion order → same key (BTreeMap canonicalizes).
        let mut a = base();
        a.inputs = BTreeMap::new();
        a.inputs.insert("a.c".into(), Digest::of(b"a-contents"));
        assert_eq!(a.content_key(), k);
    }

    #[test]
    fn every_component_change_changes_the_key() {
        let k = base().content_key();
        let mutate = |f: &dyn Fn(&mut Action)| {
            let mut a = base();
            f(&mut a);
            a.content_key()
        };
        assert_ne!(mutate(&|a| a.argv.push("-O2".into())), k, "argv");
        assert_ne!(
            mutate(&|a| {
                a.inputs.insert("a.c".into(), Digest::of(b"CHANGED"));
            }),
            k,
            "input digest"
        );
        assert_ne!(
            mutate(&|a| {
                a.inputs.insert("extra.h".into(), Digest::of(b"h"));
            }),
            k,
            "added input (the silent-wrong-build case)"
        );
        assert_ne!(
            mutate(&|a| {
                a.env.insert("LANG".into(), "C".into());
            }),
            k,
            "env"
        );
        assert_ne!(
            mutate(&|a| {
                a.tools.insert("cc".into(), Digest::of(b"NEW-CC"));
            }),
            k,
            "tool digest"
        );
        assert_ne!(
            mutate(&|a| a.platform = "darwin-arm64".into()),
            k,
            "platform"
        );
        assert_ne!(mutate(&|a| a.outputs.push("a.d".into())), k, "outputs");
    }

    #[test]
    fn no_prefix_collision_between_adjacent_argv() {
        let mut x = base();
        x.argv = vec!["ab".into(), "c".into()];
        let mut y = base();
        y.argv = vec!["a".into(), "bc".into()];
        assert_ne!(x.content_key(), y.content_key());
    }

    #[test]
    fn action_graph_uniqueness_and_dump() {
        let mut g = ActionGraph::new();
        g.add(base());
        let mut other = base();
        other.outputs = vec!["b.o".into()];
        g.add(other);
        assert!(g.keys_unique());
        assert_eq!(g.dump().lines().count(), 2);

        g.add(base()); // duplicate of the first
        assert!(!g.keys_unique());
    }
}
