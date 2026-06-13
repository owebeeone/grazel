# RazelDepsEngineV2 - message-driven dependency engine API

*2026-06-13. Design draft. Owner: RR. Scope: the replacement load/dependency
engine interface and migration seam, not the full implementation.*

This document answers the immediate API question before the redesign goes
deeper:

**The minimal API is one command ingress, one typed event egress, and immutable
snapshot ids for all readable results.** Everything else is convenience.

**Versioning is an issue.** It is not optional if the API is message-driven,
because commands, events, cached node values, snapshots, and provider/depset
schemas all need a compatibility boundary. V2 SHOULD make versions opaque and
cheap, but it MUST include them from the first seam so the old and new engines
can run side by side without semantic ambiguity.

## 1. Current shape

The live public loader surface today is synchronous and return-value shaped:

- `analyze_bazel(_with)` evaluates one BUILD source and returns
  `Vec<AnalyzedTarget>`.
- `analyze_workspace(_with)` evaluates one top label in a workspace and returns
  `Vec<AnalyzedTarget>`.
- `load_tree_report*` evaluates many packages and returns a package report plus,
  in the seeded variants, the package list actually loaded.
- `prepare_build_asts` performs the pure parse split used by `tfload`.
- `GlobalFlags` is the main options bag.
- `SchedHook(point, key)` is the current observation seam. It is stringly typed
  and mixes coordination events with diagnostics.

That surface is useful for tests and for the CLI's blocking calls, but it is not
the right core API for a graph engine. It hides concurrency in call stacks,
forces callers to wait for final results, and makes observability a hook bolted
onto the side instead of a typed stream.

The current implementation also has requirements that V2 must preserve:

- A load/eval run owns a `Session`; no process-global mutable analysis state.
- Starlark evaluation for one module/package remains single-threaded inside one
  Starlark heap.
- Live Starlark `Value`s MUST NOT cross package/worker boundaries. Completed
  module data crosses only as frozen/owned values or plain data.
- `.bzl` module identity matters: one provider object definition MUST produce
  one provider identity per engine run.
- `--strict_bazel`, external repo roots, fetched roots, host config, and
  toolchain mode are part of the evaluation input.
- The daemon/public surface is already actor-shaped: commands enter a workspace
  queue; events and committed snapshots leave it.

## 2. Design goal

REQ-DEPSV2-001: The dependency engine MUST be message-driven at its public
engine seam.

REQ-DEPSV2-002: The old engine and the new engine MUST both implement the same
message API, so the CLI/test harness can run either engine or compare both.

REQ-DEPSV2-003: The core scheduler MUST see dependency edges as data, not as
hidden recursive Rust calls.

REQ-DEPSV2-004: The engine MUST support typed events as a first-class output.
String hooks MAY remain as adapters for existing tests, but they MUST NOT be the
primary V2 event representation.

REQ-DEPSV2-005: Every externally readable result MUST be addressed through a
committed `SnapshotId`, never by reading a live `Session`.

REQ-DEPSV2-006: Query values MUST be immutable once published. Mutation happens
only through node computation and commit messages.

REQ-DEPSV2-007: Depsets MUST be persistent DAG values. Construction MUST NOT
flatten transitive depsets.

REQ-DEPSV2-008: The engine MUST expose enough structured diagnostics to explain
parallelism, wait edges, cache hits, recomputes, depset flattening, and critical
path bottlenecks.

REQ-DEPSV2-009: The V2 interface MUST remain runtime-agnostic inside
`razel-loading`/engine crates. A daemon edge MAY use async IO, but the engine
core MUST be drivable by plain threads and channels.

## 3. Minimal API

The minimal API is intentionally small:

```rust
pub trait RazelDepsEngine {
    fn submit(&self, command: EngineCommand) -> Result<CommandToken, EngineSendError>;
    fn subscribe(&self, filter: EventFilter) -> Result<EventSubscription, EngineSendError>;
}
```

Everything synchronous is a facade over that:

```rust
pub fn analyze_bazel_with(src: &str, flags: GlobalFlags) -> Result<Vec<AnalyzedTarget>, String> {
    // Build EngineCommand::Evaluate, collect events until CommandFinished,
    // read the committed snapshot, and convert it to the legacy return value.
}
```

The minimal command set:

