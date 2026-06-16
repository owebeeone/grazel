//! P2.5/P2.6: `@crates` repo materialization (RazelCrateUniverseDesign §5.1). Two kinds, both
//! from the lock: the ROOT `@crates` (inline generated text — no fetch) and the PER-CRATE repos
//! (fetch+extract+patch the `.crate`, drop in `build_file_content`).

use crate::lock::{CrateLock, CrateRepo};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// P2.5: write the root `@crates` repo's inline `contents` (`BUILD.bazel`/`defs.bzl`/
/// `alias_rules.bzl`/…) to `dest` and validate the key files are present. No fetch — it is
/// generated text (§2.2). The loader evaluates `defs.bzl` + resolves the root aliases later (P3.1).
pub fn materialize_root(contents: &BTreeMap<String, String>, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    for (file, text) in contents {
        let path = dest.join(file);
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("create {}: {e}", p.display()))?;
        }
        std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    for required in ["BUILD.bazel", "defs.bzl"] {
        if !dest.join(required).exists() {
            return Err(format!("root @crates repo is missing `{required}` after materialization"));
        }
    }
    Ok(())
}

/// RazelRustParityPlan **B2** (§2.2/§5.6): materialize a SINGLE `@crates` repo from the lock, ON
/// DEMAND — the analysis path calls this the first time it needs a repo dir that isn't present.
/// The root `@crates` repo is inline generated text (no fetch); a per-crate repo is realized by
/// [`fetch_crate`] (download + sha256-verify + extract + drop the generated `BUILD.bazel`). Analysis
/// genuinely needs the real SOURCE — the generated BUILD `glob`s `**/*.rs`, sets `crate_root`, and
/// `cargo_toml_env_vars` reads `Cargo.toml` at analysis — so this is NOT BUILD-only (correcting the
/// earlier B1 assumption). Lazy + per-repo so only the requested closure (e.g. blake3's ~15 crates)
/// is fetched, not the whole 140-crate lock. `repo` is the CANONICAL name (`rules_rust++crate+crates…`,
/// the form [`crate::state::Global::external_repo_dir`] resolves). Idempotent — an already-materialized
/// dir is skipped. Returns `Ok(false)` if the lock does not define `repo` (the caller then reports its
/// own "not vendored").
pub fn materialize_one_repo(lock: &CrateLock, repo: &str, base: &Path) -> Result<bool, String> {
    let dir = base.join(repo);
    // The root `@crates` repo: inline generated text (BUILD.bazel/defs.bzl/alias_rules.bzl/…), no fetch.
    if lock.canonical_repo("crates").as_deref() == Some(repo) {
        if !dir.join("BUILD.bazel").exists() {
            materialize_root(&lock.root_contents, &dir)?;
        }
        return Ok(true);
    }
    // A per-crate repo: canonical → apparent (`crates__<name>-<ver>`) → its `.crate` fetch spec.
    let apparent = repo.strip_prefix(&lock.canonical_prefix).unwrap_or(repo);
    let Some(spec) = lock.crates.get(apparent) else {
        return Ok(false); // not a crate_universe repo this lock defines
    };
    // Idempotent: an extracted source (Cargo.toml present) means this repo was already fetched.
    if !dir.join("Cargo.toml").exists() {
        fetch_crate(spec, &dir)?;
    }
    Ok(true)
}

/// Lowercase-hex sha256 of bytes (the lock's `.crate` digest form).
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// P2.6: realize a per-crate repo — download the `.crate` (sha256-verified), extract it (applying
/// `strip_prefix`), and drop in `build_file_content` as the package `BUILD.bazel`. Content of the
/// extracted tree is keyed by §4.4 (the caller skips re-fetch on a hit). Uses system `curl`/`tar`
/// (no extra deps); the pure path is required under the parity goldens by rung 4 (§5.1).
pub fn fetch_crate(spec: &CrateRepo, dest: &Path) -> Result<(), String> {
    let url = spec.urls.first().ok_or("crate spec has no urls")?;

    // download (sha256-verified)
    let tmp = dest.with_extension("crate.download");
    let status = Command::new("curl")
        .args(["-sSL", "--fail", "--max-time", "120", "-o"])
        .arg(&tmp)
        .arg(url)
        .status()
        .map_err(|e| format!("spawn curl: {e}"))?;
    if !status.success() {
        return Err(format!("downloading {url} failed (curl {status})"));
    }
    let bytes = std::fs::read(&tmp).map_err(|e| format!("read download: {e}"))?;
    let got = sha256_hex(&bytes);
    if got != spec.sha256 {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("sha256 mismatch for {url}: got {got}, expected {}", spec.sha256));
    }

    // extract (tar.gz) with strip-components from strip_prefix, then drop the generated BUILD.
    std::fs::create_dir_all(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    let strip = if spec.strip_prefix.is_some() { "1" } else { "0" };
    let status = Command::new("tar")
        .args(["xzf"])
        .arg(&tmp)
        .args(["--strip-components", strip, "-C"])
        .arg(dest)
        .status()
        .map_err(|e| format!("spawn tar: {e}"))?;
    let _ = std::fs::remove_file(&tmp);
    if !status.success() {
        return Err(format!("extracting {url} failed (tar {status})"));
    }
    std::fs::write(dest.join("BUILD.bazel"), &spec.build_file_content)
        .map_err(|e| format!("write generated BUILD.bazel: {e}"))?;
    Ok(())
}

