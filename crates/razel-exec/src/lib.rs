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
            copy_path(&kd.join(o), &exec_root.join(o))?;
        }
        Ok(true)
    }

    /// Store an action's outputs from `exec_root` under its key.
    pub fn store(&self, key: &Digest, outputs: &[String], exec_root: &Path) -> io::Result<()> {
        let kd = self.key_dir(key);
        for o in outputs {
            copy_path(&exec_root.join(o), &kd.join(o))?;
        }
        Ok(())
    }
}

/// Copy an output path — a FILE (`fs::copy`) or a whole DIRECTORY tree (recursive). P2.4: the
/// `cargo_build_script` `OUT_DIR` and extracted `.crate` trees are directory outputs the executor
/// must round-trip, not just single files (design §5.2/§10).
///
/// An ABSENT `from` is a NO-OP (A7): a declared output the action did not produce — e.g. the macOS
/// `.dSYM` bundle a `debuginfo=0` build omits, declared only to MATCH Bazel's action graph (the
/// system rustc, no vendored toolchain, is a documented parity deviation) — is simply not
/// captured/cached. This is symmetric with the build driver, which digests only inputs that exist;
/// if anything actually consumes a skipped output, that consumer fails loudly on the missing input.
pub(crate) fn copy_path(from: &Path, to: &Path) -> io::Result<()> {
    if !from.exists() {
        return Ok(());
    }
    if from.is_dir() {
        copy_dir_recursive(from, to)
    } else {
        if let Some(p) = to.parent() {
            fs::create_dir_all(p)?;
        }
        fs::copy(from, to)?;
        Ok(())
    }
}

/// Recursively copy `from`'s subtree into `to` (created).
fn copy_dir_recursive(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&src, &dst)?;
        } else {
            fs::copy(&src, &dst)?;
        }
    }
    Ok(())
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
    fn cache_round_trips_a_directory_output() {
        // P2.4: a DIRECTORY output (cargo_build_script OUT_DIR / extracted .crate tree) must
        // round-trip through the cache, nested files included — not just single files.
        let exec = tempfile::tempdir().unwrap();
        let out = exec.path().join("out_dir");
        std::fs::create_dir_all(out.join("sub")).unwrap();
        std::fs::write(out.join("a.o"), b"aaa").unwrap();
        std::fs::write(out.join("sub/b.o"), b"bbb").unwrap();

        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();
        let key = Digest::of(b"treekey");
        cache.store(&key, &["out_dir".into()], exec.path()).unwrap();

        let exec2 = tempfile::tempdir().unwrap();
        assert!(cache.restore(&key, &["out_dir".into()], exec2.path()).unwrap());
        assert_eq!(std::fs::read(exec2.path().join("out_dir/a.o")).unwrap(), b"aaa");
        assert_eq!(std::fs::read(exec2.path().join("out_dir/sub/b.o")).unwrap(), b"bbb");
    }

    #[test]
    fn captures_a_tree_output_and_skips_an_absent_declared_output() {
        // A7: an action writes a FILE + a DIRECTORY tree but NOT a third declared output (the
        // macOS `.dSYM` a `debuginfo=0` rust build omits — declared only to match Bazel's graph).
        // Capture is dir-aware (the tree round-trips) AND skip-absent (the unproduced output is
        // silently not captured/cached), not a hard `os error 2`. Round-trips through the cache.
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();
        let exec = tempfile::tempdir().unwrap();
        let action = Action {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "echo hi > out.bin && mkdir -p out.dir/sub && echo t > out.dir/sub/x".into(),
            ],
            outputs: vec!["out.bin".into(), "out.dir".into(), "missing.dSYM".into()],
            env: path_env(),
            ..Default::default()
        };
        let r = build_action(&action, &cache, exec.path()).unwrap();
        assert_eq!(r.exit_code, 0);
        assert!(!r.cached);
        // The produced file + tree are captured back; the unproduced declared output is absent.
        assert_eq!(fs::read_to_string(exec.path().join("out.bin")).unwrap().trim(), "hi");
        assert_eq!(fs::read_to_string(exec.path().join("out.dir/sub/x")).unwrap().trim(), "t");
        assert!(!exec.path().join("missing.dSYM").exists(), "absent output is not materialized");

        // Fresh exec root: cache hit restores the file + tree (dir-aware), skips the absent one.
        let exec2 = tempfile::tempdir().unwrap();
        let r2 = build_action(&action, &cache, exec2.path()).unwrap();
        assert!(r2.cached, "served from cache");
        assert_eq!(fs::read_to_string(exec2.path().join("out.bin")).unwrap().trim(), "hi");
        assert_eq!(fs::read_to_string(exec2.path().join("out.dir/sub/x")).unwrap().trim(), "t");
        assert!(!exec2.path().join("missing.dSYM").exists());
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
