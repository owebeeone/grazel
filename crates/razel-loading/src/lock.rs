//! P2.1: the `MODULE.bazel.lock` reader (RazelCrateUniverseDesign §2). razel consumes the
//! committed lock as the source of truth for `@crates` — it never runs cargo-bazel at build time.
//! Anything off-schema is a LOUD error, never a silent skip.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// The accepted `lockFileVersion`s. This workspace's lock is `26`; Bazel HEAD is already `28`
/// (`BazelLockFileValue.LOCK_FILE_VERSION`), so the format MOVES — the reader is version-aware and
/// errors loudly on an unknown version rather than mis-parsing (§2.1).
const ACCEPTED_VERSIONS: &[u32] = &[26];

/// The crate_universe extension's canonical key (`ModuleExtensionId.toString()`), modulo an
/// optional `%<isolationKey>` suffix (absent for crate_universe's non-isolated `crate`).
const CRATE_EXT_PREFIX: &str = "@@rules_rust+//crate_universe:extensions.bzl%crate";

/// The parsed `@crates` world: the root repo (inline generated text) + the per-crate fetch specs +
/// the raw staleness inputs (parsed in P2.2).
#[derive(Debug, Clone, PartialEq)]
pub struct CrateLock {
    pub version: u32,
    /// The root `@crates` repo's inline files (`BUILD.bazel`/`defs.bzl`/`alias_rules.bzl`/…).
    pub root_contents: BTreeMap<String, String>,
    /// Per-crate repos: extension-local name (`crates__<name>-<ver>`) → fetch spec.
    pub crates: BTreeMap<String, CrateRepo>,
    /// Raw `recordedInputs` tagged strings (the §2.3 grammar parses these).
    pub recorded_inputs: Vec<String>,
    /// The canonical `@@`-repo prefix for this extension's repos (`rules_rust++crate+`), derived
    /// from the extension key. Apparent `crates__<name>-<ver>` + this = the name Bazel
    /// materializes under `external/` (§11.3). See [`CrateLock::canonical_repo`].
    pub canonical_prefix: String,
}

impl CrateLock {
    /// The canonical `@@`-repo name for an apparent crate repo (`crates`, `crates__<name>-<ver>`),
    /// or the input unchanged if it is already canonical; `None` if `repo` is not a crate_universe
    /// repo this lock defines. `@crates//:x` and `@@rules_rust++crate+crates//:x` name the same
    /// target — the accept-both-forms rule (§11.3). Non-crate repos (`@rules_rust`, `@platforms`)
    /// return `None`, so callers canonicalize only `@crates` labels.
    pub fn canonical_repo(&self, repo: &str) -> Option<String> {
        let defines = |apparent: &str| apparent == "crates" || self.crates.contains_key(apparent);
        if let Some(apparent) = repo.strip_prefix(&self.canonical_prefix) {
            return defines(apparent).then(|| repo.to_string());
        }
        defines(repo).then(|| format!("{}{repo}", self.canonical_prefix))
    }
}

/// Derive the canonical `@@`-repo prefix for the crate_universe extension's repos from its
/// extension key (`ModuleExtensionId.toString()`): the module repo (before `//`) joined to the
/// extension name (the first `%`-segment after the `.bzl` label) by `+`. For
/// `@@rules_rust+//crate_universe:extensions.bzl%crate` → `rules_rust++crate+`, so apparent
/// `crates__blake3-1.8.2` → `rules_rust++crate+crates__blake3-1.8.2` (§11.3). Tolerates a trailing
/// `%<isolationKey>` on the key.
fn crate_canonical_prefix(ext_key: &str) -> Option<String> {
    let key = ext_key.strip_prefix("@@")?;
    let (module_repo, rest) = key.split_once("//")?;
    let ext_name = rest.split_once('%')?.1.split('%').next()?;
    Some(format!("{module_repo}+{ext_name}+"))
}

