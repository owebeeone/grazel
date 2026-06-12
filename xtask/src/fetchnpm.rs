//! `fetch-npm` (S2, ws-razel/RazelReleaseSpike §5): npm `package-lock.json` v3 → the
//! content-addressed cache → the LOCKED `node_modules` layout, verbatim.
//!
//! The lock is the single source of truth: v3 lists literal `node_modules/...` paths
//! (nesting included) — materialization follows them with zero resolution logic.
//! Integrity is sha512 (npm's `sha512-<base64>`) → cached under
//! `cache/repos/v1/content_addressable/sha512/<hex>/file` (the round-36 schema, new
//! digest dir). Decision point closed 2026-06-12: gryth-ui carries package-lock v3
//! (261 packages, two `link:` entries to sibling grip-* dirs) — npm, by evidence.
//!
//! NON-GOALS (spike §5 S2, named holes until a real consumer demands them): NO
//! lifecycle scripts (postinstall never runs), NO native-addon builds (gyp), NO
//! workspace/monorepo features beyond gryth's own lock (`link:` entries → symlinks),
//! NO registry auth, NO `node_modules/.bin` shims (S3 invokes tool entry points by
//! direct path).

use crate::fetchcmd::{razel_output_root, sh};
use std::path::{Path, PathBuf};

/// One lock entry razel materializes.
#[derive(Debug, PartialEq)]
pub(crate) struct NpmEntry {
    /// Literal lock key: `node_modules/...` (nested paths included).
    pub path: String,
    /// Registry tarball URL, or for `link` entries the RELATIVE target dir.
    pub resolved: String,
    /// `(algo, hex)` from `integrity` — `None` for `link` entries.
    pub integrity: Option<(String, String)>,
    /// npm `link` entry: `resolved` is a directory relative to the lock's dir.
    pub link: bool,
}

/// Parse a package-lock v3. Skips the root `""` entry and bare descriptor entries
/// (local dirs like `../grip-core` — they are the TARGETS `link` entries point at).
pub(crate) fn parse_lock(json: &str) -> Result<Vec<NpmEntry>, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let ver = v.get("lockfileVersion").and_then(|x| x.as_i64()).unwrap_or(0);
    if ver != 3 {
        return Err(format!("package-lock version {ver} unsupported (v3 only — S2 scope)"));
    }
    let Some(pkgs) = v.get("packages").and_then(|p| p.as_object()) else {
        return Err("no `packages` map".into());
    };
    let mut out = Vec::new();
    for (path, e) in pkgs {
        if !path.starts_with("node_modules/") && !path.contains("/node_modules/") {
            continue; // root ("") and local-dir descriptors
        }
        let link = e.get("link").and_then(|x| x.as_bool()).unwrap_or(false);
        let resolved = e.get("resolved").and_then(|x| x.as_str()).unwrap_or_default().to_string();
        if resolved.is_empty() {
            return Err(format!("`{path}`: no `resolved` — lock not fully resolved"));
        }
        let integrity = if link {
            None
        } else {
            let raw = e
                .get("integrity")
                .and_then(|x| x.as_str())
                .ok_or_else(|| format!("`{path}`: tarball entry without integrity"))?;
            Some(integrity_hex(raw)?)
        };
        out.push(NpmEntry { path: path.clone(), resolved, integrity, link });
    }
    Ok(out)
}

/// npm integrity `sha512-<base64>` → `("sha512", <hex>)`. sha512 only — a legacy
/// sha1-only entry fails LOUD (re-lock with a modern npm), never silently unverified.
pub(crate) fn integrity_hex(s: &str) -> Result<(String, String), String> {
    let (algo, b64) = s.split_once('-').ok_or_else(|| format!("bad integrity `{s}`"))?;
    if algo != "sha512" {
        return Err(format!("integrity algo `{algo}` unsupported (sha512 only — re-lock)"));
    }
    let bytes = b64_decode(b64).ok_or_else(|| format!("bad base64 in integrity `{s}`"))?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok((algo.to_string(), hex))
}

/// Standard-alphabet base64 (npm integrity uses no urlsafe chars).
fn b64_decode(s: &str) -> Option<Vec<u8>> {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let val = |c: u8| A.iter().position(|&a| a == c).map(|p| p as u32);
    let raw: Vec<u8> = s.bytes().filter(|&c| c != b'=').collect();
    let mut out = Vec::with_capacity(raw.len() * 3 / 4);
    for chunk in raw.chunks(4) {
        let mut acc = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            acc |= val(c)? << (18 - 6 * i as u32);
        }
        let n = chunk.len();
        if n >= 2 { out.push((acc >> 16) as u8) }
        if n >= 3 { out.push((acc >> 8) as u8) }
        if n == 4 { out.push(acc as u8) }
    }
    Some(out)
}

