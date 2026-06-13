//! The message API (`RazelDepsEngineV2.md` §3, §5, §6). Pure data + one trait — no Starlark, no
//! scheduler. The minimal surface: submit a command, subscribe to typed events, read results by
//! committed [`SnapshotId`].

use razel_core::Digest;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// API schema version (§4.1). In-process this is a compile-time match; a future wire edge encodes
/// it explicitly. Bump when the command/event shape changes incompatibly.
pub const API_VERSION: ApiVersion = ApiVersion(1);

/// §4.1 — the command/event schema version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ApiVersion(pub u32);

/// Correlates a submitted command with the events it produces (§6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CommandId(pub u64);

/// Monotonic per-workspace command/invalidation order (§4.2). Orders writes against reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkspaceEpoch(pub u64);

/// Addresses an immutable, readable result view (§4.2, REQ-DEPSV2-005). Every committed result
/// has exactly one; queries name a snapshot, never a live `Session`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SnapshotId(pub u64);

/// Digest over the **semantic** options that affect results (§4.4, §13). Scheduling-only and
/// observability options MUST NOT change it (REQ-DEPSV2-023) — that is what lets a thread-count
/// change reuse cached semantic values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OptionsDigest(pub u64);

/// Returned by [`RazelDepsEngine::submit`] — names the command whose events will follow and the
/// epoch it was accepted at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandToken {
    pub command_id: CommandId,
    pub epoch: WorkspaceEpoch,
}

/// The command channel is gone (engine dropped/closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineSendError {
    Closed,
}

// ---- commands (§5) --------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum EngineCommand {
    Evaluate(EvaluateRequest),
}

#[derive(Debug, Clone)]
pub struct EvaluateRequest {
    pub command_id: CommandId,
    pub root: EvalRoot,
    pub options: EngineOptions,
}

/// Slice 1 supports the single-`BUILD`-source root (the `analyze_bazel_with` path). The
/// workspace / label / pattern roots arrive with the package graph keys (migration §5 steps 5–6).
#[derive(Debug, Clone)]
pub enum EvalRoot {
    BuildSource { name: String, src: String },
}

/// Options split by EFFECT (§13): `semantic` participates in the [`OptionsDigest`]; `scheduling`
/// (thread count, event verbosity) does not (REQ-DEPSV2-023). `semantic.sched_hook` is ignored —
/// the engine installs its own hook to translate loader events into typed [`EngineEvent`]s.
#[derive(Debug, Clone, Default)]
pub struct EngineOptions {
    pub semantic: razel_loading::GlobalFlags,
    pub scheduling: SchedulingOptions,
}

#[derive(Debug, Clone, Default)]
pub struct SchedulingOptions {
    /// Loading/analysis worker count (`--loading_phase_threads`). `None` ⇒ engine default.
    pub threads: Option<usize>,
    pub event_profile: EventProfile,
}

/// Event verbosity (§18). Slice 1 distinguishes the user stream from the debug-graph stream; the
/// backpressure/drop policy lands with the high-volume graph events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EventProfile {
    #[default]
    Normal,
    DebugGraph,
}

impl EngineOptions {
    /// REQ-DEPSV2-023 — digest the SEMANTIC options only. `scheduling` (threads, event profile)
    /// and `semantic.sched_hook` (observability) are deliberately excluded, so changing them
    /// reuses semantic results. In-memory digest (not a persistent-cache ABI digest — §4.4).
    pub fn options_digest(&self) -> OptionsDigest {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let g = &self.semantic;
        g.copts.hash(&mut h);
        g.linkopts.hash(&mut h);
        // cc_toolchain / external bases are config inputs (semantic); hash via stable Debug so we
        // do not require Hash impls to leak out of razel-loading.
        format!("{:?}", g.cc_toolchain).hash(&mut h);
        g.external_base.hash(&mut h);
        g.fetched_external_base.hash(&mut h);
        g.compilation_mode.hash(&mut h);
        g.defines.hash(&mut h);
        g.strict_bazel.hash(&mut h);
        OptionsDigest(h.finish())
    }
}

// ---- events (§6) ----------------------------------------------------------------

/// Carries enough identity to correlate an event without parsing display strings
/// (REQ-DEPSV2-015).
#[derive(Debug, Clone)]
pub struct EventHeader {
    pub api_version: ApiVersion,
    pub command_id: Option<CommandId>,
    pub epoch: WorkspaceEpoch,
    pub snapshot: Option<SnapshotId>,
    pub sequence: u64,
}

#[derive(Debug, Clone)]
pub enum EngineEvent {
    CommandAccepted(EventHeader),
    Diagnostic(DiagnosticEvent),
    SnapshotCommitted(SnapshotCommitted),
    CommandFinished(CommandFinished),
}

impl EngineEvent {
    pub fn header(&self) -> &EventHeader {
        match self {
            EngineEvent::CommandAccepted(h) => h,
            EngineEvent::Diagnostic(d) => &d.header,
            EngineEvent::SnapshotCommitted(s) => &s.header,
            EngineEvent::CommandFinished(c) => &c.header,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiagnosticEvent {
    pub header: EventHeader,
    pub code: DiagnosticCode,
    /// The legacy key (label / resource name) the loader reported, if any.
    pub subject: Option<String>,
    pub message: String,
}

/// Typed replacement for the stringly `SchedHook` point (§6, REQ-DEPSV2-004). Slice 1 carries the
/// legacy point string verbatim in [`DiagnosticCode::Sched`] so the adapter is lossless; richer
/// typed codes (and the `DebugGraphEvent` family) arrive with the graph engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticCode {
    Sched(String),
}

#[derive(Debug, Clone)]
pub struct SnapshotCommitted {
    pub header: EventHeader,
    pub snapshot: SnapshotId,
    pub options_digest: OptionsDigest,
    /// Content address of the committed facts — blake3 over their taut serialization
    /// (`facts::snapshot_fingerprint`). The `SnapshotId` is the in-run handle; this `Digest` is
    /// the cross-run / cross-worker cache key (REQ-DEPSV2-012/013).
    pub content: Digest,
    /// True when this snapshot was served from the content-addressed cache (taut bytes decoded)
    /// rather than freshly analyzed — the early-cutoff / incremental win (REQ-DEPSV2-027).
    pub from_cache: bool,
}

#[derive(Debug, Clone)]
pub struct CommandFinished {
    pub header: EventHeader,
    pub outcome: CommandOutcome,
}

#[derive(Debug, Clone)]
pub enum CommandOutcome {
    Ok { snapshot: SnapshotId },
    Err { message: String },
}

// ---- the seam -------------------------------------------------------------------

/// REQ-DEPSV2-001 — the engine is message-driven at its public seam: submit a command, subscribe
/// to a typed event stream. Synchronous facades (`analyze_bazel_with`-style) are convenience over
/// this. The legacy and (future) graph engines both implement it (REQ-DEPSV2-002).
pub trait RazelDepsEngine {
    fn submit(&self, command: EngineCommand) -> Result<CommandToken, EngineSendError>;
    fn subscribe(&self) -> EventSubscription;
}

/// A typed event reader. Slice 1 buffers events into a shared queue (subscribe-then-submit);
/// backpressure/drop policy (§18) and `EventFilter` land with the high-volume graph stream.
#[derive(Clone)]
pub struct EventSubscription {
    pub(crate) sink: Arc<Mutex<VecDeque<EngineEvent>>>,
}

impl EventSubscription {
    /// Take all events buffered so far, in emission order.
    pub fn drain(&self) -> Vec<EngineEvent> {
        self.sink.lock().expect("event sink").drain(..).collect()
    }
}