/// A per-crate repo: a `.crate` to fetch + its generated package `BUILD` (§2.2, §4.4).
#[derive(Debug, Clone, PartialEq)]
pub struct CrateRepo {
    pub urls: Vec<String>,
    pub sha256: String,
    pub strip_prefix: Option<String>,
    pub build_file_content: String,
    /// `http_archive`'s per-crate patch strip level (observed on `crates__blake3-1.8.2`, §4.4).
    pub remote_patch_strip: Option<u32>,
    pub archive_type: Option<String>,
}

// ── the on-disk JSON shape (only what we read) ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct LockFile {
    #[serde(rename = "lockFileVersion")]
    lock_file_version: u32,
    #[serde(rename = "moduleExtensions", default)]
    module_extensions: BTreeMap<String, ModuleExt>,
}

#[derive(Deserialize)]
struct ModuleExt {
    general: Option<General>,
}

#[derive(Deserialize)]
struct General {
    #[serde(rename = "recordedInputs", default)]
    recorded_inputs: Vec<String>,
    #[serde(rename = "generatedRepoSpecs", default)]
    generated_repo_specs: BTreeMap<String, RepoSpec>,
}

#[derive(Deserialize)]
struct RepoSpec {
    #[serde(rename = "repoRuleId")]
    repo_rule_id: String,
    #[serde(default)]
    attributes: serde_json::Value,
}

/// Read + validate `MODULE.bazel.lock` into the `@crates` world. Loud errors throughout.
pub fn read_lock(path: &Path) -> Result<CrateLock, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    parse_lock(&text)
}

/// Parse + validate the lock text (split out for testing).
pub fn parse_lock(text: &str) -> Result<CrateLock, String> {
    let lock: LockFile =
        serde_json::from_str(text).map_err(|e| format!("MODULE.bazel.lock is not valid JSON: {e}"))?;

    if !ACCEPTED_VERSIONS.contains(&lock.lock_file_version) {
        return Err(format!(
            "MODULE.bazel.lock lockFileVersion {} is unsupported (accepted: {:?}; Bazel HEAD is \
             already 28 — regenerate the lock with a compatible Bazel, or extend the reader)",
            lock.lock_file_version, ACCEPTED_VERSIONS
        ));
    }

    // The crate_universe extension — accept the canonical key + an optional isolation suffix.
    let general = lock
        .module_extensions
        .iter()
        .find(|(k, _)| {
            k.as_str() == CRATE_EXT_PREFIX
                || k.strip_prefix(CRATE_EXT_PREFIX).is_some_and(|s| s.starts_with('%'))
        })
        .and_then(|(_, ext)| ext.general.as_ref())
        .ok_or_else(|| {
            format!("MODULE.bazel.lock has no `{CRATE_EXT_PREFIX}` extension — is @crates configured?")
        })?;

    let specs = &general.generated_repo_specs;
    let mut root_contents = None;
    let mut crates = BTreeMap::new();
    for (name, spec) in specs {
        if spec.repo_rule_id.ends_with("%_generate_repo") {
            // The root `@crates` repo: inline `contents`. (Ignore the sibling `_generate_repo`
            // helper extension; the root is the one whose contents we materialize.)
            if name == "crates" || root_contents.is_none() {
                root_contents = Some(parse_root(name, &spec.attributes)?);
            }
        } else if spec.repo_rule_id.contains("http_archive") {
            crates.insert(name.clone(), parse_crate(name, &spec.attributes)?);
        } else {
            return Err(format!(
                "MODULE.bazel.lock repo `{name}` has unknown repoRuleId `{}`",
                spec.repo_rule_id
            ));
        }
    }

    let root_contents = root_contents
        .ok_or("MODULE.bazel.lock has no root `@crates` repo (the inline generated contents)")?;

    // The extension key always starts with CRATE_EXT_PREFIX (matched above), so the canonical
    // prefix derives from the const regardless of any isolation suffix.
    let canonical_prefix = crate_canonical_prefix(CRATE_EXT_PREFIX)
        .ok_or("internal: cannot derive canonical repo prefix from the crate extension key")?;

    Ok(CrateLock {
        version: lock.lock_file_version,
        root_contents,
        crates,
        recorded_inputs: general.recorded_inputs.clone(),
        canonical_prefix,
    })
}