```rust
pub enum EngineCommand {
    OpenWorkspace(OpenWorkspace),
    CloseWorkspace(CloseWorkspace),
    Evaluate(EvaluateRequest),
    Query(QueryRequest),
    Invalidate(InvalidateRequest),
    Cancel(CancelRequest),
}
```

The minimal event set:

```rust
pub enum EngineEvent {
    CommandAccepted(CommandAccepted),
    CommandRejected(CommandRejected),
    Diagnostic(DiagnosticEvent),
    Progress(ProgressEvent),
    SnapshotCommitted(SnapshotCommitted),
    QueryResult(QueryResultEvent),
    CommandFinished(CommandFinished),
}
```

For implementation and tests, the engine SHOULD also expose debug graph events:

```rust
pub enum DebugGraphEvent {
    NodeScheduled { key: QueryKey, reason: ScheduleReason },
    NodeStarted { key: QueryKey, attempt: AttemptId, worker: WorkerId },
    DependencyGroupDeclared { parent: QueryKey, group: DepGroupId, deps: Vec<QueryKey> },
    NodeBlocked { key: QueryKey, group: DepGroupId },
    NodeUnblocked { key: QueryKey, by: QueryKey },
    NodeFinished { key: QueryKey, outcome: NodeOutcome, changed: bool },
    WaitEdgeAdded { waiter: QueryKey, owner: QueryKey },
    WaitCycleDetected { cycle: Vec<QueryKey>, action: CycleAction },
    DepsetConstructed { id: DepsetId, direct: usize, transitive: usize },
    DepsetFlattened { id: DepsetId, unique: usize, cache_hit: bool },
}
```

Stable clients should not need debug graph events. Tests, `tfload`, profilers, and
the parity harness do.

## 4. Versioning

Versioning is required at four levels.

### 4.1 API schema version

REQ-DEPSV2-010: Every command stream MUST begin with or imply an
`ApiVersion`/capability handshake.

For in-process Rust, this can be compile-time type matching. For daemon/wire use,
it must be explicit. A future `razel-wire`/taut representation should encode this
directly; V2 MUST NOT create a JSON shadow protocol.

### 4.2 Workspace epoch and snapshot version

REQ-DEPSV2-011: A workspace actor MUST maintain a monotonic `WorkspaceEpoch`.

REQ-DEPSV2-012: Every committed result MUST produce a `SnapshotId`.

`WorkspaceEpoch` orders commands and invalidations. `SnapshotId` addresses a
readable, immutable view. Queries MUST name a snapshot or use an explicit
`LatestCommitted` alias that resolves to one snapshot before execution.

This is the contract that lets the writer run while viewers read stable data.

### 4.3 Input versions

REQ-DEPSV2-013: File, directory, repository, environment, and toolchain inputs
MUST be represented by stable input fingerprints, not by ambient process state.

In V2 this can be:

```rust
pub struct InputVersion {
    pub digest: Digest,
    pub observed: InputObservation,
}
```

`InputObservation` MAY carry mtime/size/device/inode for fast local checking,
but correctness MUST rest on a digest or an equivalent content identity when the
value participates in a cache key.

### 4.4 Engine/cache ABI version

REQ-DEPSV2-014: Memoized node values MUST include an `EngineAbiDigest`.

The digest SHOULD cover:

- the key/value encoding version,
- rule-pack implementation identity,
- provider schema ids,
- depset encoding version,
- option/config key semantics,
- any algorithm choice that changes observable output.

For the initial in-memory V2 engine, this can be a constant plus
`DeclarationSetId`/`OptionsDigest`. For persistent cache, it becomes mandatory.
When unsure, V2 MUST invalidate rather than reuse.

## 5. Command model

Commands are user-facing or harness-facing messages. They do not describe every
internal graph step.

```rust
pub struct EvaluateRequest {
    pub command_id: CommandId,
    pub workspace: WorkspaceId,
    pub roots: Vec<EvalRoot>,
    pub options: EngineOptions,
    pub event_profile: EventProfile,
    pub compare: CompareMode,
}

pub enum EvalRoot {
    Package(PackageKey),
    Label(TargetLabel),
    Pattern(TargetPattern),
    BuildSource { name: String, src: String },
    WorkspaceRepos,
}

pub struct QueryRequest {
    pub command_id: CommandId,
    pub workspace: WorkspaceId,
    pub snapshot: SnapshotSelector,
    pub query: EngineQuery,
}

pub enum EngineQuery {
    Targets(Vec<TargetKey>),
    Package(PackageKey),
    Providers(TargetKey),
    Actions(TargetKey),
    Depset(DepsetId),
    Explain(QueryKey),
    GraphStats,
}
```