/// Download-or-hit one tarball into the content-addressed cache (sha512 twin of
/// `fetchcmd::fetch_archive`; same dir schema, `shasum -a 512` verify, tmp+rename).
fn fetch_tarball(cache_root: &Path, url: &str, hex: &str) -> Result<(PathBuf, bool), String> {
    let dir = cache_root.join("cache/repos/v1/content_addressable/sha512").join(hex);
    let file = dir.join("file");
    if file.is_file() {
        return Ok((file, true));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = dir.join("file.razel-tmp");
    let _ = std::fs::remove_file(&tmp);
    sh("curl", &["-fsSL", "--retry", "2", "-o", &tmp.display().to_string(), url])?;
    let got = sh("shasum", &["-a", "512", &tmp.display().to_string()])?;
    let got = got.split_whitespace().next().unwrap_or_default().to_string();
    if got != hex {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("sha512 mismatch from {url}: got {got}, want {hex}"));
    }
    std::fs::rename(&tmp, &file).map_err(|e| format!("rename: {e}"))?;
    Ok((file, false))
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct NpmStats {
    pub fetched: usize,
    pub cache_hits: usize,
    pub links: usize,
}

/// Materialize a lockfile into `<out>/node_modules/...` (cache root parameterized for
/// tests; production passes `razel_output_root()`).
pub(crate) fn fetch_npm_with_root(
    cache_root: &Path,
    lock_path: &Path,
    out: &Path,
) -> Result<NpmStats, String> {
    let src = std::fs::read_to_string(lock_path)
        .map_err(|e| format!("{}: {e}", lock_path.display()))?;
    let entries = parse_lock(&src)?;
    let lock_dir = lock_path.parent().unwrap_or(Path::new("."));
    let mut stats = NpmStats::default();
    for e in &entries {
        let dest = out.join(&e.path);
        if e.link {
            // npm link entry: symlink to the target dir, resolved relative to the lock.
            let target = lock_dir.join(&e.resolved);
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p).map_err(|err| format!("{}: {err}", p.display()))?;
            }
            let _ = std::fs::remove_file(&dest);
            std::os::unix::fs::symlink(&target, &dest)
                .map_err(|err| format!("link {}: {err}", dest.display()))?;
            stats.links += 1;
            continue;
        }
        let (_, hex) = e.integrity.as_ref().expect("tarball entries carry integrity");
        let (file, hit) = fetch_tarball(cache_root, &e.resolved, hex)?;
        if hit { stats.cache_hits += 1 } else { stats.fetched += 1 }
        std::fs::create_dir_all(&dest).map_err(|err| format!("{}: {err}", dest.display()))?;
        // npm tarballs carry one top-level dir (conventionally `package/`) — strip it.
        sh(
            "tar",
            &["-xzf", &file.display().to_string(), "-C", &dest.display().to_string(), "--strip-components=1"],
        )?;
    }
    Ok(stats)
}

