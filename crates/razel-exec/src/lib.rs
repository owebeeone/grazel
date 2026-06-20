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

/// A canonical output manifest: `(path, digest)` per PRESENT declared output, sorted by path,
/// unique. Same shape as the engine's `NodeValue::Manifest` (C1) — a transparent `Vec<(String,
/// Digest)>` alias, so `razel-build`'s `add_action` closure wraps an [`execute_action`] result
/// with no conversion and `razel-exec` needs no dependency on `razel-engine`.
pub type Manifest = Vec<(String, Digest)>;

/// The outcome of [`execute_action`] (C2). Carries the output [`Manifest`] so the engine's
/// output-level early cutoff has teeth. A failure carries its MESSAGE (composes with both the
/// warm engine's `Result<_, ComputeError>` action closure and the cold path's error handling);
/// the boundary never returns `io::Result` for an action failure — only `Failed`.
#[derive(Debug)]
pub enum ExecOutcome {
    /// Served from cache — `actions_executed += 0`, `action_cache_hits += 1`.
    Cached(Manifest),
    /// Ran in the sandbox — `actions_executed += 1`.
    Executed(Manifest),
    /// Nonzero action exit, or an internal exec error; the string describes it.
    Failed(String),
}

/// A content-addressed output cache: `<root>/<action-key-hex>/<output-paths>`.
/// `Clone` is cheap (just the root path) — lets the build driver hand a fresh `IncrementalBuilder`
/// the same cache for a one-shot cold build.
#[derive(Clone)]
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

/// Canonical digest of an output (or input) path: a FILE's content blake3, or a DIRECTORY's
/// deterministic tree-hash — sorted `(relpath, file-digest)` pairs concatenated `relpath\0hex\0`
/// then blake3'd. ABSENT → `None`. This is the ONE path-digest algorithm: `razel-build`'s input
/// `digest_input` is migrated (WS-C) to call this, so a generated directory consumed downstream
/// compares byte-for-byte with the same tree read as an input (C2 §4.2 / the C1 manifest).
pub fn digest_path(path: &Path) -> Option<Digest> {
    let md = fs::metadata(path).ok()?;
    if md.is_dir() {
        let mut files: Vec<(String, Digest)> = Vec::new();
        digest_tree_into(path, path, &mut files)?;
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let mut buf = Vec::new();
        for (rel, dg) in files {
            buf.extend_from_slice(rel.as_bytes());
            buf.push(0);
            buf.extend_from_slice(dg.to_hex().as_bytes());
            buf.push(0);
        }
        Some(Digest::of(&buf))
    } else {
        fs::read(path).ok().map(|b| Digest::of(&b))
    }
}

fn digest_tree_into(root: &Path, dir: &Path, acc: &mut Vec<(String, Digest)>) -> Option<()> {
    for entry in fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let p = entry.path();
        if fs::metadata(&p).ok()?.is_dir() {
            digest_tree_into(root, &p, acc)?;
        } else {
            let rel = p.strip_prefix(root).ok()?.to_string_lossy().into_owned();
            acc.push((rel, Digest::of(&fs::read(&p).ok()?)));
        }
    }
    Some(())
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
    let (cached, exit_code) = restore_or_run(action, cache, exec_root, sandbox)?;
    Ok(RunResult {
        exit_code,
        cached,
        outputs: action.outputs.iter().map(|o| exec_root.join(o)).collect(),
    })
}

