//! Execution + cache (Phase 5).
//!
//! Runs an [`Action`]'s `argv` in an exec root with a **default-deny env** (only the
//! action's allowlisted env — the portable hermeticity baseline, F5), captures declared
//! outputs, and caches them content-addressed by the action key. A second build with the
//! same key restores outputs with **zero execution**.
//!
//! SCOPE: the portable core (exec root + env-clear + content-addressed cache). The native
//! enforcing sandbox (`linux-sandbox` namespaces / macOS `sandbox-exec`) and a symlink-tree
//! input materialization are the OS-specific layer on top (tracked); they enforce, but
//! don't change, this contract.

pub mod sandbox;
pub use sandbox::{Isolation, Materialize, Sandbox, seatbelt_available};

use razel_actions::Action;
use razel_core::Digest;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{fs, io};

#[derive(Debug)]
pub struct RunResult {
    pub exit_code: i32,
    pub cached: bool,
    pub outputs: Vec<PathBuf>,
}

/// A content-addressed output cache: `<root>/<action-key-hex>/<output-paths>`.
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    pub fn new(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn key_dir(&self, key: &Digest) -> PathBuf {
        self.root.join(key.to_hex())
    }

    /// Restore cached outputs into `exec_root`; `Ok(true)` if the key was cached.
    pub fn restore(&self, key: &Digest, outputs: &[String], exec_root: &Path) -> io::Result<bool> {
        let kd = self.key_dir(key);
        if !kd.exists() {
            return Ok(false);
        }
        for o in outputs {
            let to = exec_root.join(o);
            if let Some(p) = to.parent() {
                fs::create_dir_all(p)?;
            }
            fs::copy(kd.join(o), to)?;
        }
        Ok(true)
    }

    /// Store an action's outputs from `exec_root` under its key.
    pub fn store(&self, key: &Digest, outputs: &[String], exec_root: &Path) -> io::Result<()> {
        let kd = self.key_dir(key);
        for o in outputs {
            let to = kd.join(o);
            if let Some(p) = to.parent() {
                fs::create_dir_all(p)?;
            }
            fs::copy(exec_root.join(o), to)?;
        }
        Ok(())
    }
}

/// Spawn the action's `argv` in `exec_root` with a default-deny env (only `action.env`).
pub fn run_action(action: &Action, exec_root: &Path) -> io::Result<i32> {
    let (prog, rest) = action
        .argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty argv"))?;
    let status = Command::new(prog)
        .args(rest)
        .current_dir(exec_root)
        .env_clear()
        .envs(&action.env)
        .status()?;
    Ok(status.code().unwrap_or(-1))
}

/// Build one action: cache hit → restore (0 exec); miss → run in a fresh sandbox
/// containing only the declared inputs, and store on success.
pub fn build_action(action: &Action, cache: &Cache, exec_root: &Path) -> io::Result<RunResult> {
    let key = action.content_key();
    let mut sandbox = Sandbox::transient(&exec_root.join(".razel-sandbox"), &key.to_hex())?;
    build_action_in(action, cache, exec_root, &mut sandbox)
}

