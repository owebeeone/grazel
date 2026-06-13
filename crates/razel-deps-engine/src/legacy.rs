//! [`LegacyDepsEngine`] — the existing synchronous loader behind the V2 message API
//! (REQ-DEPSV2-024: the FIRST milestone is a legacy adapter, before any new graph behavior).
//!
//! It is intentionally behavior-preserving: `Evaluate(BuildSource)` calls
//! [`razel_loading::analyze_bazel_with`], commits its `Vec<AnalyzedTarget>` as an immutable
//! snapshot, and emits `CommandAccepted → [Diagnostic…] → SnapshotCommitted → CommandFinished`.
//! The loader's stringly `SchedHook` is translated into typed [`DiagnosticEvent`]s on the way out,
//! and [`legacy_sched_point`] converts them back for the old point/key tests (the lossless,
//! intentionally one-way-degradable adapter of §6).

use crate::api::*;
use razel_core::Digest;
use razel_loading::{AnalyzedTarget, GlobalFlags, SchedHook};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

type EventSink = Arc<Mutex<VecDeque<EngineEvent>>>;

#[derive(Default)]
struct EngineState {
    epoch: u64,
    next_snapshot: u64,
    subscribers: Vec<EventSink>,
    snapshots: HashMap<u64, Arc<Vec<AnalyzedTarget>>>,
    /// Content-addressed cache: input `Digest` → the committed snapshot's taut bytes. A hit serves
    /// the result by decoding (no Starlark, no re-analysis) — the in-process embryo of the
    /// persistent cross-invocation cache.
    cache: HashMap<Digest, Vec<u8>>,
}

/// The legacy loader behind the message API. `Send + Sync` so a daemon edge can share it.
#[derive(Default)]
pub struct LegacyDepsEngine {
    state: Mutex<EngineState>,
}

