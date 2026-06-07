//! Daemon + file-watch (Phase 7) — the Tier-2.5 "instant rebuild" deliverable.
//!
//! A [`Workspace`] holds the **warm** engine graph in RAM across requests; a file change
//! is reconciled by re-supplying that input's digest (`sync_file`), after which a rebuild
//! recomputes only the affected subgraph. The key correctness property is **warm == cold**:
//! the warm graph, after a sequence of edits, must produce exactly the result a fresh
//! (cold) graph built from the final state would. A no-op rebuild does **zero** work.
//!
//! SCOPE: this is the in-process core + the IPC protocol + a `notify` watcher. The
//! long-lived daemon process, the UDS+bincode server loop, and FSEvents
//! coalescing/atomic-rename reconciliation (the watcher *torture* test) are the OS/process
//! integration layer on top — they transport these operations, they don't change them.

use razel_core::Digest;
use razel_engine::Engine;
use std::path::{Path, PathBuf};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// IPC request (transported over UDS+bincode by the daemon server — wrapper TODO).
#[derive(Debug, Clone)]
pub enum Request {
    Build { target: String },
    SyncFile { path: String, digest: Digest },
    Version,
}

/// IPC response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Built {
        value_hex: String,
        recomputes: usize,
    },
    Synced,
    Version(String),
    Error(String),
}

/// A warm workspace: the persistent engine graph + the operations a client drives.
#[derive(Default)]
pub struct Workspace {
    engine: Engine,
}

impl Workspace {
    pub fn new() -> Self {
        Self::default()
    }

    /// Access the engine to declare the build graph (inputs = files, derived = targets).
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Reconcile a file change: re-supply its content digest (the watcher's entry point).
    pub fn sync_file(&self, path: &str, digest: Digest) {
        self.engine.set_input(path, digest);
    }

    /// Build a target against the warm graph (recomputes only the affected subgraph).
    pub fn build(&self, target: &str) -> Result<Digest, String> {
        self.engine.request(target)
    }

    /// Handle one IPC request.
    pub fn handle(&self, req: Request) -> Response {
        match req {
            Request::Build { target } => match self.build(&target) {
                Ok(v) => Response::Built {
                    value_hex: v.to_hex(),
                    recomputes: self.engine.recomputes(),
                },
                Err(e) => Response::Error(e),
            },
            Request::SyncFile { path, digest } => {
                self.sync_file(&path, digest);
                Response::Synced
            }
            Request::Version => Response::Version(VERSION.to_string()),
        }
    }
}

/// Start watching `dir`; `on_change` fires (with the changed path) on filesystem events.
/// This is the daemon's watch loop bridge to `sync_file`. (FSEvents coalescing / atomic-
/// rename reconciliation — the torture test — is the OS-specific hardening layer.)
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

    fn d(s: &str) -> Digest {
        Digest::of(s.as_bytes())
    }
    fn concat(parts: &[Digest]) -> Digest {
        Digest::of(
            parts
                .iter()
                .map(|p| p.to_hex())
                .collect::<String>()
                .as_bytes(),
        )
    }

    // file a -> lib; (lib, file b) -> bin.
    fn setup(w: &Workspace) {
        w.engine().add_input("f/a", d("a0"));
        w.engine().add_input("f/b", d("b0"));
        w.engine().add_derived("lib", &["f/a"], concat);
        w.engine().add_derived("bin", &["lib", "f/b"], concat);
    }

    #[test]
    fn warm_graph_equals_cold_graph_after_edits() {
        // Warm: build, then a sequence of file syncs, then rebuild.
        let warm = Workspace::new();
        setup(&warm);
        warm.build("bin").unwrap();
        warm.sync_file("f/a", d("a1"));
        warm.sync_file("f/b", d("b1"));
        let warm_result = warm.build("bin").unwrap();

        // Cold: a fresh graph built straight from the final state.
        let cold = Workspace::new();
        setup(&cold);
        cold.sync_file("f/a", d("a1"));
        cold.sync_file("f/b", d("b1"));
        let cold_result = cold.build("bin").unwrap();

        assert_eq!(warm_result, cold_result, "warm graph must equal cold graph");
    }

    #[test]
    fn no_op_rebuild_does_zero_work() {
        let w = Workspace::new();
        setup(&w);
        w.build("bin").unwrap();
        w.engine().reset_recomputes();
        w.build("bin").unwrap(); // nothing changed
        assert_eq!(w.engine().recomputes(), 0, "warm no-op rebuild = zero work");
    }

    #[test]
    fn editing_one_file_recomputes_only_its_rdeps() {
        let w = Workspace::new();
        setup(&w);
        w.build("bin").unwrap();
        w.engine().reset_recomputes();
        w.sync_file("f/b", d("b1")); // only bin depends on b
        w.build("bin").unwrap();
        assert_eq!(w.engine().recomputes(), 1, "only bin recomputes");
    }

    #[test]
    fn ipc_protocol_roundtrips() {
        let w = Workspace::new();
        setup(&w);
        assert!(matches!(
            w.handle(Request::Build {
                target: "bin".into()
            }),
            Response::Built { .. }
        ));
        assert_eq!(
            w.handle(Request::SyncFile {
                path: "f/a".into(),
                digest: d("a2")
            }),
            Response::Synced
        );
        assert_eq!(
            w.handle(Request::Version),
            Response::Version(VERSION.to_string())
        );
        assert!(matches!(
            w.handle(Request::Build {
                target: "nope".into()
            }),
            Response::Error(_)
        ));
    }

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
