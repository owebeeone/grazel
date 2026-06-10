//! glob() support: filesystem walk for source globbing. C0.

use crate::state::Session;
use std::path::Path;



/// Shared `glob()`/`native.glob()` implementation: scan the current package dir
/// against the include/exclude patterns, package-relative, sorted.
pub(crate) fn do_glob(sess: &Session, include: Vec<String>, exclude: Vec<String>) -> anyhow::Result<Vec<String>> {
    // External packages (`@repo//pkg`) glob against the vendored repo's dir.
    let dir = sess.current_pkg.borrow().clone().and_then(|pkg| {
        if let Some(rest) = pkg.strip_prefix('@') {
            let (repo, sub) = rest.split_once("//")?;
            let base = sess.global.external_base.clone()?;
            [repo.to_string(), repo.replace('_', "-")]
                .iter()
                .map(|d| base.join(d))
                .find(|p| p.exists())
                .map(|r| r.join(sub))
        } else {
            sess.workspace.clone().map(|root| root.join(&pkg))
        }
    });
    let Some(dir) = dir else {
        return Err(anyhow::anyhow!(
            "glob() needs a package on disk — use the workspace build path"
        ));
    };
    // Result memo: identical (dir, patterns) globs repeat across TF's macro layer.
    let key = (dir.clone(), include.join(","), exclude.join(","));
    if let Some(hit) = sess.glob_cache.borrow().get(&key) {
        return Ok(hit.as_ref().clone());
    }
    let files = crate::state::walk_cached(sess, &dir);
    let mut out: Vec<String> = files
        .iter()
        .filter(|f| {
            include.iter().any(|p| crate::glob_match(p, f))
                && !exclude.iter().any(|p| crate::glob_match(p, f))
        })
        .cloned()
        .collect();
    out.sort();
    sess.glob_cache.borrow_mut().insert(key, std::sync::Arc::new(out.clone()));
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
        // file_type() comes from the readdir entry — no extra stat per entry. Symlinks
        // (the llvm-project overlay tree) still need the follow-stat.
        let is_dir = match e.file_type() {
            Ok(t) if t.is_symlink() => p.is_dir(),
            Ok(t) => t.is_dir(),
            Err(_) => false,
        };
        if is_dir {
            walk_files(&p, base, out);
        } else if let Ok(rel) = p.strip_prefix(base) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