`EvaluateRequest` does not return a value directly. It produces events. A
successful evaluation commits a snapshot, then finishes.

## 6. Event model

REQ-DEPSV2-015: Every event MUST carry enough identity for correlation without
inspection of display strings.

```rust
pub struct EventHeader {
    pub event_id: EventId,
    pub api_version: ApiVersion,
    pub workspace: WorkspaceId,
    pub command_id: Option<CommandId>,
    pub epoch: WorkspaceEpoch,
    pub snapshot: Option<SnapshotId>,
    pub sequence: u64,
}
```

Events SHOULD use stable numeric/string codes for machine handling:

```rust
pub struct DiagnosticEvent {
    pub header: EventHeader,
    pub severity: Severity,
    pub code: DiagnosticCode,
    pub subject: Option<QueryKey>,
    pub message: String,
    pub provenance: Vec<ProvenanceFrame>,
}
```

The current `SchedHook(point, key)` can be preserved as:

```rust
impl LegacySchedHookAdapter {
    fn on_event(&self, event: &EngineEvent) {
        if let Some((point, key)) = legacy_sched_point(event) {
            (self.hook.0)(point, key);
        }
    }
}
```

That adapter is intentionally lossy. V2 tests SHOULD assert typed event fields,
not parsed point strings.

## 7. Internal graph messages

The public command/event API is small. The internal scheduler is fully
message-driven.

REQ-DEPSV2-016: A node computation MUST communicate with the scheduler by
emitting `NodeMsg`s. It MUST NOT synchronously recurse into another node's
computation.

```rust
pub enum NodeMsg {
    NeedOne {
        parent: QueryKey,
        dep: QueryKey,
        continuation: ContinuationId,
    },
    NeedMany {
        parent: QueryKey,
        group: DepGroupId,
        deps: Vec<QueryKey>,
        continuation: ContinuationId,
    },
    Produced {
        key: QueryKey,
        attempt: AttemptId,
        value: NodeValue,
        read_set: ReadSetDigest,
    },
    Failed {
        key: QueryKey,
        attempt: AttemptId,
        error: EngineError,
    },
    Emit {
        key: QueryKey,
        event: EngineEvent,
    },
}
```

`ContinuationId` is deliberately an id, not a captured closure in the API model.
The implementation may initially map it to an in-memory state enum. The state it
references MUST NOT contain live Starlark heap values that can be resumed on a
different worker.

The V2 improvement over Bazel/Skyframe should be here: when a computation waits
for deps, it should resume from an explicit state where possible instead of
restarting the whole function. If a Starlark evaluation cannot be resumed safely,
that node MAY use restart semantics, but the restart MUST be local to that node
and visible in diagnostics.

## 8. Query keys

REQ-DEPSV2-017: Query keys MUST be structured values with canonical equality and
hashing. No graph key may be a display string.

Initial key set:

```rust
pub enum QueryKey {
    WorkspaceRepos(WorkspaceId),
    SourceStat(SourceKey),
    DirectoryListing(DirectoryKey),
    Glob(GlobKey),
    BzlModule(BzlModuleKey),
    Package(PackageKey),
    TargetDecl(TargetDeclKey),
    TargetAnalysis(TargetAnalysisKey),
    ProviderProjection(ProviderProjectionKey),
    Depset(DepsetKey),
    Action(ActionKey),
}
```

Key shape notes:

- `BzlModuleKey` MUST be distinct from target `Label`.
- `PackageKey` MUST include canonical repo plus package path and the relevant
  analysis/config instance when applicable.
- `TargetAnalysisKey` MUST include analysis instance/config identity.
- `DepsetKey` MUST include order, direct children, transitive children, and
  schema/element type where relevant.
- `ActionKey` MUST be action-intrinsic: executable identity, argv, env, working
  dir policy, inputs, outputs, exec properties, and platform/toolchain identity.

## 9. Node lifecycle

REQ-DEPSV2-018: Each `QueryKey` MUST be single-flight per workspace epoch.

