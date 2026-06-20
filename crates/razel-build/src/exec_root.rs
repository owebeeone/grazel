//! razel-build `exec_root` — split from `lib.rs`.

use super::*;

/// RazelRustParityPlan B4: lay out a proper bazel-style EXEC ROOT for external-crate builds — a
/// symlink forest of the workspace's SOURCE entries plus `external/<repo>` → the fetched `@crates`
/// repos (`.razel-crates`), so an external crate's declared source path
/// (`external/<repo>/src/lib.rs`) resolves at execution. Generated outputs land in
/// `<exec_root>/bazel-out/…` (under `--bazel_build_compat`), SEPARATE from sources — the bazel-faithful
/// layout, never mixed into the fetched-source cache. Razel-managed state + build outputs are excluded.
pub fn prepare_exec_root(workspace: &Path) -> std::io::Result<std::path::PathBuf> {
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
// run_one_target + digest_input were the OLD per-target executor (re-hash every input, then
// build_action). The unified engine path (execute_jobs → IncrementalBuilder → request_parallel)
// replaced them: the engine carries input digests as named DepValues (no re-hash) and runs actions
// in parallel through the same restore_or_run core. Deleted — one executor now.


