//! Per-action execution sandbox (F12 enforcement).
//!
//! An action runs in a directory that contains **only its declared inputs**
//! (materialized from the exec root), with cwd set there. An *undeclared*
//! workspace file is therefore simply absent — the action fails to find it,
//! exactly like Bazel. This makes the content-addressed action key trustworthy:
//! under-declaration fails loudly instead of silently mis-caching.
//!
//! This is the **portable** sandbox: it isolates the *workspace tree* via a
//! symlink (or hardlink) forest, not the whole filesystem — absolute paths
//! (`/usr/bin/cc`, system headers, libc) remain reachable. Full OS isolation
//! (Linux mount/net namespaces, macOS Seatbelt) is a stronger layer on top.
//!
//! Cost: symlinks/hardlinks copy no data, and a [`Sandbox`] can be **reused**
//! across rebuilds so only changed links are fixed up ([`Sandbox::sync_inputs`]).

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

/// How a declared input is placed into the sandbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Materialize {
    /// Symlink to the source — zero copy, points outside the sandbox.
    #[default]
    Symlink,
    /// Hardlink the source into the sandbox — zero copy, same inode, stays
    /// "inside" (survives a future mount-namespace). Requires the same
    /// filesystem; falls back to a symlink across devices.
    Hardlink,
}

/// A per-action sandbox directory. `transient` ones self-delete on drop;
/// `persistent` ones are reused across builds (only the link *delta* is fixed up).
pub struct Sandbox {
    dir: PathBuf,
    owned: bool,
    how: Materialize,
    /// Inputs currently linked in (relative path → its source), for delta fixup.
    linked: BTreeMap<String, PathBuf>,
}

impl Sandbox {
    /// A fresh, self-deleting sandbox under `root`, named by `tag`.
    pub fn transient(root: &Path, tag: &str) -> io::Result<Self> {
        Self::at(root.join(tag), true, Materialize::Symlink)
    }

    /// A reusable sandbox at a fixed `dir` (not deleted on drop). Across rebuilds
    /// call [`sync_inputs`](Self::sync_inputs) to fix up only what changed.
    pub fn persistent(dir: impl Into<PathBuf>, how: Materialize) -> io::Result<Self> {
        Self::at(dir.into(), false, how)
    }

    fn at(dir: PathBuf, owned: bool, how: Materialize) -> io::Result<Self> {
        if owned {
            let _ = std::fs::remove_dir_all(&dir);
        }
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            owned,
            how,
            linked: BTreeMap::new(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
    pub fn strategy(mut self, how: Materialize) -> Self {
        self.how = how;
        self
    }

    fn link_one(&self, rel: &str, source: &Path) -> io::Result<()> {
        let link = self.dir.join(rel);
        if let Some(p) = link.parent() {
            std::fs::create_dir_all(p)?;
        }
        let _ = std::fs::remove_file(&link);
        match self.how {
            Materialize::Symlink => symlink(source, &link),
            // Hardlink needs same-fs; fall back to a symlink across devices.
            Materialize::Hardlink => {
                std::fs::hard_link(source, &link).or_else(|_| symlink(source, &link))
            }
        }
    }

    /// Reconcile the sandbox to exactly `inputs` (paths relative to `exec_root`),
    /// adding/removing only the links that differ. Returns the number of links
    /// changed — 0 means the forest was already correct (the reuse win). When the
    /// input *set* is unchanged (only file *contents* changed), this is a no-op.
    pub fn sync_inputs(&mut self, exec_root: &Path, inputs: &[String]) -> io::Result<usize> {
        let want: BTreeSet<&String> = inputs.iter().collect();
        let mut changed = 0;

        // Remove links no longer wanted.
        let stale: Vec<String> = self
            .linked
            .keys()
            .filter(|k| !want.contains(*k))
            .cloned()
            .collect();
        for rel in stale {
            let _ = std::fs::remove_file(self.dir.join(&rel));
            self.linked.remove(&rel);
            changed += 1;
        }
        // Add links not yet present (pointing at the right source).
        for rel in inputs {
            let source = exec_root.join(rel);
            if self.linked.get(rel) != Some(&source) {
                self.link_one(rel, &source)?;
                self.linked.insert(rel.clone(), source);
                changed += 1;
            }
        }
        Ok(changed)
    }

    /// Pre-create parent directories for `outputs` so the action can write them.
    pub fn prepare_outputs(&self, outputs: &[String]) -> io::Result<()> {
        for o in outputs {
            if let Some(p) = self.dir.join(o).parent() {
                std::fs::create_dir_all(p)?;
            }
        }
        Ok(())
    }

    /// Run `argv` with cwd = the sandbox and a default-deny env (only `env`).
    pub fn run(&self, argv: &[String], env: &BTreeMap<String, String>) -> io::Result<i32> {
        let (prog, rest) = argv
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty argv"))?;
        let status = Command::new(prog)
            .args(rest)
            .current_dir(&self.dir)
            .env_clear()
            .envs(env)
            .status()?;
        Ok(status.code().unwrap_or(-1))
    }

    /// Copy declared `outputs` produced in the sandbox back into `exec_root`.
    pub fn capture_outputs(&self, exec_root: &Path, outputs: &[String]) -> io::Result<()> {
        for o in outputs {
            let to = exec_root.join(o);
            if let Some(p) = to.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(self.dir.join(o), to)?;
        }
        Ok(())
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if self.owned {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}
