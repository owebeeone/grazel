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
//!
//! Platforms: unix-only today (symlink/hardlink + Seatbelt). The **Windows** port
//! is untouched — it would materialize via `CreateHardLink` (no privilege on NTFS
//! same-volume; symlinks need Developer Mode) and confine via a Job Object +
//! restricted token / AppContainer (the Seatbelt analogue). Tracked, not built.

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
    ///
    /// Caveat for *reused* sandboxes: a hardlink tracks the inode, so an in-place
    /// edit (truncate+write) is reflected automatically, but a *rename-replace*
    /// edit (new inode) leaves a stale link until re-materialized. Digest-aware
    /// refresh on content change is the remote-exec-grade follow-up; symlink mode
    /// (the default) tracks the path and has no such caveat.
    Hardlink,
}

/// OS-level confinement applied when an action runs, on top of the input tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Isolation {
    /// No OS sandbox — the input tree alone isolates the workspace (cwd).
    #[default]
    None,
    /// macOS Seatbelt (`sandbox-exec`): a write-confinement + optional network
    /// block, requiring **no root**. Reads stay open (the input tree handles
    /// read-isolation). Inactive on non-macOS targets. `network = false` blocks
    /// the network (keeping UDS).
    Seatbelt { network: bool },
}

/// A per-action sandbox directory. `transient` ones self-delete on drop;
/// `persistent` ones are reused across builds (only the link *delta* is fixed up).
pub struct Sandbox {
    dir: PathBuf,
    owned: bool,
    how: Materialize,
    isolation: Isolation,
    /// Inputs currently linked in (relative path → its source), for delta fixup.
    linked: BTreeMap<String, PathBuf>,
}