/// CLI entry: `cargo xtask fetch-npm <package-lock.json> <out-dir>`.
pub(crate) fn fetch_npm(lock_path: &Path, out: &Path) -> Result<(), String> {
    let stats = fetch_npm_with_root(&razel_output_root(), lock_path, out)?;
    eprintln!(
        "fetch-npm: {} fetched, {} cache hits, {} links → {}",
        stats.fetched,
        stats.cache_hits,
        stats.links,
        out.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integrity_decodes_to_hex() {
        // sha512(b"razel-npm-fixture"), vector computed independently.
        let (algo, hex) = integrity_hex(
            "sha512-cuYgZ7aBuCDh3G74xUl7k31AVtn7wu1QeTeji2K2jjtPe7Z0lxyCZ+TS2tXQDEBkcAY4XjYUvb7jYSS7URPm8g==",
        )
        .unwrap();
        assert_eq!(algo, "sha512");
        assert_eq!(
            hex,
            "72e62067b681b820e1dc6ef8c5497b937d4056d9fbc2ed507937a38b62b68e3b4f7bb674971c8267e4d2dad5d00c40647006385e3614bdbee36124bb5113e6f2"
        );
    }

    #[test]
    fn sha1_integrity_fails_loud() {
        assert!(integrity_hex("sha1-2jmj7l5rSw0yVb/vlWAYkK/YBwk=").is_err());
    }

    #[test]
    fn parse_v3_lock_shapes() {
        let lock = r#"{
          "lockfileVersion": 3,
          "packages": {
            "": {"version": "0.0.0"},
            "../grip-core": {"version": "0.2.1"},
            "node_modules/grip-core": {"resolved": "../grip-core", "link": true},
            "node_modules/a": {
              "version": "1.0.0",
              "resolved": "https://registry.npmjs.org/a/-/a-1.0.0.tgz",
              "integrity": "sha512-cuYgZ7aBuCDh3G74xUl7k31AVtn7wu1QeTeji2K2jjtPe7Z0lxyCZ+TS2tXQDEBkcAY4XjYUvb7jYSS7URPm8g=="
            },
            "node_modules/a/node_modules/b": {
              "version": "2.0.0",
              "resolved": "https://registry.npmjs.org/b/-/b-2.0.0.tgz",
              "integrity": "sha512-cuYgZ7aBuCDh3G74xUl7k31AVtn7wu1QeTeji2K2jjtPe7Z0lxyCZ+TS2tXQDEBkcAY4XjYUvb7jYSS7URPm8g=="
            }
          }
        }"#;
        let entries = parse_lock(lock).unwrap();
        assert_eq!(entries.len(), 3, "root + local-dir descriptor skipped");
        let gc = entries.iter().find(|e| e.path == "node_modules/grip-core").unwrap();
        assert!(gc.link && gc.resolved == "../grip-core" && gc.integrity.is_none());
        assert!(entries.iter().any(|e| e.path == "node_modules/a/node_modules/b"));
    }

    #[test]
    fn v2_lock_rejected() {
        assert!(parse_lock(r#"{"lockfileVersion": 2, "packages": {}}"#).is_err());
    }

    /// E2E on a file:// fixture: materialize → layout + symlink correct; re-run →
    /// 100% cache hits (offline-safe); corrupted integrity → loud mismatch, no cache.
    #[test]
    fn fixture_materializes_and_rerun_hits_cache() {
        let root = std::env::temp_dir().join(format!("razel-npm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // A fake package tarball: package/{package.json,index.js}.
        let pkg = root.join("src/package");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("package.json"), r#"{"name":"a","main":"index.js"}"#).unwrap();
        std::fs::write(pkg.join("index.js"), "module.exports = 42;\n").unwrap();
        let tgz = root.join("a-1.0.0.tgz");
        sh("tar", &["-czf", &tgz.display().to_string(), "-C", &root.join("src").display().to_string(), "package"]).unwrap();
        let hex_out = sh("shasum", &["-a", "512", &tgz.display().to_string()]).unwrap();
        let hex = hex_out.split_whitespace().next().unwrap();
        // Lock with a file:// resolved URL + a link entry to a sibling dir.
        let sib = root.join("grip-core");
        std::fs::create_dir_all(&sib).unwrap();
        let b64 = {
            // hex -> bytes -> base64 (tiny inline encoder for the fixture only)
            let bytes: Vec<u8> = (0..hex.len() / 2)
                .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
                .collect();
            const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut s = String::new();
            for c in bytes.chunks(3) {
                let n = c.len();
                let acc = (c[0] as u32) << 16
                    | (*c.get(1).unwrap_or(&0) as u32) << 8
                    | *c.get(2).unwrap_or(&0) as u32;
                s.push(A[(acc >> 18) as usize & 63] as char);
                s.push(A[(acc >> 12) as usize & 63] as char);
                s.push(if n > 1 { A[(acc >> 6) as usize & 63] as char } else { '=' });
                s.push(if n > 2 { A[acc as usize & 63] as char } else { '=' });
            }
            s
        };
        let lock = root.join("package-lock.json");
        std::fs::write(
            &lock,
            format!(
                r#"{{"lockfileVersion": 3, "packages": {{
                  "": {{}},
                  "node_modules/grip-core": {{"resolved": "grip-core", "link": true}},
                  "node_modules/a": {{"resolved": "file://{}", "integrity": "sha512-{b64}"}}
                }}}}"#,
                tgz.display()
            ),
        )
        .unwrap();
        let cache = root.join("cacheroot");
        let out = root.join("out");
        let s1 = fetch_npm_with_root(&cache, &lock, &out).unwrap();
        assert_eq!((s1.fetched, s1.cache_hits, s1.links), (1, 0, 1));
        assert!(out.join("node_modules/a/index.js").is_file(), "strip-components layout");
        assert!(out.join("node_modules/grip-core").is_symlink());
        // Offline re-run: 100% cache hits.
        let out2 = root.join("out2");
        let s2 = fetch_npm_with_root(&cache, &lock, &out2).unwrap();
        assert_eq!((s2.fetched, s2.cache_hits), (0, 1));
        // Integrity mismatch fails LOUD and caches nothing.
        let bad = lock.with_file_name("bad-lock.json");
        // Swap in a VALID-b64, WRONG-content integrity (the unit-test vector).
        let wrong = "cuYgZ7aBuCDh3G74xUl7k31AVtn7wu1QeTeji2K2jjtPe7Z0lxyCZ+TS2tXQDEBkcAY4XjYUvb7jYSS7URPm8g==";
        std::fs::write(
            &bad,
            std::fs::read_to_string(&lock).unwrap().replace(&b64, wrong),
        )
        .unwrap();
        let err = fetch_npm_with_root(&root.join("cache2"), &bad, &root.join("out3"))
            .expect_err("mismatch must fail");
        assert!(err.contains("mismatch"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
