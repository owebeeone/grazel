//! Daemon — file watch + the warm-build server (Phase 7 / WS-D).
//!
//! The live request plane is [`rpc::Server`] (UDS/CBOR). The WARM incremental build runs in a
//! single-writer, per-workspace actor that owns the engine graph
//! (FixMissingServerImplementationPlan §3.5a; WS-D in progress). This module holds the daemon's
//! cross-cutting pieces: the `notify` file watcher (the actor's invalidation source) and the
//! version constant.
//!
//! The former toy `Workspace` over a raw `razel_engine::Engine` — a Phase-7 proving ground for
//! warm==cold / no-op==zero-work / incremental-firewall — has been removed (it was wired to
//! nothing real and broke under the C1 engine API change). Those properties are now proven where
//! the REAL build graph lives: `razel-engine` (the incremental core + cancel-restart) and
//! `razel-build`'s `IncrementalBuilder` (the action graph). The real warm path is the actor in
//! `rpc.rs` (WS-D).

pub mod actor; // §3.5a the warm single-owner build actor (WS-D)
pub mod outlock; // §1b cross-daemon workspace writer lock (contract: ws-razel/inbox/0005)
pub mod rpc;
pub mod transport;

use std::path::{Path, PathBuf};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Start watching `dir`; `on_change` fires (with the changed path) on filesystem events. This is
/// the daemon's watch loop bridge to the actor's invalidation (`set_input` for a known leaf /
/// `Rescan` for a graph-shape change, §3.5a). FSEvents coalescing / atomic-rename reconciliation
/// is the OS-specific hardening layer on top.
pub fn watch<F>(dir: &Path, mut on_change: F) -> notify::Result<notify::RecommendedWatcher>
where
    F: FnMut(PathBuf) + Send + 'static,
{
    use notify::{Event, RecursiveMode, Watcher};
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(ev) = res {
            for p in ev.paths {
                on_change(p);
            }
        }
    })?;
    watcher.watch(dir, RecursiveMode::Recursive)?;
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_watcher_delivers_fs_events() {
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let _watcher = watch(dir.path(), move |p| {
            let _ = tx.send(p);
        })
        .unwrap();

        // Write a file; expect at least one event within a tolerant window.
        std::fs::write(dir.path().join("hello.txt"), "hi").unwrap();
        let got = rx.recv_timeout(Duration::from_secs(5));
        assert!(got.is_ok(), "watcher delivered no event within 5s");
    }
}
