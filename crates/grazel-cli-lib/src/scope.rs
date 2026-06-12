//! Service-scope resolution (PublicSurfaces §1e).
//!
//! A workspace names a SCOPE, not a daemon. Override chain, highest wins:
//! `--scope` flag → `GRAZEL_SCOPE` env → `.grazelrc` `service_scope=` → `default`.
//! This module parses ONLY the `service_scope` key out of `.grazelrc`; the full
//! rc layer (command prefixes, precedence ladder, razelrc-key policing) is GR2.

use std::path::Path;

pub const DEFAULT_SCOPE: &str = "default";

/// Scope names become socket FILENAMES under `<home>/.uds/` — kept short and flat
/// because macOS caps the whole UDS path at 104 bytes (§1e).
pub fn validate_scope_name(name: &str) -> Result<(), String> {
    let ok_head = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let ok_rest = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if !ok_head || !ok_rest || name.len() > 32 {
        return Err(format!(
            "invalid scope name {name:?}: want [a-z0-9][a-z0-9_-]*, max 32 chars"
        ));
    }
    Ok(())
}

/// The `service_scope=` line of `<workspace>/.grazelrc`, if any. Absent file or
/// absent key are both `None`; a malformed value is a loud error, not a default.
pub fn read_rc_scope(workspace: &Path) -> Result<Option<String>, String> {
    let rc = workspace.join(".grazelrc");
    let Ok(text) = std::fs::read_to_string(&rc) else {
        return Ok(None);
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(v) = line.strip_prefix("service_scope=") {
            let v = v.trim();
            validate_scope_name(v).map_err(|e| format!("{}: {e}", rc.display()))?;
            return Ok(Some(v.to_string()));
        }
    }
    Ok(None)
}

/// §1e config arrow: razel must never become grazel-aware, so a grazel key in
/// `.razelrc` is an ERROR pointing at the right file — not a silently-read value.
fn check_razelrc(workspace: &Path) -> Result<(), String> {
    let rc = workspace.join(".razelrc");
    let Ok(text) = std::fs::read_to_string(&rc) else {
        return Ok(());
    };
    for line in text.lines().map(str::trim) {
        if line.starts_with("service_scope=") {
            return Err(format!(
                "{}: grazel key `service_scope` in .razelrc — grazel config belongs in .grazelrc (§1e)",
                rc.display()
            ));
        }
    }
    Ok(())
}

/// Resolve the scope per the §1e chain. `flag`/`env` are the already-extracted
/// `--scope=` and `GRAZEL_SCOPE` values; `workspace` is where `.grazelrc` lives.
pub fn resolve(
    flag: Option<&str>,
    env: Option<&str>,
    workspace: &Path,
) -> Result<String, String> {
    check_razelrc(workspace)?;
    let chosen = match (flag, env) {
        (Some(f), _) => f.to_string(),
        (None, Some(e)) => e.to_string(),
        (None, None) => match read_rc_scope(workspace)? {
            Some(rc) => rc,
            None => DEFAULT_SCOPE.to_string(),
        },
    };
    validate_scope_name(&chosen)?;
    Ok(chosen)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_order_flag_env_rc_default() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        assert_eq!(resolve(None, None, ws).unwrap(), "default");
        std::fs::write(ws.join(".grazelrc"), "# c\nservice_scope=rcscope\n").unwrap();
        assert_eq!(resolve(None, None, ws).unwrap(), "rcscope");
        assert_eq!(resolve(None, Some("envscope"), ws).unwrap(), "envscope");
        assert_eq!(resolve(Some("flagscope"), Some("envscope"), ws).unwrap(), "flagscope");
    }

    #[test]
    fn bad_names_fail_loud() {
        for bad in ["", "UPPER", "has space", "a/b", "-lead", &"x".repeat(33)] {
            assert!(validate_scope_name(bad).is_err(), "{bad:?} accepted");
        }
        validate_scope_name("customer-a_2").unwrap();
    }

    #[test]
    fn malformed_rc_value_is_an_error_not_a_default() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".grazelrc"), "service_scope=Not/Valid\n").unwrap();
        assert!(resolve(None, None, tmp.path()).is_err());
    }
}