/// The root repo's `attributes.contents` — a `{filename: text}` map.
fn parse_root(name: &str, attrs: &serde_json::Value) -> Result<BTreeMap<String, String>, String> {
    let contents = attrs
        .get("contents")
        .and_then(|c| c.as_object())
        .ok_or_else(|| format!("root repo `{name}` is missing `attributes.contents`"))?;
    let mut out = BTreeMap::new();
    for (file, text) in contents {
        let text = text
            .as_str()
            .ok_or_else(|| format!("root repo `{name}` content `{file}` is not a string"))?;
        out.insert(file.clone(), text.to_string());
    }
    Ok(out)
}

/// A per-crate `http_archive` spec — `urls`/`sha256`/`build_file_content` are required (§2.1).
fn parse_crate(name: &str, attrs: &serde_json::Value) -> Result<CrateRepo, String> {
    let req_str = |k: &str| -> Result<String, String> {
        attrs
            .get(k)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| format!("crate repo `{name}` is missing required `{k}`"))
    };
    let urls = attrs
        .get("urls")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|u| u.as_str().map(str::to_string)).collect::<Vec<_>>())
        .filter(|v: &Vec<String>| !v.is_empty())
        .ok_or_else(|| format!("crate repo `{name}` is missing required `urls`"))?;
    Ok(CrateRepo {
        urls,
        sha256: req_str("sha256")?,
        build_file_content: req_str("build_file_content")?,
        strip_prefix: attrs.get("strip_prefix").and_then(|v| v.as_str()).map(str::to_string),
        remote_patch_strip: attrs.get("remote_patch_strip").and_then(serde_json::Value::as_u64).map(|n| n as u32),
        archive_type: attrs.get("type").and_then(|v| v.as_str()).map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "lockFileVersion": 26,
      "moduleExtensions": {
        "@@rules_rust+//crate_universe:extensions.bzl%crate": {
          "general": {
            "recordedInputs": ["ENV:CARGO_BAZEL_REPIN \\0", "FILE:@@//Cargo.lock abc"],
            "generatedRepoSpecs": {
              "crates": {
                "repoRuleId": "@@rules_rust+//crate_universe:extensions.bzl%_generate_repo",
                "attributes": { "contents": { "BUILD.bazel": "exports_files([])\n", "defs.bzl": "X=1\n" } }
              },
              "crates__blake3-1.8.2": {
                "repoRuleId": "@@bazel_tools//tools/build_defs/repo:http.bzl%http_archive",
                "attributes": {
                  "remote_patch_strip": 1,
                  "sha256": "deadbeef",
                  "type": "tar.gz",
                  "urls": ["https://static.crates.io/crates/blake3/1.8.2/download"],
                  "strip_prefix": "blake3-1.8.2",
                  "build_file_content": "rust_library(...)\n"
                }
              }
            }
          }
        }
      }
    }"#;

    #[test]
    fn reads_root_and_per_crate_specs() {
        let lock = parse_lock(FIXTURE).unwrap();
        assert_eq!(lock.version, 26);
        assert_eq!(lock.root_contents.get("BUILD.bazel").unwrap(), "exports_files([])\n");
        let blake3 = lock.crates.get("crates__blake3-1.8.2").unwrap();
        assert_eq!(blake3.sha256, "deadbeef");
        assert_eq!(blake3.urls, ["https://static.crates.io/crates/blake3/1.8.2/download"]);
        assert_eq!(blake3.strip_prefix.as_deref(), Some("blake3-1.8.2"));
        assert_eq!(blake3.remote_patch_strip, Some(1));
        assert_eq!(lock.recorded_inputs.len(), 2);
    }

    #[test]
    fn unknown_version_is_a_loud_error() {
        let bad = FIXTURE.replace("\"lockFileVersion\": 26", "\"lockFileVersion\": 28");
        assert!(parse_lock(&bad).unwrap_err().contains("lockFileVersion 28 is unsupported"));
    }

    #[test]
    fn missing_required_crate_field_errors() {
        let bad = FIXTURE.replace("\"sha256\": \"deadbeef\",", "");
        assert!(parse_lock(&bad).unwrap_err().contains("missing required `sha256`"));
    }

    #[test]
    fn missing_crate_extension_errors() {
        let bad = FIXTURE.replace("crate_universe:extensions.bzl%crate", "other:extensions.bzl%other");
        assert!(parse_lock(&bad).unwrap_err().contains("no `@@rules_rust"));
    }

    #[test]
    fn reads_the_real_workspace_lock() {
        // The committed 940KB lock (228 specs) — authoritative validation of the reader.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../MODULE.bazel.lock");
        if !path.exists() {
            return; // not in this checkout — skip rather than fail
        }
        let lock = read_lock(&path).unwrap();
        assert_eq!(lock.version, 26);
        assert!(lock.crates.contains_key("crates__blake3-1.8.2"), "blake3 per-crate spec present");
        assert!(lock.crates.len() > 100, "many per-crate specs ({})", lock.crates.len());
        assert!(lock.root_contents.contains_key("defs.bzl"), "root @crates defs.bzl present");
        assert!(!lock.recorded_inputs.is_empty(), "recordedInputs present");
    }

    // crate-universe P3.1c: the apparent→canonical repo mapping (§11.3 accept-both-forms).
    #[test]
    fn canonical_repo_maps_both_forms_and_only_crate_repos() {
        // The prefix is DERIVED from the extension key (carries the module's `+` version marker).
        assert_eq!(crate_canonical_prefix(CRATE_EXT_PREFIX).as_deref(), Some("rules_rust++crate+"));
        // …and tolerates an isolation suffix on the key.
        assert_eq!(
            crate_canonical_prefix("@@rules_rust+//crate_universe:extensions.bzl%crate%foo+bar")
                .as_deref(),
            Some("rules_rust++crate+"),
        );

        let lock = parse_lock(FIXTURE).unwrap();
        assert_eq!(lock.canonical_prefix, "rules_rust++crate+");
        // apparent → canonical: the root and a per-crate repo.
        assert_eq!(lock.canonical_repo("crates").as_deref(), Some("rules_rust++crate+crates"));
        assert_eq!(
            lock.canonical_repo("crates__blake3-1.8.2").as_deref(),
            Some("rules_rust++crate+crates__blake3-1.8.2"),
        );
        // accept-both-forms: the canonical name maps to itself.
        assert_eq!(
            lock.canonical_repo("rules_rust++crate+crates__blake3-1.8.2").as_deref(),
            Some("rules_rust++crate+crates__blake3-1.8.2"),
        );
        // not a crate_universe repo this lock defines → None (don't canonicalize @rules_rust etc.).
        assert_eq!(lock.canonical_repo("rules_rust"), None);
        assert_eq!(lock.canonical_repo("platforms"), None);
        // canonical-form of an UNKNOWN crate is also None (the apparent part must be defined).
        assert_eq!(lock.canonical_repo("rules_rust++crate+crates__nope-9.9.9"), None);
    }

    #[test]
    fn real_lock_canonicalizes_blake3_to_the_bazel_external_dir_name() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../MODULE.bazel.lock");
        if !path.exists() {
            return;
        }
        let lock = read_lock(&path).unwrap();
        // Matches Bazel's real dir: bazel-*/external/rules_rust++crate+crates__blake3-1.8.2.
        assert_eq!(
            lock.canonical_repo("crates__blake3-1.8.2").as_deref(),
            Some("rules_rust++crate+crates__blake3-1.8.2"),
        );
        assert_eq!(lock.canonical_repo("crates").as_deref(), Some("rules_rust++crate+crates"));
    }
}