Lifecycle:

```text
Absent -> Scheduled -> Running -> Waiting -> ReadyToResume -> Running
                                      |             |
                                      v             v
                                    Failed        Done
```

Rules:

- The first requester of an absent key schedules it.
- Concurrent requesters attach as waiters.
- A node that declares missing deps moves to `Waiting`.
- When all deps in a dependency group complete, the scheduler emits a resume.
- `Done` stores an immutable `NodeValue`.
- `Failed` stores a typed error with transience.
- A changed input invalidates affected nodes by epoch/snapshot logic, not by
  mutating an already committed snapshot.

## 10. Dependency groups

REQ-DEPSV2-019: Independent dependency requests MUST be represented as a
dependency group.

This is the key API feature for parallelism. A rule/package/glob computation
that knows it will need `A, B, C` regardless of their individual values MUST emit
one `NeedMany` group instead of three ordered `NeedOne` messages.

Benefits:

- The scheduler sees graph width immediately.
- Workers can compute missing deps while the parent waits.
- Future invalidation can re-check a group in parallel.
- Diagnostics can distinguish true sequential chains from accidental sequential
  discovery.

## 11. Depset DAG API

REQ-DEPSV2-020: Depset construction MUST produce a node id, not a flattened list.

```rust
pub struct DepsetNode {
    pub id: DepsetId,
    pub order: DepsetOrder,
    pub element_type: DepsetElementType,
    pub direct: Arc<[AtomId]>,
    pub transitive: Arc<[DepsetId]>,
    pub fingerprint: Digest,
}
```

Construction:

- Direct items MAY be deduped within the direct segment.
- Transitive depsets MUST be stored as ids.
- Structural interning SHOULD reuse equal nodes.
- Singleton/empty nodes SHOULD use compact representations.

Consumption:

```rust
pub enum DepsetRequest {
    Flatten { id: DepsetId, order: DepsetOrder },
    Fingerprint { id: DepsetId, map_fn: MapFnId },
    Visit { id: DepsetId, visitor: VisitorId },
}
```

Flattening MUST use an ephemeral visited set and SHOULD cache results by
`(DepsetId, order, projection/map_fn)`. Fingerprinting SHOULD memoize transitive
subresults. This is where V2 should beat the current eager materialization and
avoid the O(NxM) shape seen in `tfload`.

## 12. Values

REQ-DEPSV2-021: `NodeValue` MUST be immutable and shareable.

```rust
pub enum NodeValue {
    WorkspaceRepos(Arc<[RepoSpec]>),
    SourceStat(SourceStatValue),
    DirectoryListing(Arc<[DirEntry]>),
    Glob(GlobValue),
    BzlModule(BzlModuleValue),
    Package(PackageValue),
    TargetDecl(TargetDeclValue),
    TargetAnalysis(TargetAnalysisValue),
    ProviderProjection(ProviderProjectionValue),
    Depset(DepsetNode),
    Action(ActionValue),
}
```

Starlark-specific rule:

REQ-DEPSV2-022: `NodeValue` MUST NOT contain live Starlark `Value<'v>` handles.
If Starlark data must cross node boundaries, it MUST be frozen, serialized into
engine-owned data, or represented by a stable engine id.

## 13. Engine options

`GlobalFlags` should become a field inside a broader `EngineOptions`.

```rust
pub struct EngineOptions {
    pub global: GlobalFlags,
    pub threads: ThreadPolicy,
    pub strict_bazel: bool,
    pub event_profile: EventProfile,
    pub cache_policy: CachePolicy,
    pub comparison: CompareMode,
}
```

`GlobalFlags` can remain for compatibility, but V2 SHOULD separate:

- user semantic options that affect keys/results,
- execution policy options that affect scheduling only,
- diagnostic/event options that affect observability only.

REQ-DEPSV2-023: Options that affect output MUST participate in key or
`AnalysisInstanceId` identity. Options that only affect scheduling MUST NOT
invalidate semantic node values.

## 14. Legacy adapter

V2 should land through adapters, not a big switch.

```rust
pub struct LegacyDepsEngine {
    // wraps current Session/rules.rs entry points
}

pub struct GraphDepsEngine {
    // new keyed scheduler
}

pub struct CompareDepsEngine {
    // drives both, compares committed snapshots/events under selected checks
}
```