impl LegacyDepsEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read a committed result by id (REQ-DEPSV2-005: results are addressed by `SnapshotId`,
    /// never by reading a live `Session`). `None` if the id is unknown.
    pub fn snapshot(&self, id: SnapshotId) -> Option<Arc<Vec<AnalyzedTarget>>> {
        self.state.lock().expect("engine state").snapshots.get(&id.0).cloned()
    }

    /// Commit a fact set as an immutable, content-addressed snapshot; return its in-run handle and
    /// content `Digest`.
    fn commit(&self, targets: Vec<AnalyzedTarget>) -> (SnapshotId, Digest) {
        let content = crate::facts::snapshot_fingerprint(&targets);
        let mut st = self.state.lock().expect("engine state");
        let id = st.next_snapshot;
        st.next_snapshot += 1;
        st.snapshots.insert(id, Arc::new(targets));
        (SnapshotId(id), content)
    }

    fn evaluate(&self, req: EvaluateRequest) -> Result<CommandToken, EngineSendError> {
        let command_id = req.command_id;
        // Take the epoch and a snapshot of current subscribers, then release the lock so the
        // (possibly slow) analysis and the in-hook emits never hold it.
        let (epoch, sinks) = {
            let mut st = self.state.lock().expect("engine state");
            st.epoch += 1;
            (WorkspaceEpoch(st.epoch), st.subscribers.clone())
        };
        let seq = Arc::new(AtomicU64::new(0));
        let options_digest = req.options.options_digest();

        emit(
            &sinks,
            EngineEvent::CommandAccepted(header(&seq, command_id, epoch, None)),
        );

        let (root_name, src) = match &req.root {
            EvalRoot::BuildSource { name, src } => (name.clone(), src.clone()),
        };
        let _ = root_name; // reserved: multi-root snapshots name their root (migration §5)

        // Cache key = the inputs that determine the result: the source + the SEMANTIC options
        // digest (scheduling/observability options excluded — REQ-DEPSV2-023, so a thread-count
        // change still hits the cache).
        let cache_key = {
            let mut input = src.clone().into_bytes();
            input.extend_from_slice(&options_digest.0.to_le_bytes());
            Digest::of(&input)
        };

        let emit_ok = |snapshot: SnapshotId, content: Digest, from_cache: bool| {
            emit(
                &sinks,
                EngineEvent::SnapshotCommitted(SnapshotCommitted {
                    header: header(&seq, command_id, epoch, Some(snapshot)),
                    snapshot,
                    options_digest,
                    content,
                    from_cache,
                }),
            );
            emit(
                &sinks,
                EngineEvent::CommandFinished(CommandFinished {
                    header: header(&seq, command_id, epoch, Some(snapshot)),
                    outcome: CommandOutcome::Ok { snapshot },
                }),
            );
        };

        // Cache HIT: decode the stored taut bytes — no Starlark, no re-analysis, no SchedHook.
        let cached = self.state.lock().expect("engine state").cache.get(&cache_key).cloned();
        if let Some(bytes) = cached {
            if let Ok(targets) = crate::facts::decode_snapshot(&bytes) {
                let (snapshot, content) = self.commit(targets);
                emit_ok(snapshot, content, true);
                return Ok(CommandToken { command_id, epoch });
            }
            // A corrupt entry falls through to a fresh analysis.
        }

        // MISS: analyze with the translating SchedHook (every loader (point, key) → typed
        // Diagnostic; the closure is Send + Sync, capturing only Arc/Copy state), then cache the
        // serialized snapshot.
        let mut flags: GlobalFlags = req.options.semantic.clone();
        {
            let sinks = sinks.clone();
            let seq = Arc::clone(&seq);
            flags.sched_hook = Some(SchedHook(Arc::new(move |point: &str, key: &str| {
                emit(
                    &sinks,
                    EngineEvent::Diagnostic(DiagnosticEvent {
                        header: header(&seq, command_id, epoch, None),
                        code: DiagnosticCode::Sched(point.to_string()),
                        subject: Some(key.to_string()),
                        message: String::new(),
                    }),
                );
            })));
        }

        match razel_loading::analyze_bazel_with(&src, flags) {
            Ok(targets) => {
                self.state
                    .lock()
                    .expect("engine state")
                    .cache
                    .insert(cache_key, crate::facts::encode_snapshot(&targets));
                let (snapshot, content) = self.commit(targets);
                emit_ok(snapshot, content, false);
            }
            Err(message) => {
                emit(
                    &sinks,
                    EngineEvent::CommandFinished(CommandFinished {
                        header: header(&seq, command_id, epoch, None),
                        outcome: CommandOutcome::Err { message },
                    }),
                );
            }
        }

        Ok(CommandToken { command_id, epoch })
    }
}

impl RazelDepsEngine for LegacyDepsEngine {
    fn submit(&self, command: EngineCommand) -> Result<CommandToken, EngineSendError> {
        match command {
            EngineCommand::Evaluate(req) => self.evaluate(req),
        }
    }

    fn subscribe(&self) -> EventSubscription {
        let sink: EventSink = Arc::new(Mutex::new(VecDeque::new()));
        self.state.lock().expect("engine state").subscribers.push(Arc::clone(&sink));
        EventSubscription { sink }
    }
}

/// The legacy `SchedHook` adapter, in reverse (§6): recover the old `(point, key)` pair from a
/// typed event. `None` for events that have no legacy point. Intentionally lossy — V2 tests should
/// assert typed fields; this exists only to keep the old point/key tests alive during migration.
pub fn legacy_sched_point(event: &EngineEvent) -> Option<(String, String)> {
    match event {
        EngineEvent::Diagnostic(d) => match &d.code {
            DiagnosticCode::Sched(point) => {
                Some((point.clone(), d.subject.clone().unwrap_or_default()))
            }
        },
        _ => None,
    }
}

fn header(
    seq: &AtomicU64,
    command_id: CommandId,
    epoch: WorkspaceEpoch,
    snapshot: Option<SnapshotId>,
) -> EventHeader {
    EventHeader {
        api_version: API_VERSION,
        command_id: Some(command_id),
        epoch,
        snapshot,
        // Gap-free per command, starting at 1 (mirrors the daemon invocation-log convention).
        sequence: seq.fetch_add(1, Ordering::SeqCst) + 1,
    }
}

fn emit(sinks: &[EventSink], event: EngineEvent) {
    for sink in sinks {
        sink.lock().expect("event sink").push_back(event.clone());
    }
}
