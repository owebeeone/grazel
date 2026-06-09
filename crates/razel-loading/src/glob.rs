//! glob() support: filesystem walk for source globbing. C0.

use crate::state::Session;
use std::path::Path;



/// Shared `glob()`/`native.glob()` implementation: scan the current package dir
/// against the include/exclude patterns, package-relative, sorted.
pub(crate) fn do_glob(sess: &Session, include: Vec<String>, exclude: Vec<String>) -> anyhow::Result<Vec<String>> {
    let dir = sess
        .workspace
        .clone()
        .zip(sess.current_pkg.borrow().clone())
        .map(|(root, pkg)| root.join(&pkg));
    let Some(dir) = dir else {
        return Err(anyhow::anyhow!(
            "glob() needs a package on disk — use the workspace build path"
        ));
    };
    let mut files = Vec::new();
    walk_files(&dir, &dir, &mut files);
    let mut out: Vec<String> = files
        .into_iter()
        .filter(|f| {
            include.iter().any(|p| crate::glob_match(p, f))
                && !exclude.iter().any(|p| crate::glob_match(p, f))
        })
        .collect();
    out.sort();
    Ok(out)
}


/// Recursively collect files under `dir` as paths relative to `base` (skipping
/// dot-directories like `.razel-sandbox`/`.razel-cache`).
pub(crate) fn walk_files(dir: &Path, base: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let dot = p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('.'));
        if dot {
            continue;
        }
        if p.is_dir() {
            walk_files(&p, base, out);
        } else if let Ok(rel) = p.strip_prefix(base) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