/// Like [`build_action`] but the caller supplies the [`Sandbox`] — letting a warm
/// builder hand in a **persistent, reused** sandbox so only the changed inputs are
/// re-linked across rebuilds. On a cache hit the sandbox is untouched.
pub fn build_action_in(
    action: &Action,
    cache: &Cache,
    exec_root: &Path,
    sandbox: &mut Sandbox,
) -> io::Result<RunResult> {
    let key = action.content_key();
    let out_paths = || action.outputs.iter().map(|o| exec_root.join(o)).collect();

    if cache.restore(&key, &action.outputs, exec_root)? {
        return Ok(RunResult {
            exit_code: 0,
            cached: true,
            outputs: out_paths(),
        });
    }

    // Miss: materialize only declared inputs, run isolated, capture outputs.
    let inputs: Vec<String> = action.inputs.keys().cloned().collect();
    sandbox.sync_inputs(exec_root, &inputs)?;
    sandbox.prepare_outputs(&action.outputs)?;
    let code = sandbox.run(&action.argv, &action.env)?;
    if code == 0 {
        sandbox.capture_outputs(exec_root, &action.outputs)?;
        cache.store(&key, &action.outputs, exec_root)?;
    }
    Ok(RunResult {
        exit_code: code,
        cached: false,
        outputs: out_paths(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn path_env() -> BTreeMap<String, String> {
        BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())])
    }

    #[test]
    fn runs_then_caches_with_zero_exec_on_second_build() {
        let cache_dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(cache_dir.path()).unwrap();
        let action = Action {
            argv: vec!["/bin/sh".into(), "-c".into(), "cat in.txt > out.txt".into()],
            outputs: vec!["out.txt".into()],
            inputs: BTreeMap::from([("in.txt".into(), Digest::of(b"hello"))]),
            env: path_env(),
            ..Default::default()
        };

        // First build: real subprocess produces out.txt.
        let r1env = tempfile::tempdir().unwrap();
        fs::write(r1env.path().join("in.txt"), "hello").unwrap();
        let r1 = build_action(&action, &cache, r1env.path()).unwrap();
        assert_eq!(r1.exit_code, 0);
        assert!(!r1.cached);
        assert_eq!(
            fs::read_to_string(r1env.path().join("out.txt")).unwrap(),
            "hello"
        );

        // Second build in a FRESH exec root: cache hit, no execution, output restored.
        let r2env = tempfile::tempdir().unwrap();
        let r2 = build_action(&action, &cache, r2env.path()).unwrap();
        assert!(r2.cached);
        assert_eq!(
            fs::read_to_string(r2env.path().join("out.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn sandbox_blocks_undeclared_workspace_inputs() {
        let exec = tempfile::tempdir().unwrap();
        fs::write(exec.path().join("dep.txt"), "secret").unwrap();
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // Reads dep.txt but does NOT declare it → absent in the sandbox → fails.
        let undeclared = Action {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "cat dep.txt > out.txt".into(),
            ],
            outputs: vec!["out.txt".into()],
            env: path_env(),
            ..Default::default()
        };
        let r = build_action(&undeclared, &cache, exec.path()).unwrap();
        assert_ne!(
            r.exit_code, 0,
            "undeclared dep.txt must be absent in the sandbox"
        );

        // Declaring it makes it present → succeeds, output captured back to exec_root.
        let declared = Action {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "cat dep.txt > out.txt".into(),
            ],
            outputs: vec!["out.txt".into()],
            inputs: BTreeMap::from([("dep.txt".into(), Digest::of(b"secret"))]),
            env: path_env(),
            ..Default::default()
        };
        let r2 = build_action(&declared, &cache, exec.path()).unwrap();
        assert_eq!(r2.exit_code, 0, "declared dep.txt is present");
        assert_eq!(
            fs::read_to_string(exec.path().join("out.txt")).unwrap(),
            "secret"
        );
    }

    #[test]
    fn really_compiles_c_with_cc() {
        // A genuine compile action — razel running a real toolchain.
        if !Path::new("/usr/bin/cc").exists() {
            return; // skip where no cc
        }
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();
        let exec = tempfile::tempdir().unwrap();
        fs::write(exec.path().join("a.c"), "int main(void){return 0;}").unwrap();
        let action = Action {
            argv: vec![
                "/usr/bin/cc".into(),
                "-c".into(),
                "a.c".into(),
                "-o".into(),
                "a.o".into(),
            ],
            outputs: vec!["a.o".into()],
            inputs: BTreeMap::from([("a.c".into(), Digest::of(b"int main(void){return 0;}"))]),
            env: path_env(),
            ..Default::default()
        };
        let r = build_action(&action, &cache, exec.path()).unwrap();
        assert_eq!(r.exit_code, 0, "cc failed");
        assert!(exec.path().join("a.o").exists());
        assert!(fs::metadata(exec.path().join("a.o")).unwrap().len() > 0);
    }
}
