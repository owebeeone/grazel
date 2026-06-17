//! glob() support: filesystem walk for source globbing. C0.

use crate::state::Session;
use std::path::Path;

#[derive(Debug)]
struct GlobPattern {
    segments: Vec<String>,
}

impl GlobPattern {
    fn new(pattern: &str) -> Self {
        Self {
            segments: pattern.split('/').map(String::from).collect(),
        }
    }

    fn matches(&self, path: &[&str]) -> bool {
        seg_match(&self.segments, path)
    }
}

/// Shared `glob()`/`native.glob()` implementation: scan the current package dir
/// against the include/exclude patterns, package-relative, sorted.
pub(crate) fn do_glob(
    sess: &Session,
    include: Vec<String>,
    exclude: Vec<String>,
) -> anyhow::Result<Vec<String>> {
    // External packages (`@repo//pkg`) glob against the vendored repo's dir. Trim ALL leading `@`
    // so BOTH the apparent `@crates//` and the canonical `@@rules_rust++crate+crates//` (§11.3) forms
    // resolve — mirrors `load_package_body` (a lone `strip_prefix('@')` left the canonical `@@…` repo
    // name with a stray `@`, so `external_repo_dir` missed the vendored dir → B2 glob failure).
    // EXTERNAL crate packages (`@repo//…`) include the hidden files bazel's glob lists (`.github/*`,
    // `.gitignore`, `.cargo/config.toml`); the WORKSPACE walk keeps skipping dotfiles so `.git`/
    // `.razel-*` infra never leaks into a source glob (P6.Q1.b).
    let pkg = sess.current_pkg();
    let include_hidden = pkg.as_deref().is_some_and(|p| p.starts_with('@'));
    let dir = pkg.and_then(|pkg| {
        if pkg.starts_with('@') {
            let (repo, sub) = pkg.trim_start_matches('@').split_once("//")?;
            sess.global.external_repo_dir(repo).map(|r| r.join(sub))
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
    let files = crate::state::walk_cached(sess, &dir, include_hidden);
    let include: Vec<GlobPattern> = include.iter().map(|p| GlobPattern::new(p)).collect();
    let exclude: Vec<GlobPattern> = exclude.iter().map(|p| GlobPattern::new(p)).collect();
    let mut out = Vec::new();
    for f in files.iter() {
        let path: Vec<&str> = f.split('/').collect();
        if include.iter().any(|p| p.matches(&path)) && !exclude.iter().any(|p| p.matches(&path)) {
            out.push(f.clone());
        }
    }
    out.sort();
    sess.glob_cache
        .borrow_mut()
        .insert(key, std::sync::Arc::new(out.clone()));
    Ok(out)
}

pub(crate) fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern = GlobPattern::new(pattern);
    let path: Vec<&str> = path.split('/').collect();
    pattern.matches(&path)
}

fn seg_match(pat: &[String], path: &[&str]) -> bool {
    match pat.first().map(String::as_str) {
        None => path.is_empty(),
        Some("**") => (0..=path.len()).any(|i| seg_match(&pat[1..], &path[i..])),
        Some(seg) => {
            !path.is_empty() && star_match(seg, path[0]) && seg_match(&pat[1..], &path[1..])
        }
    }
}

/// Single-segment match with `*` = any run of non-`/` chars.
fn star_match(pat: &str, s: &str) -> bool {
    match pat.split_once('*') {
        None => pat == s,
        Some((pre, rest)) => {
            if !s.starts_with(pre) {
                return false;
            }
            let s = &s[pre.len()..];
            (0..=s.len()).any(|i| star_match(rest, &s[i..]))
        }
    }
}

/// Recursively collect files under `dir` as paths relative to `base`. `include_hidden=false` (the
/// workspace walk) skips dotfiles/dot-dirs so `.git`/`.razel-*` infra never leaks; `include_hidden=true`
/// (external crate packages) lists them, matching bazel's glob (P6.Q1.b).
pub(crate) fn walk_files(dir: &Path, base: &Path, include_hidden: bool, out: &mut Vec<String>) {
    if dir != base && (dir.join("BUILD").is_file() || dir.join("BUILD.bazel").is_file()) {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        // bazel's glob lists dotfiles; the workspace walk prunes them so `.git`/`.razel-*` infra
        // never leaks into a source glob (P6.Q1.b — external crate packages set `include_hidden`).
        if !include_hidden {
            let dot = p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'));
            if dot {
                continue;
            }
        }
        // file_type() comes from the readdir entry — no extra stat per entry. Symlinks
        // (the llvm-project overlay tree) still need the follow-stat.
        let is_dir = match e.file_type() {
            Ok(t) if t.is_symlink() => p.is_dir(),
            Ok(t) => t.is_dir(),
            Err(_) => false,
        };
        if is_dir {
            walk_files(&p, base, include_hidden, out);
        } else if let Ok(rel) = p.strip_prefix(base) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P6.Q1.b: external crate packages include the dotfiles bazel's glob lists; the WORKSPACE walk
    /// still prunes them (so `.git`/`.razel-*` never leak). `walk_files`'s `include_hidden` gates it.
    #[test]
    fn walk_files_hidden_inclusion_is_scoped_to_external() {
        let base = std::env::temp_dir().join(format!("razel-p6q1b-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let w = |rel: &str| {
            let p = base.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        };
        w("lib.rs");
        w(".gitignore");
        w(".github/workflows/ci.yml");
        w("c/blake3.c");
        w("c/.gitignore");

        let walk = |hidden: bool| {
            let mut v = Vec::new();
            walk_files(&base, &base, hidden, &mut v);
            v.sort();
            v
        };
        // Workspace (include_hidden=false): dotfiles + dot-dirs skipped (unchanged behavior).
        assert_eq!(walk(false), ["c/blake3.c", "lib.rs"]);
        // External crate (include_hidden=true): the hidden files bazel lists are included.
        assert_eq!(
            walk(true),
            [".github/workflows/ci.yml", ".gitignore", "c/.gitignore", "c/blake3.c", "lib.rs"]
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
