//! `state::tools` — split from `state.rs` (facade in `mod.rs`).

use super::*;

pub(crate) const CXX: &str = "/usr/bin/c++";

pub(crate) const AR: &str = "/usr/bin/ar";

/// The resolved native (host) cc compiler, by walking `PATH` (§7 ·iii — the Native toolchain). This
/// is what Bazel's `cc_configure` does (probe the host); razel does it at build time. Resolved +
/// logged **once**; `CXX` is the fallback when no candidate is on `PATH`.
// (the host-cc resolver now lives on `Session::host_cc` — AD2: per-Session, not a process-global
// OnceLock; F13. The pure PATH-walk is `first_on_path`, the identity is `tool_id`.)


/// First `<dir>/<candidate>` for which `exists` holds — PATH-walk, candidates in preference order.
/// Pure (dirs + probe injected) so it's testable without touching the environment.
pub(crate) fn first_on_path(
    candidates: &[&str],
    dirs: &[&str],
    exists: impl Fn(&std::path::Path) -> bool,
) -> Option<String> {
    for cand in candidates {
        for dir in dirs {
            let p = std::path::Path::new(dir).join(cand);
            if exists(&p) {
                return Some(p.to_string_lossy().into_owned());
            }
        }
    }
    None
}


/// A cheap stable identity for a resolved tool: `size@mtime` from one stat — the fast-path proxy.
/// (The content digest that actually keys actions is the follow-on, RazelGaps "toolchain-change
/// cache"; this is enough to log + later gate the re-hash.)
pub(crate) fn tool_id(path: &str) -> String {
    match std::fs::metadata(path) {
        Ok(m) => {
            let secs = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!("{}b@{secs}", m.len())
        }
        Err(_) => "absent".to_string(),
    }
}


/// Cached file-existence check (the dep/file-label fallbacks stat per srcs entry).
pub(crate) fn path_is_file(sess: &Session, p: &std::path::Path) -> bool {
    if let Some(&hit) = sess.exists_cache.borrow().get(p) {
        return hit;
    }
    let v = p.is_file();
    sess.exists_cache.borrow_mut().insert(p.to_path_buf(), v);
    v
}


/// Cached RECURSIVE file listing under `dir` (paths relative to it) for glob().
pub(crate) fn walk_cached(sess: &Session, dir: &std::path::Path) -> std::sync::Arc<Vec<String>> {
    if let Some(hit) = sess.walk_cache.borrow().get(dir) {
        return hit.clone();
    }
    let mut files = Vec::new();
    crate::glob::walk_files(dir, dir, &mut files);
    let arc = std::sync::Arc::new(files);
    sess.walk_cache
        .borrow_mut()
        .insert(dir.to_path_buf(), arc.clone());
    arc
}


/// The session's host tool triple (rustc, cc, sysroot) — discovered ONCE per session.
pub(crate) fn host_tools(sess: &Session) -> (String, String, String) {
    if let Some(t) = sess.host_tools.borrow().as_ref() {
        return t.clone();
    }
    let find = |name: &str| -> String {
        std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .map(|d| std::path::Path::new(d).join(name))
            .find(|p| p.is_file())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| name.to_string())
    };
    let sysroot = if std::env::consts::OS == "macos" {
        std::process::Command::new("xcrun")
            .args(["--show-sdk-path"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/".to_string())
    } else {
        "/".to_string()
    };
    let t = (find("rustc"), find("cc"), sysroot);
    *sess.host_tools.borrow_mut() = Some(t.clone());
    t
}


