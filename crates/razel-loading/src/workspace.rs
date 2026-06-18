//! Workspace structure — boundary walk-up and package-file resolution.
//!
//! `BUILD.razel` removed (2026-06-18): there is no package-level `.razel` grammar — every
//! package is `BUILD.bazel`/`BUILD`. `MODULE.razel` remains as the (dormant) razel-native
//! module-root marker that `find_workspace_root` reads on the walk-up, and `--strict_bazel`
//! the dormant hook that makes razel ignore `*.razel`. Neither is on the live path yet —
//! the live build takes its root from `--workspace` — so they are scaffolding for a future
//! `MODULE.razel`-settings feature, not a working capability today.

use std::path::{Path, PathBuf};

/// Bazel's workspace-boundary markers (what `bazel` itself recognizes on the walk-up).
const BAZEL_BOUNDARY: [&str; 4] = ["MODULE.bazel", "REPO.bazel", "WORKSPACE.bazel", "WORKSPACE"];

/// Walk up from `start` to the nearest workspace boundary. `MODULE.razel` joins the
/// marker set in razel mode; under `strict_bazel` only Bazel's own markers bound (a
/// razel-native module is not a workspace — exactly what bazel would conclude).
pub fn find_workspace_root(start: &Path, strict_bazel: bool) -> Option<PathBuf> {
    start.ancestors().find_map(|dir| {
        let bazel = BAZEL_BOUNDARY.iter().any(|m| dir.join(m).is_file());
        let razel = !strict_bazel && dir.join("MODULE.razel").is_file();
        (bazel || razel).then(|| dir.to_path_buf())
    })
}

/// Resolve a package directory to its build file: `BUILD.bazel` over `BUILD`
/// (bazel precedence — both present ⇒ `query` serves the BUILD.bazel target).
///
/// `BUILD.razel` removed (2026-06-18): razel no longer recognizes a package-level
/// `.razel` grammar. It polluted the target space — one `BUILD.razel` made its targets
/// bazel-invisible and (via the old boundary guard) forced the whole module razel-native,
/// which defeats the bazel-compliance goal. `_strict_bazel` is retained as the dormant
/// `.razel`-visibility hook for the `MODULE.razel` settings idea (not yet wired live).
///
/// `Ok(None)` = not a package (no build file).
pub fn resolve_build_file(
    pkg_dir: &Path,
    _strict_bazel: bool,
) -> Result<Option<PathBuf>, String> {
    Ok(["BUILD.bazel", "BUILD"].iter().map(|f| pkg_dir.join(f)).find(|p| p.is_file()))
}