Legacy behavior:

- `Evaluate(BuildSource)` calls `analyze_bazel_with`.
- `Evaluate(Package list)` calls `load_tree_report_with_threads`.
- Existing `SchedHook` events are converted to typed `DebugGraphEvent`s where
  possible, and typed events are converted back to string points for old tests.
- Legacy final return values are emitted as `SnapshotCommitted` +
  `CommandFinished`.

REQ-DEPSV2-024: The first V2 milestone MUST be a legacy adapter implementing
the message API before the new graph engine replaces behavior.

This gives the daemon/CLI/tests one seam while the engine is still old.

## 15. New graph engine

The graph engine should have these internal components:

- `CommandActor`: serializes workspace commands and owns snapshot commits.
- `Scheduler`: owns runnable queues, waiters, node states, cancellation, and
  event emission.
- `NodeStore`: maps `QueryKey` to lifecycle state and immutable value.
- `InputStore`: fingerprints file/repo/env/toolchain inputs.
- `DepsetStore`: interns depset DAG nodes and caches flatten/fingerprint results.
- `DdsStore`: committed facts and read-set validation.
- `StarlarkAdapter`: runs package/module Starlark steps and emits node messages.
- `EventBus`: typed event fan-out with backpressure policy.

REQ-DEPSV2-025: Workers waiting on deps SHOULD help the scheduler run other
ready nodes instead of blocking an OS thread.

REQ-DEPSV2-026: The scheduler SHOULD prefer unblocking high-fanout waiters and
short dependency groups before deep serial chains, but this must be a policy
layer over the same graph semantics.

## 16. Early cutoff

REQ-DEPSV2-027: A node recomputation whose output value/fingerprint is equal to
the prior committed value MUST NOT wake downstream nodes as changed.

This is the Shake/Bazel early-cutoff rule. It requires:

- deterministic `NodeValue` equality or fingerprints,
- `ReadSetDigest` for DDS/query reads,
- stable provider/depset/action fingerprints,
- typed transience for errors.

Early cutoff is the main reason versioning and immutable snapshots are not
ceremony. Without those ids, the engine cannot safely decide what did not
change.

## 17. Invalidations

REQ-DEPSV2-028: Invalidations MUST be messages, not direct store mutation.

```rust
pub struct InvalidateRequest {
    pub command_id: CommandId,
    pub workspace: WorkspaceId,
    pub changes: Vec<InputChange>,
}

pub enum InputChange {
    PathChanged(SourceKey),
    RepoMappingChanged,
    OptionsChanged(OptionsDigest),
    RulePackChanged(DeclarationSetId),
    ToolchainChanged(ToolchainResolutionId),
    ForgetAll,
}
```

The actor applies invalidations by advancing `WorkspaceEpoch` and marking graph
state stale for future evaluations. Already committed snapshots remain readable.

## 18. Event volume and backpressure

REQ-DEPSV2-029: The event API MUST distinguish stable user events from debug
graph events.

Profiles:

- `Quiet`: command lifecycle, final diagnostics, snapshot commit.
- `Normal`: user diagnostics, progress, final graph stats.
- `DebugGraph`: node/edge/wait/depset events.
- `TraceAll`: high-volume deterministic test stream.

Debug subscribers MUST have a backpressure/drop policy. For tests, `TraceAll`
MUST be lossless. For daemon UI clients, debug event loss MAY be acceptable if a
`DroppedEvents` event is emitted.

## 19. Migration plan

*Status (2026-06-13): step 1 LANDED — `razel-deps-engine` crate (`api.rs` + `legacy.rs`): the
message API (`RazelDepsEngine` submit/subscribe, `EngineCommand`/`EngineEvent`, `SnapshotId`,
`OptionsDigest`) and a behavior-preserving `LegacyDepsEngine` over `analyze_bazel_with`. Slice-1
seam tests green: `Evaluate(BuildSource)` lifecycle, `SchedHook`→typed-event round-trip, and the
semantic-vs-scheduling options digest. This seam is evidence-justified — the 1-thread and 6-thread
profiles proved the eval wall is the inline single-heap demand recursion (not depsets, not a
single O(N²), not lock contention), which only a deps-as-data scheduler behind this seam can
parallelize. Steps 2+ next.*

