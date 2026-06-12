//! The §1e scope filesystem contract — grazel's OWN namespace.
//!
//! Sockets: `<home>/.uds/<scope>` (flat + short: macOS caps a UDS path at 104
//! bytes). Per-scope state: `<home>/scopes/<scope>/` (daemon.json, later the iroh
//! key + scope config). `<home>` is `$GRAZEL_HOME` when set (tests, fixtures),
//! else `~/.grazel`. Razel's daemon rendezvous lives at `_razel_<user>/daemon/`
//! (§1e) — no shared prefix with this layout, so the two CLIs cannot collide on a
//! socket by construction.

use crate::scope::validate_scope_name;
use std::path::{Path, PathBuf};

/// macOS `sun_path` is 104 bytes including the NUL.
const UDS_PATH_MAX: usize = 103;

#[derive(Debug, Clone)]
pub struct ScopePaths {
    pub scope: String,
    pub home: PathBuf,
    pub socket: PathBuf,
    pub state_dir: PathBuf,
    pub daemon_json: PathBuf,
}

/// `$GRAZEL_HOME` override, else `<user home>/.grazel`.
pub fn grazel_home(env_override: Option<&str>, user_home: Option<&str>) -> Result<PathBuf, String> {
    if let Some(h) = env_override {
        return Ok(PathBuf::from(h));
    }
    user_home
        .map(|h| Path::new(h).join(".grazel"))
        .ok_or_else(|| "cannot locate grazel home: neither GRAZEL_HOME nor HOME set".into())
}

impl ScopePaths {
    pub fn new(home: &Path, scope: &str) -> Result<Self, String> {
        validate_scope_name(scope)?;
        let socket = home.join(".uds").join(scope);
        let len = socket.as_os_str().len();
        if len > UDS_PATH_MAX {
            return Err(format!(
                "socket path {} is {len} bytes; the UDS limit is {UDS_PATH_MAX} (macOS 104-byte cap) — use a shorter GRAZEL_HOME or scope name",
                socket.display()
            ));
        }
        let state_dir = home.join("scopes").join(scope);
        Ok(Self {
            scope: scope.to_string(),
            home: home.to_path_buf(),
            daemon_json: state_dir.join("daemon.json"),
            socket,
            state_dir,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_uds_and_scopes_under_home() {
        let p = ScopePaths::new(Path::new("/tmp/gh"), "alpha").unwrap();
        assert_eq!(p.socket, Path::new("/tmp/gh/.uds/alpha"));
        assert_eq!(p.state_dir, Path::new("/tmp/gh/scopes/alpha"));
        assert_eq!(p.daemon_json, Path::new("/tmp/gh/scopes/alpha/daemon.json"));
    }

    #[test]
    fn distinct_scopes_distinct_sockets() {
        let a = ScopePaths::new(Path::new("/tmp/gh"), "a").unwrap();
        let b = ScopePaths::new(Path::new("/tmp/gh"), "b").unwrap();
        assert_ne!(a.socket, b.socket);
    }

    #[test]
    fn overlong_socket_path_fails_loud() {
        let deep = format!("/tmp/{}", "d/".repeat(60));
        let err = ScopePaths::new(Path::new(&deep), "a").unwrap_err();
        assert!(err.contains("104"), "{err}");
    }

    #[test]
    fn grazel_home_override_chain() {
        assert_eq!(grazel_home(Some("/x"), Some("/h")).unwrap(), Path::new("/x"));
        assert_eq!(grazel_home(None, Some("/h")).unwrap(), Path::new("/h/.grazel"));
        assert!(grazel_home(None, None).is_err());
    }
}
