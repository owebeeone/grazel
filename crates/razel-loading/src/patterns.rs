//! P1.2: the LOAD-ONLY pattern resolver — package discovery, target-pattern → packages, and
//! loading the §11 loading-phase graph WITHOUT analysis. R1#1 (RazelCrateUniversePlan): this lives
//! in `razel-loading` so `razel-query` can enumerate the graph without depending on the build
//! driver. `razel-build` reuses `discover_packages` for its analyze-mode `expand_pattern`.

use crate::loaded::LoadedTarget;
use crate::state::GlobalFlags;
use crate::workspace::resolve_build_file;
use std::collections::BTreeMap;
use std::path::Path;

/// Recursively discover packages — dirs with a resolvable BUILD file — under `root`, as
/// workspace-relative "/"-joined paths (the root package is `""`). Skips output, VCS, external,
/// and `bazel-*`/`razel-*` convenience-symlink dirs. (Moved here from `razel-build` so query
/// enumerates without the build driver.)
pub fn discover_packages(root: &Path, strict_bazel: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        if resolve_build_file(&dir, strict_bazel).ok().flatten().is_some() {
            out.push(rel.clone());
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.')
                || name.starts_with("bazel-")
                || name.starts_with("razel-")
                || matches!(name.as_str(), "external" | "node_modules" | "target")
            {
                continue;
            }
            let p = e.path();
            if p.is_dir() && !p.is_symlink() {
                let child = if rel.is_empty() { name } else { format!("{rel}/{name}") };
                stack.push((p, child));
            }
        }
    }
    out
}

/// The packages a target pattern selects, WITHOUT analysis: `//pkg:x` / `//pkg:all` → `[pkg]`;
/// `//pkg/...` → `pkg` + its subpackages; `//...` → all. The caller loads these and filters
/// `loaded_targets` to the pattern's targets. Workspace patterns only in v1 (an `@repo//…` pattern
/// is a loud error until q4 materialization, §13).
pub fn packages_for_pattern(
    root: &Path,
    pattern: &str,
    strict_bazel: bool,
) -> Result<Vec<String>, String> {
    if pattern.starts_with('@') {
        return Err(format!("external pattern `{pattern}` is not supported in query v1 (q4 — §13)"));
    }
    let body = pattern.strip_prefix("//").unwrap_or(pattern);
    // A concrete `//pkg:x` (no wildcard) → just its package; the target is filtered by the caller.
    if !pattern.contains("...") && !pattern.ends_with(":all") {
        let pkg = body.split_once(':').map(|(p, _)| p).unwrap_or(body).to_string();
        return Ok(vec![pkg]);
    }
    let pkgs = discover_packages(root, strict_bazel);
    let matched: Vec<String> = if body == "..." || body == "...:all" {
        pkgs
    } else if let Some(pfx) = body.strip_suffix("/...:all").or_else(|| body.strip_suffix("/...")) {
        pkgs.into_iter().filter(|p| p == pfx || p.starts_with(&format!("{pfx}/"))).collect()
    } else {
        let pkg = body.split_once(':').map(|(p, _)| p).unwrap_or(body);
        pkgs.into_iter().filter(|p| p == pkg).collect()
    };
    if matched.is_empty() {
        return Err(format!("no packages match `{pattern}` under {}", root.display()));
    }
    Ok(matched)
}

/// Load `packages` (deps included) and return the §11 loading-phase graph — the captured
/// `LoadedTarget`s with their resolved edges. Loading runs the capture seam + `finalize_edges`;
/// it does NOT mint the analyzed action graph for query's sake (query reads `loaded_targets`).
pub fn load_query_graph(
    root: &Path,
    flags: GlobalFlags,
    packages: &[String],
) -> BTreeMap<String, LoadedTarget> {
    let (session, _report, _loaded) = crate::rules::drive_tree(root, flags, packages, Vec::new(), 1);
    session.loaded_targets.borrow().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_to_packages_workspace_forms() {
        // a concrete label → just its package (no discovery, no fs).
        assert_eq!(packages_for_pattern(Path::new("/x"), "//a/b:t", false).unwrap(), vec!["a/b"]);
        assert_eq!(packages_for_pattern(Path::new("/x"), "//:t", false).unwrap(), vec![""]);
        // an external pattern is a named v1 error.
        assert!(packages_for_pattern(Path::new("/x"), "@crates//:x", false).unwrap_err().contains("q4"));
    }

    #[test]
    fn load_query_graph_returns_the_loading_phase_nodes() {
        let tmp = std::env::temp_dir().join(format!("razel-p12-{}", std::process::id()));
        let pkg = tmp.join("app");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(tmp.join("MODULE.bazel"), "").unwrap();
        std::fs::write(
            pkg.join("BUILD"),
            "filegroup(name = \"a\", srcs = [])\nfilegroup(name = \"b\", srcs = [\":a\"])\n",
        )
        .unwrap();
        let g = load_query_graph(&tmp, GlobalFlags::default(), &["app".to_string()]);
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(g.get("//app:a").expect("a").rule_class, "filegroup");
        // b's deps edge to :a resolved (the load-only graph carries edges).
        assert!(g.get("//app:b").expect("b").edges.iter().any(|e| e.to == "//app:a"));
    }
}