*Status (2026-06-13): taut FACT CODEC LANDED (`facts.rs`) — `AnalyzedTarget` ⟷ taut/CBOR bytes
(`razel-wire`) + a blake3 content `Digest` (`razel-core`). This is the move verified in Bazel's
own source: `ObjectCodec.serialize(.., CodedOutputStream)` → `PackedFingerprint`, and the
`NestedSetStore` fingerprint-DAG. `SnapshotCommitted` now carries the snapshot's content `Digest`
(content-addressed commits). It is the keystone for the two biggest wins, neither needing the full
scheduler: (a) serialized facts are `Send` bytes → a worker reads another's frozen result instead
of re-analyzing the shared spine (the measured ~5000 6-thread re-analysis fallbacks); (b) the
`Digest` keys a persistent cross-invocation cache → the second run is near-instant (why Bazel is
fast incrementally).*

*Status (2026-06-13): content-addressed CACHE mechanism LANDED in `LegacyDepsEngine` — an
`Evaluate` whose (source + semantic-options) `Digest` is already cached DECODES the taut snapshot
instead of re-running Starlark (`from_cache` on `SnapshotCommitted`; tested: the hit emits no
analysis diagnostics and yields byte-identical facts). This is the "second run is fast" win in
miniature.*

*Status (2026-06-13): WHOLE-CORPUS loader cache LANDED — the real `tfload` number. `razel-loading`
gained `load_tree_report_with_targets` (the analyzed facts out of a tree load, via a `drive_tree`
refactor; existing signatures intact). `tfload` got an opt-in `RAZEL_TFLOAD_CACHE=<dir>` path:
miss → analyze + serialize all facts to a content-addressed file; hit → decode, skipping analysis.
MEASURED (sample-256, 1 thread): cold **~199s** → warm **2.2s** ≈ **91×**, identical load verdict —
the incremental win, real and end-to-end on TF.*

*Caveats — ADDRESSED (2026-06-13):*
- *Soundness (was the serious one): the key is now a SOURCE fingerprint over every `BUILD`/`.bzl`
  under the corpus (size+mtime), so a loaded-`.bzl` or BUILD edit invalidates it (unit-tested:
  `source_fingerprint_changes_on_bzl_edit_not_on_unrelated_files`); it still hits an unchanged
  corpus. The hit cost rose 465ms→2.2s — that's the whole-corpus walk, the price of soundness. Two
  residual limits, fine for a read-only corpus and noted for the editable build-path cache: it
  ignores glob-affecting changes to NON-`BUILD`/`.bzl` files, and uses size+mtime (not content). A
  precise read-set + content hash (and a faster hit) is the follow-up.*
- *Size: gzip the (highly repetitive) facts — **178 MB → 19 MB (~9×)**.*
- *Granularity: whole-corpus all-or-nothing is DELIBERATE — it sidesteps the live-instance problem
  (a partial cache where an un-cached package depends on a cached one needs live provider
  instances, which the DDS-fact serialization cannot reconstruct — same blocker as the cross-worker
  fallback below). Right for tfload/CI; true incremental is the build-path effort gated on the
  live-instance/typed-provider work.*

*Correction (2026-06-13): the cross-WORKER (cross-thread) fallback is NOT a clean taut slice. Read
the path (`decls.rs` ~1083–1111): the 6-thread fallback re-analyzes because a dep's LIVE Starlark
provider instances (for `dep[Provider]`) are harvested into the shared Session only at the
producing worker's whole-package freeze. The taut fact codec serializes the DDS PROJECTION
(scalars/sets/depsets) — lossy for arbitrary provider fields — so it cannot reconstruct those live
instances. That fix is a threading/freeze-timing change (per-target freeze-and-publish) or a full
Starlark-value serializer, NOT this codec. Taut's clean, codec-sufficient win is the cache.*

