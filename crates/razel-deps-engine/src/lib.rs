//! `razel-deps-engine` — the V2 dependency-engine seam (design: `dev-docs/RazelDepsEngineV2.md`).
//!
//! Slice 1 is the **boundary, not the scheduler**: one command ingress, one typed event egress,
//! and immutable [`SnapshotId`](api::SnapshotId)s for every readable result. The legacy
//! synchronous loader runs behind the same message API ([`LegacyDepsEngine`]) so the future graph
//! engine can be swapped in (and compared) without rewriting callers.
//!
//! What is deliberately NOT here yet: the keyed scheduler, `NeedMany`/resume node messages, the
//! persistent `DepsetStore`, and the `graph`/`compare` engines. Those land behind this seam.

pub mod api;
pub mod facts;
pub mod legacy;

pub use facts::{decode_target, encode_target, snapshot_fingerprint, target_fingerprint};

pub use api::{
    ApiVersion, CommandFinished, CommandId, CommandOutcome, CommandToken, DiagnosticCode,
    DiagnosticEvent, EngineCommand, EngineEvent, EngineOptions, EngineSendError, EvalRoot,
    EvaluateRequest, EventHeader, EventProfile, EventSubscription, OptionsDigest, RazelDepsEngine,
    SchedulingOptions, SnapshotCommitted, SnapshotId, WorkspaceEpoch, API_VERSION,
};
pub use legacy::{legacy_sched_point, LegacyDepsEngine};
