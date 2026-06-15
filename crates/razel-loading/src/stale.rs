//! P2.3: stale-lock detection (RazelCrateUniverseDesign §2.3). razel cannot regenerate the lock
//! offline, so slice 1 gates staleness on the `FILE`/`DIR*` WORKSPACE inputs only — rehash +
//! compare sha256. `ENV`/`REPO_MAPPING` are informational (a deliberate, documented divergence:
//! they describe the generation environment the committed lock already froze). A mismatch is a
//! LOUD error ("regenerate offline"), never a silent stale build.

use crate::recorded::{FileValue, RecordedInput};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Verify the workspace `FILE` inputs still match the lock's recorded sha256/state. `@@//<path>`
/// is the workspace root. A changed/absent/changed-type file → loud error.
pub fn check_stale(root: &Path, recorded: &[RecordedInput]) -> Result<(), String> {
    for input in recorded {
        // Only workspace files (`@@//…`) gate; ENV/REPO_MAPPING and non-workspace files are skipped.
        let RecordedInput::File { label, value } = input else { continue };
        let Some(rel) = label.strip_prefix("@@//") else { continue };
        let path = root.join(rel);
        match value {
            FileValue::Sha256(expected) => {
                let actual = sha256_file(&path).map_err(|e| {
                    format!("MODULE.bazel.lock input `{rel}` is unreadable (stale?): {e}")
                })?;
                if &actual != expected {
                    return Err(format!(
                        "MODULE.bazel.lock is stale — `{rel}` changed (sha256 {actual} != recorded \
                         {expected}); regenerate the lock offline"
                    ));
                }
            }
            FileValue::Enoent => {
                if path.exists() {
                    return Err(format!(
                        "MODULE.bazel.lock is stale — `{rel}` now exists (was absent); regenerate offline"
                    ));
                }
            }
            FileValue::Dir => {
                if !path.is_dir() {
                    return Err(format!(
                        "MODULE.bazel.lock is stale — `{rel}` is no longer a directory; regenerate offline"
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Lowercase-hex sha256 of a file's bytes (matches the lock's `FILE` digest form).
fn sha256_file(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(label: &str, value: FileValue) -> RecordedInput {
        RecordedInput::File { label: label.into(), value }
    }

    #[test]
    fn fresh_passes_changed_fails() {
        let tmp = std::env::temp_dir().join(format!("razel-stale-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("Cargo.lock"), "deps\n").unwrap();
        let good = sha256_file(&tmp.join("Cargo.lock")).unwrap();

        assert!(check_stale(&tmp, &[file("@@//Cargo.lock", FileValue::Sha256(good))]).is_ok());
        let err = check_stale(&tmp, &[file("@@//Cargo.lock", FileValue::Sha256("deadbeef".into()))])
            .unwrap_err();
        assert!(err.contains("stale") && err.contains("Cargo.lock"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn env_and_non_workspace_are_not_gated() {
        // ENV inputs + a missing path are skipped (informational / non-FILE).
        let recorded = vec![
            RecordedInput::Env { name: "CARGO_BAZEL_REPIN".into(), value: None },
            RecordedInput::RepoMapping { source: "a".into(), apparent: "b".into(), canonical: None },
        ];
        assert!(check_stale(Path::new("/nonexistent"), &recorded).is_ok());
    }

    #[test]
    fn enoent_and_dir_state() {
        let tmp = std::env::temp_dir().join(format!("razel-stale2-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("adir")).unwrap();
        std::fs::write(tmp.join("present"), "x").unwrap();
        // ENOENT: a still-absent path is fresh; a now-present one is stale.
        assert!(check_stale(&tmp, &[file("@@//gone", FileValue::Enoent)]).is_ok());
        assert!(check_stale(&tmp, &[file("@@//present", FileValue::Enoent)]).unwrap_err().contains("stale"));
        // DIR: a still-dir is fresh.
        assert!(check_stale(&tmp, &[file("@@//adir", FileValue::Dir)]).is_ok());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