/// Whether macOS Seatbelt (`/usr/bin/sandbox-exec`) is usable on this machine.
/// Probes once with a trivial allow-all profile; false on non-macOS or if the
/// (deprecated-but-present) tool is missing — callers degrade to [`Isolation::None`].
pub fn seatbelt_available() -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    Command::new("/usr/bin/sandbox-exec")
        .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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
            isolation: Isolation::None,
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
    /// Apply OS-level confinement (e.g. Seatbelt) when running.
    pub fn with_isolation(mut self, isolation: Isolation) -> Self {
        self.isolation = isolation;
        self
    }

    /// macOS Seatbelt profile (SBPL): allow-default, then **deny all writes**
    /// except the sandbox dir + `$TMPDIR` (+ the std streams), and optionally
    /// block the network. Paths are canonicalized so the kernel's realpath
    /// (`/var` → `/private/var`) matches.
    fn seatbelt_profile(&self, network: bool) -> io::Result<String> {
        let dir = self.dir.canonicalize()?;
        let tmp = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
        let tmp = Path::new(&tmp)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from("/tmp"));
        let net = if network {
            ""
        } else {
            "(deny network*)(allow network* (remote unix-socket))"
        };
        Ok(format!(
            "(version 1)(allow default){net}(deny file-write*)(allow file-write* \
             (subpath \"{dir}\") (subpath \"{tmp}\") \
             (literal \"/dev/null\") (literal \"/dev/stdout\") (literal \"/dev/stderr\"))",
            dir = dir.display(),
            tmp = tmp.display(),
        ))
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

    /// Run `argv` with cwd = the sandbox and a default-deny env (only `env`),
    /// optionally wrapped in the OS sandbox ([`Isolation`]).
    pub fn run(&self, argv: &[String], env: &BTreeMap<String, String>) -> io::Result<i32> {
        let (prog, rest) = argv
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty argv"))?;

        // Seatbelt: run `sandbox-exec -p <profile> -- argv` (macOS only; elsewhere
        // the request is a no-op and the action runs in just the input tree).
        let mut cmd = match self.isolation {
            Isolation::Seatbelt { network } if cfg!(target_os = "macos") => {
                let profile = self.seatbelt_profile(network)?;
                let mut c = Command::new("/usr/bin/sandbox-exec");
                c.arg("-p").arg(profile).arg("--").arg(prog).args(rest);
                c
            }
            _ => {
                let mut c = Command::new(prog);
                c.args(rest);
                c
            }
        };
        let status = cmd.current_dir(&self.dir).env_clear().envs(env).status()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_inputs_fixes_up_only_the_delta() {
        let exec = tempfile::tempdir().unwrap();
        for f in ["a", "b", "c"] {
            std::fs::write(exec.path().join(f), f).unwrap();
        }
        let sbdir = tempfile::tempdir().unwrap();
        let mut sb = Sandbox::persistent(sbdir.path().join("s"), Materialize::Symlink).unwrap();

        // First materialize {a, b}: two links created.
        let n1 = sb
            .sync_inputs(exec.path(), &["a".into(), "b".into()])
            .unwrap();
        assert_eq!(n1, 2);
        assert!(sb.dir().join("a").exists() && sb.dir().join("b").exists());

        // Reconcile to {a, c}: drop b, add c, KEEP a → exactly 2 changes.
        let n2 = sb
            .sync_inputs(exec.path(), &["a".into(), "c".into()])
            .unwrap();
        assert_eq!(n2, 2, "remove b + add c");
        assert!(sb.dir().join("a").exists() && sb.dir().join("c").exists());
        assert!(!sb.dir().join("b").exists(), "b removed");

        // Same set again: zero churn — the reuse win.
        let n3 = sb
            .sync_inputs(exec.path(), &["a".into(), "c".into()])
            .unwrap();
        assert_eq!(n3, 0, "input set unchanged → no link work");
    }

    #[test]
    fn hardlink_materialization_is_zero_copy() {
        use std::os::unix::fs::MetadataExt;
        let exec = tempfile::tempdir().unwrap();
        std::fs::write(exec.path().join("a"), "data").unwrap();
        // Sandbox under exec → guaranteed same filesystem → real hardlink (no fallback).
        let mut sb = Sandbox::persistent(exec.path().join(".sb"), Materialize::Hardlink).unwrap();
        sb.sync_inputs(exec.path(), &["a".into()]).unwrap();

        let entry = sb.dir().join("a");
        // It's a real file (a hardlink), NOT a symlink...
        let lst = std::fs::symlink_metadata(&entry).unwrap();
        assert!(!lst.file_type().is_symlink(), "hardlink, not symlink");
        // ...sharing the source's inode (zero byte copy), source link count >= 2.
        let src = std::fs::metadata(exec.path().join("a")).unwrap();
        let dst = std::fs::metadata(&entry).unwrap();
        assert_eq!(src.ino(), dst.ino(), "hardlink shares the inode");
        assert!(src.nlink() >= 2, "source now has >= 2 links");
    }

    #[test]
    fn persistent_sandbox_dir_survives_drop() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("keep");
        {
            let _sb = Sandbox::persistent(&dir, Materialize::Symlink).unwrap();
        }
        assert!(dir.exists(), "persistent sandbox is not deleted on drop");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_confines_writes_to_the_sandbox() {
        if !seatbelt_available() {
            return; // sandbox-exec unavailable (e.g. stripped CI image)
        }
        let env = BTreeMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())]);
        let exec = tempfile::tempdir().unwrap();
        let sb = Sandbox::persistent(exec.path().join(".sb"), Materialize::Symlink)
            .unwrap()
            .with_isolation(Isolation::Seatbelt { network: false });

        // A write *inside* the sandbox (cwd) is allowed.
        let inside = sb
            .run(
                &["/bin/sh".into(), "-c".into(), "echo ok > inside.txt".into()],
                &env,
            )
            .unwrap();
        assert_eq!(inside, 0, "in-sandbox write should be allowed");
        assert!(sb.dir().join("inside.txt").exists());

        // A write *outside* the sandbox (/tmp is not the sandbox nor $TMPDIR on
        // macOS, which is /var/folders/...) is denied by the profile.
        let escape = format!("/tmp/razel-seatbelt-escape-{}.txt", std::process::id());
        let _ = std::fs::remove_file(&escape);
        let outside = sb
            .run(
                &["/bin/sh".into(), "-c".into(), format!("echo x > {escape}")],
                &env,
            )
            .unwrap();
        assert_ne!(outside, 0, "write outside the sandbox must be denied");
        assert!(
            !std::path::Path::new(&escape).exists(),
            "the escaping write must not have happened"
        );
    }
}