*Finding (2026-06-14) — the correction above was too pessimistic; MEASURED it. New diag
`RAZEL_TFLOAD_DIAG_PROVIDER_FIELDS` classifies every captured provider field as DDS-projectable or
not. TF sweep (sample-256): **587/819 (71.7%) projectable, 232 non-projectable — ALL of type
`host_absorbed` (the `Absorb` placeholder, which carries NO data), and ZERO rich fields (no
struct/dict/function).** So a re-analyzed live instance holds nothing a consumer couldn't get from
(the dep's DDS facts in shared `results`) + (a synthesized `Absorb` for the unmodeled slots) — the
loader ALREADY does exactly that synthesis for native-rule deps (`provider_values.rs`
`self.providers.is_empty()`). Therefore the ~5000 cross-thread fallbacks are PURE WASTE, and the
fix is the CHEAP option: extend that DDS-synthesis to the cross-thread Starlark-dep case (serve
`dep[Provider]` from DDS facts instead of re-analyzing) — days, not the per-target-freeze /
serializer (weeks). It kills the fallbacks (real parallelism past 1.6×) AND unblocks incremental
caching (DDS facts are `Send`). Caveat: TF sample only; a struct-valued provider in another corpus
would be the one lossy case — re-run the diag there. This is the keystone, and it's small.*

1. Add the message API types and `LegacyDepsEngine`.
2. Convert `SchedHook` tests to assert typed events via the adapter, keeping the
   old hook as compatibility.
3. Add `CompareDepsEngine` and make `tfload` accept
   `RAZEL_DEPS_ENGINE=legacy|graph|compare`.
4. Implement `DepsetStore` and route depset construction through ids while the
   legacy engine still drives Starlark.
5. Implement graph keys for source stat, directory listing, glob, `.bzl`, and
   package loading.
6. Move target declaration/analysis onto graph nodes.
7. Turn recursive `resolve_dep`/`load_package` paths into `NeedMany`/resume
   message emission.
8. Make graph engine default for `tfload` compare fixtures.
9. Make graph engine default for normal loading.
10. Retire legacy adapter after parity and performance gates hold.

## 20. Acceptance matrix

| requirement | test / gate |
|---|---|
| REQ-DEPSV2-001, 024 | legacy adapter accepts `Evaluate` and emits lifecycle events |
| REQ-DEPSV2-004, 015 | typed event unit tests; legacy `SchedHook` adapter preserves old point/key stream |
| REQ-DEPSV2-005, 011, 012 | query against old snapshot stays stable while later eval commits a new snapshot |
| REQ-DEPSV2-016, 019 | forced `NeedMany` fixture shows multiple workers active and parent blocked/resumed |
| REQ-DEPSV2-018 | two waiters for one key produce one compute attempt |
| REQ-DEPSV2-020 | depset construction with transitive children does not flatten; flatten event occurs only on request |
| REQ-DEPSV2-022 | compile/type test: no live Starlark `Value` in graph value variants |
| REQ-DEPSV2-023 | scheduling thread count changes do not change semantic key digest |
| REQ-DEPSV2-027 | equal recompute early-cuts and does not wake downstream node as changed |
| REQ-DEPSV2-028 | file invalidation advances epoch and recomputes only affected read-set |
| TF regression gate | sample-256 and then broader TF sweep: 6 threads materially faster than 1 thread, no provider reanalysis spike |

## 21. Open decisions

OD-DEPSV2-001: Whether `ContinuationId` maps to hand-written state enums from
the start or begins as restart-per-node for Starlark-heavy nodes.

OD-DEPSV2-002: Exact persistent cache encoding. In-memory V2 can use Rust
types; daemon/wire should use the existing taut/CBOR direction, not JSON.

OD-DEPSV2-003: Whether graph keys live in `razel-loading` or a new
`razel-deps-engine` crate. A new crate is cleaner if it prevents Starlark and
runtime dependencies from leaking into the scheduler.

OD-DEPSV2-004: Event stability policy. Recommended: command/snapshot/query and
diagnostic events are stable; debug graph events are versioned but not promised
as long-term third-party API until the graph engine is default.

OD-DEPSV2-005: Coarsening threshold. Fine-grained graph nodes unlock
parallelism, but tiny nodes can dominate overhead. V2 should measure first and
coarsen hot micro-nodes only after graph diagnostics prove it.

## 22. Design stance

The engine API should be smaller than the system behind it. The boundary is:

```text
commands in -> typed events out -> committed snapshots queried by id
```

That shape supports the old synchronous API, the daemon actor API, the parity
harness, and the future graph scheduler. It also leaves maximum room for
optimization: work stealing, early cutoff, structural depset sharing, durable
inputs, persistent cache, critical-path scheduling, and debug replay all become
implementation choices behind the same message contract.
