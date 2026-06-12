//! S1 (V3sh1): E-mode workspace structure — boundary walk-up, package-file resolution
//! (the `BUILD.razel` XOR rule), and the `.bazelignore` boundary guard (spike §3c rule 2).
//!
//! Design E (ws-razel/RazelReleaseSpike §3c): `BUILD.razel` exists only as a package's
//! SOLE grammar; razel-native packages live where Bazel is provably blind (razel-native
//! modules, or `.bazelignore`d subtrees of dual workspaces). `--strict_bazel` (§3d) acts
//! as if it IS bazel: `.razel` files are invisible, never an error.

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

/// Does `root` carry Bazel boundary grammar (i.e. is the workspace DUAL — openable by
/// bazel — rather than razel-native)?
pub(crate) fn root_is_dual(root: &Path) -> bool {
    BAZEL_BOUNDARY.iter().any(|m| root.join(m).is_file())
}

/// Resolve a package directory to its build file. Encodes two rules:
/// - **Bazel precedence** (ground truth 2026-06-12: both files present ⇒ `query` serves
///   the BUILD.bazel target): `BUILD.bazel` over `BUILD`.
/// - **The E-mode XOR** (§3c): `BUILD.razel` is a package's sole grammar — coexistence
///   with bazel grammar is an error. Under `strict_bazel`, `.razel` files are invisible
///   (no package, no error — bazel's view).
///
/// `Ok(None)` = not a package (no build file at all, from this mode's viewpoint).
pub(crate) fn resolve_build_file(
    pkg_dir: &Path,
    strict_bazel: bool,
) -> Result<Option<PathBuf>, String> {
    let bazel = ["BUILD.bazel", "BUILD"].iter().map(|f| pkg_dir.join(f)).find(|p| p.is_file());
    if strict_bazel {
        return Ok(bazel);
    }
    let razel = pkg_dir.join("BUILD.razel");
    if razel.is_file() {
        if let Some(b) = bazel {
            return Err(format!(
                "package `{}` has both BUILD.razel and {} — BUILD.razel is a package's \
                 SOLE grammar (E-mode XOR): remove one, or move the razel-native targets \
                 to their own package",
                pkg_dir.display(),
                b.file_name().unwrap_or_default().to_string_lossy(),
            ));
        }
        return Ok(Some(razel));
    }
    Ok(bazel)
}

/// The boundary guard (§3c rule 2), pure for testability: in a DUAL workspace, an
/// E-package must sit under a `.bazelignore`d subtree, else the two engines disagree on
/// package boundaries (glob/subpackage shadowing). Returns the warning text when the
/// guard trips. S1 contract: WARNING; becomes an ERROR at S3.
pub fn e_mode_guard(
    root_is_dual: bool,
    bazelignore: Option<&str>,
    pkg: &str,
) -> Option<String> {
    if !root_is_dual {
        return None; // razel-native module: bazel can't open the workspace at all.
    }
    let covered = bazelignore.is_some_and(|src| {
        src.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .any(|l| pkg == l || pkg.starts_with(&format!("{l}/")))
    });
    if covered {
        return None;
    }
    Some(format!(
        "WARNING: razel-native package `{pkg}` (BUILD.razel) is not under a .bazelignore'd \
         subtree of this dual workspace — bazel and razel will disagree on package \
         boundaries there. Add the subtree to .bazelignore. (E-mode boundary guard; \
         this becomes an ERROR at S3.)"
    ))
}
