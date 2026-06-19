//! razel-build `exec_root` — split from `lib.rs`.

use super::*;

/// RazelRustParityPlan B4: lay out a proper bazel-style EXEC ROOT for external-crate builds — a
/// symlink forest of the workspace's SOURCE entries plus `external/<repo>` → the fetched `@crates`
/// repos (`.razel-crates`), so an external crate's declared source path
/// (`external/<repo>/src/lib.rs`) resolves at execution. Generated outputs land in
/// `<exec_root>/bazel-out/…` (under `--bazel_build_compat`), SEPARATE from sources — the bazel-faithful
/// layout, never mixed into the fetched-source cache. Razel-managed state + build outputs are excluded.
pub(crate) fn prepare_exec_root(workspace: &Path) -> std::io::Result<std::path::PathBuf> {
    let exec_root = workspace.join(".razel-exec");
    let _ = std::fs::remove_dir_all(&exec_root);
    std::fs::create_dir_all(&exec_root)?;
    for entry in std::fs::read_dir(workspace)? {
        let entry = entry?;
        let name = entry.file_name();
        let n = name.to_string_lossy();
        if n.starts_with(".razel-")
            || n.starts_with(".git")
            || matches!(n.as_ref(), "target" | "bazel-out" | "razel-out" | "razel-bin" | "razel-testlogs")
        {
            continue;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(entry.path(), exec_root.join(&name))?;
    }
    // `external/<repo>` → the fetched `@crates` repos (analysis materialized them lazily into .razel-crates).
    let crates = workspace.join(".razel-crates");
    if crates.is_dir() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(&crates, exec_root.join("external"))?;
    }
    Ok(exec_root)
}
use std::collections::BTreeMap;
use std::path::Path;


/// Run ONE target's actions in `exec_root` against `cache` (cache hit → 0 exec). The unit
/// shared by the serial and parallel drivers, so `-j1` and `-jN` run identical action/cache
/// semantics — only the *order across independent targets* differs. Returns
/// `(executed_count, produced_paths)`.
/// Content digest of a declared input that EXISTS — a FILE's bytes, or a DIRECTORY tree's content
/// (P2.4: a `cargo_build_script` `OUT_DIR` is a tree input to the consuming crate compile, e.g.
/// blake3's `libblake3_neon.a`). A directory hashes its files' `(relative-path, content-digest)`
/// pairs in SORTED order → deterministic. Absent → `None` (skipped, symmetric with the executor's
/// `copy_path`: e.g. a declared-but-unproduced `.dSYM`). Follows symlinks (the exec-root forest).
/// Before this, a directory input hit `std::fs::read`'s `EISDIR` and was silently dropped — omitted
/// from BOTH the content key AND the sandbox staging, so the consuming compile couldn't see it.
pub(crate) fn digest_input(path: &Path) -> Option<Digest> {
    if std::fs::metadata(path).ok()?.is_dir() {
        let mut files: Vec<(String, Digest)> = Vec::new();
        digest_tree_into(path, path, &mut files)?;
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let mut buf = Vec::new();
        for (rel, d) in files {
            buf.extend_from_slice(rel.as_bytes());
            buf.push(0);
            buf.extend_from_slice(d.to_hex().as_bytes());
            buf.push(0);
        }
        Some(Digest::of(&buf))
    } else {
        std::fs::read(path).ok().map(|b| Digest::of(&b))
    }
}


/// Collect `(path-relative-to-root, file-content-digest)` for every file under `dir`. `None` on any
/// I/O error mid-walk → the whole input is skipped (surfacing loudly as a missing input downstream).
pub(crate) fn digest_tree_into(root: &Path, dir: &Path, acc: &mut Vec<(String, Digest)>) -> Option<()> {
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if std::fs::metadata(&path).ok()?.is_dir() {
            digest_tree_into(root, &path, acc)?;
        } else {
            let rel = path.strip_prefix(root).ok()?.to_string_lossy().into_owned();
            acc.push((rel, Digest::of(&std::fs::read(&path).ok()?)));
        }
    }
    Some(())
}


pub(crate) fn run_one_target(
    t: &AnalyzedTarget,
    exec_root: &Path,
    cache: &Cache,
) -> Result<(usize, Vec<String>), String> {
    let mut executed = 0;
    let mut produced = Vec::new();
    for act in &t.actions {
        // Digest the declared inputs that exist on disk → the action's content key.
        let mut inputs = BTreeMap::new();
        for inp in &act.inputs {
            if let Some(d) = digest_input(&exec_root.join(inp)) {
                inputs.insert(inp.clone(), d);
            }
        }
        let action = Action {
            argv: act.argv.clone(),
            inputs,
            env: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
            tools: BTreeMap::new(),
            platform: "host".into(),
            outputs: act.outputs.clone(),
        };
        let r = build_action(&action, cache, exec_root).map_err(|e| e.to_string())?;
        if r.exit_code != 0 {
            return Err(format!("action failed ({}): {:?}", r.exit_code, act.argv));
        }
        if !r.cached {
            executed += 1;
            // Progress — so a build isn't silent (Bazel/cargo-style, on stderr): the action's
            // mnemonic + primary output as it runs.
            let out = act.outputs.first().map(String::as_str).unwrap_or(t.name.as_str());
            eprintln!("  {} {out}", act.mnemonic);
        }
        produced.extend(act.outputs.clone());
    }
    Ok((executed, produced))
}