/// P2.7: the INTERIM dev-only cache — copy Bazel's already-fetched repo tree (e.g.
/// `bazel-bin/external/rules_rust++crate+crates__blake3-1.8.2`) to `dest` instead of downloading.
/// Defers download/extract while the build lands; **NOT parity-gating** — reading Bazel's tree
/// masks RepoFetch/patch/§4.4-key bugs, so the pure [`fetch_crate`] path is the one under the
/// goldens by rung 4 (§5.1).
pub fn read_from_bazel_external(
    repo_canonical: &str,
    external_root: &Path,
    dest: &Path,
) -> Result<(), String> {
    let src = external_root.join(repo_canonical);
    if !src.is_dir() {
        return Err(format!(
            "bazel external repo `{repo_canonical}` not found under {} (run `bazel fetch` first, \
             or use the pure RepoFetch path)",
            external_root.display()
        ));
    }
    copy_tree(&src, dest).map_err(|e| format!("copy bazel external tree: {e}"))
}

/// Recursively copy `from`'s subtree into `to` (created). Follows symlinks (Bazel's external tree
/// is symlink-heavy) by copying their targets' bytes.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if src.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?; // is_dir() follows symlinks, so this copies link targets
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materialize_root_writes_and_validates() {
        let tmp = std::env::temp_dir().join(format!("razel-mat-{}", std::process::id()));
        let mut contents = BTreeMap::new();
        contents.insert("BUILD.bazel".to_string(), "exports_files([])\n".to_string());
        contents.insert("defs.bzl".to_string(), "X = 1\n".to_string());
        materialize_root(&contents, &tmp).unwrap();
        assert_eq!(std::fs::read_to_string(tmp.join("defs.bzl")).unwrap(), "X = 1\n");

        // missing BUILD.bazel → loud error.
        let mut bad = BTreeMap::new();
        bad.insert("defs.bzl".to_string(), "X=1\n".to_string());
        let bad_dest = tmp.join("bad");
        assert!(materialize_root(&bad, &bad_dest).unwrap_err().contains("missing `BUILD.bazel`"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn materialize_one_repo_root_is_inline_idempotent_and_scoped() {
        // B2: the ROOT @crates repo materializes inline (no fetch/network); a repo the lock does not
        // define returns Ok(false); a second call is a no-op. (The per-crate FETCH path needs network
        // → exercised by the B2 dev-probe `blake3_closure`, not here.)
        let mut root_contents = BTreeMap::new();
        root_contents.insert("BUILD.bazel".to_string(), "exports_files([])\n".to_string());
        root_contents.insert("defs.bzl".to_string(), "X = 1\n".to_string());
        let lock = CrateLock {
            version: 18,
            root_contents,
            crates: BTreeMap::new(),
            recorded_inputs: vec![],
            canonical_prefix: "rules_rust++crate+".to_string(),
        };
        let tmp = std::env::temp_dir().join(format!("razel-one-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);

        // The root repo materializes inline, under its canonical name.
        assert!(materialize_one_repo(&lock, "rules_rust++crate+crates", &tmp).unwrap());
        assert_eq!(
            std::fs::read_to_string(tmp.join("rules_rust++crate+crates/defs.bzl")).unwrap(),
            "X = 1\n"
        );
        // Idempotent: a second call is a no-op (no error, still present).
        assert!(materialize_one_repo(&lock, "rules_rust++crate+crates", &tmp).unwrap());
        // A repo the lock does NOT define → Ok(false), no fetch attempted.
        assert!(!materialize_one_repo(&lock, "rules_rust++crate+crates__nope-9.9.9", &tmp).unwrap());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn sha256_matches_known_vector() {
        // sha256("") is the well-known empty-string digest.
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn interim_cache_copies_a_bazel_external_tree() {
        // P2.7: a fake bazel external/ tree → copied to dest (the dev-only no-fetch path).
        let tmp = std::env::temp_dir().join(format!("razel-interim-{}", std::process::id()));
        let ext = tmp.join("external");
        let repo = "rules_rust++crate+crates__blake3-1.8.2";
        std::fs::create_dir_all(ext.join(repo).join("src")).unwrap();
        std::fs::write(ext.join(repo).join("BUILD.bazel"), "rust_library(...)\n").unwrap();
        std::fs::write(ext.join(repo).join("src/lib.rs"), "//! blake3\n").unwrap();

        let dest = tmp.join("out");
        read_from_bazel_external(repo, &ext, &dest).unwrap();
        assert!(dest.join("BUILD.bazel").exists());
        assert_eq!(std::fs::read_to_string(dest.join("src/lib.rs")).unwrap(), "//! blake3\n");
        // a missing repo → loud error.
        assert!(read_from_bazel_external("nope", &ext, &dest).unwrap_err().contains("not found"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    #[ignore = "network: downloads the real blake3 .crate from static.crates.io"]
    fn fetch_crate_downloads_verifies_extracts() {
        let spec = CrateRepo {
            urls: vec!["https://static.crates.io/crates/blake3/1.8.2/download".into()],
            sha256: "3888aaa89e4b2a40fca9848e400f6a658a5a3978de7be858e209cafa8be9a4a0".into(),
            strip_prefix: Some("blake3-1.8.2".into()),
            build_file_content: "# generated build\n".into(),
            remote_patch_strip: Some(1),
            archive_type: Some("tar.gz".into()),
        };
        let tmp = std::env::temp_dir().join(format!("razel-fetch-{}", std::process::id()));
        fetch_crate(&spec, &tmp).unwrap();
        assert!(tmp.join("Cargo.toml").exists(), "extracted crate root has Cargo.toml");
        assert!(tmp.join("src").is_dir(), "src/ extracted (strip_prefix applied)");
        assert!(tmp.join("BUILD.bazel").exists(), "generated BUILD.bazel dropped in");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