/// The shared executor core (C2): cache hit → restore; miss → materialize declared inputs, run
/// isolated, capture + store on success. Returns `(cached, exit_code)` and does NOT digest
/// outputs, so the cold [`build_action_in`] path is byte-for-byte unchanged. The ONE seam the
/// cold path (`run_one_target`) and the warm engine closure (`add_action`) share — RULE 3
/// (tested == run), RULE 5 (one owner); the two paths cannot drift.
fn restore_or_run(
    action: &Action,
    cache: &Cache,
    exec_root: &Path,
    sandbox: &mut Sandbox,
) -> io::Result<(bool, i32)> {
    let key = action.content_key();
    if cache.restore(&key, &action.outputs, exec_root)? {
        return Ok((true, 0));
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
    Ok((false, code))
}

/// Execute one action and return its [`ExecOutcome`] + output [`Manifest`] (C2). The warm
/// engine's `add_action` closure calls THIS and maps `Cached`/`Executed` → `Ok(Manifest)` and
/// `Failed(msg)` → `Err(msg)`; the cold path uses [`build_action_in`]. Same core
/// (`restore_or_run`), so the warm and cold executions cannot drift. The output manifest is
/// digested only when the action actually restores/runs — on a true no-op the engine's early
/// cutoff firewalls the action node, so this is never called.
pub fn execute_action(
    action: &Action,
    cache: &Cache,
    exec_root: &Path,
    sandbox: &mut Sandbox,
) -> ExecOutcome {
    match restore_or_run(action, cache, exec_root, sandbox) {
        Ok((true, _)) => ExecOutcome::Cached(output_manifest(action, exec_root)),
        Ok((false, 0)) => ExecOutcome::Executed(output_manifest(action, exec_root)),
        Ok((false, code)) => {
            ExecOutcome::Failed(format!("action failed (rc={code}): {:?}", action.argv))
        }
        Err(e) => ExecOutcome::Failed(format!("razel internal: exec error: {e}")),
    }
}

/// The canonical output manifest: `(path, digest)` for each PRESENT declared output (absent
/// outputs omitted — A7), sorted by path. File digest = content blake3; directory digest = the
/// deterministic tree-hash, both via [`digest_path`].
fn output_manifest(action: &Action, exec_root: &Path) -> Manifest {
    let mut m: Manifest = action
        .outputs
        .iter()
        .filter_map(|o| digest_path(&exec_root.join(o)).map(|dg| (o.clone(), dg)))
        .collect();
    m.sort_by(|a, b| a.0.cmp(&b.0));
    m
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

    // ---- C2: execute_action / ExecOutcome / Manifest ----

    #[test]
    fn execute_action_reports_cached_executed_failed() {
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();
        let exec = tempfile::tempdir().unwrap();
        let mut sb = Sandbox::transient(&exec.path().join(".razel-sandbox"), "k").unwrap();
        let action = Action {
            argv: vec!["/bin/sh".into(), "-c".into(), "echo hi > out.txt".into()],
            outputs: vec!["out.txt".into()],
            env: path_env(),
            ..Default::default()
        };
        // miss → Executed(manifest)
        match execute_action(&action, &cache, exec.path(), &mut sb) {
            ExecOutcome::Executed(m) => {
                assert_eq!(m.len(), 1);
                assert_eq!(m[0].0, "out.txt");
            }
            other => panic!("expected Executed, got {other:?}"),
        }
        // hit → Cached(manifest) in a fresh exec root
        let exec2 = tempfile::tempdir().unwrap();
        let mut sb2 = Sandbox::transient(&exec2.path().join(".razel-sandbox"), "k").unwrap();
        assert!(matches!(
            execute_action(&action, &cache, exec2.path(), &mut sb2),
            ExecOutcome::Cached(_)
        ));
        // failing action → Failed(msg) carrying the rc
        let boom = Action {
            argv: vec!["/bin/sh".into(), "-c".into(), "exit 7".into()],
            outputs: vec![],
            env: path_env(),
            ..Default::default()
        };
        let exec3 = tempfile::tempdir().unwrap();
        let mut sb3 = Sandbox::transient(&exec3.path().join(".razel-sandbox"), "b").unwrap();
        match execute_action(&boom, &cache, exec3.path(), &mut sb3) {
            ExecOutcome::Failed(msg) => assert!(msg.contains("rc=7"), "msg: {msg}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// G13: a directory output → one `(path, tree-hash)` entry (== `digest_path` read back); an
    /// absent declared output → omitted (not a sentinel, not an error); entries sorted by path.
    #[test]
    fn manifest_has_dir_treehash_and_omits_absent_output() {
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();
        let exec = tempfile::tempdir().unwrap();
        let mut sb = Sandbox::transient(&exec.path().join(".razel-sandbox"), "d").unwrap();
        let action = Action {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "echo f > file.out && mkdir -p d.out/sub && echo x > d.out/sub/x".into(),
            ],
            outputs: vec!["file.out".into(), "d.out".into(), "absent.dSYM".into()],
            env: path_env(),
            ..Default::default()
        };
        let ExecOutcome::Executed(m) = execute_action(&action, &cache, exec.path(), &mut sb) else {
            panic!("expected Executed");
        };
        let paths: Vec<&str> = m.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["d.out", "file.out"]); // sorted; absent omitted
        let dir_dg = m.iter().find(|(p, _)| p == "d.out").unwrap().1;
        assert_eq!(Some(dir_dg), digest_path(&exec.path().join("d.out")));
    }
}
