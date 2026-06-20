# FixMissingServerImplementationPlan — wire the warm server that was built but never connected

**Status:** DRAFT rev5 — cancel-and-restart (bazel parity, R-9.5 flipped) (2026-06-20). Owner: RR.
Scope: design + parallelized execution plan + the regression-prevention gate suite.

**rev5 note.** R-9.5 is **flipped**: the daemon actor does **cancel-and-restart** (bazel
parity), not the previously-frozen NO-CANCEL. The engine already supports cooperative
cancellation (committed `4ea0c60`, razel-engine 12/0): `Engine::set_cancel` installs a shared
`Arc<AtomicBool>` flag and `request` aborts with `Err("cancelled")` at the next action
boundary (in-flight actions stay atomic). §3.5a/§3.5/G26/R-9.5/R-9.6/§12 are updated to the
cancel-and-restart actor loop; `--batch` remains the no-daemon escape but is no longer the
staleness mitigation (cancel-restart is).

**rev4 note.** The PAR-7/R-9/§5-WS-D "design-before-coding" blocker is **discharged**: the
three coupled WS-D sub-designs — the single-writer actor mechanics (§3.5), the persistent
exec-root incremental fixup (§3.6a), and the cached-analysis key + invalidation rule (§3.6b)
— are now concrete and codeable (algorithms + data structures + exact file:line seams + edge
cases + gates). §5 WS-D drops the DESIGN-HEAVY blocker and lists implementable sub-tasks.
New gates **G24–G27** (§7.2). The actor does **cancel-and-restart** (R-9.5 flipped, bazel
parity, rev5): a watcher event during an in-flight `Build` flips the shared cancel flag, the
engine aborts at the next action boundary, and the actor re-runs on the now-current inputs
(§3.5a). Resolutions logged in §12.1.
Subordinate to `RazelDepsEngineV2.md` (the message-driven engine seam),
`RazelPublicSurfaces.md` (§1 "the CLI is a CLIENT, with no privileged in-process path";
§1c actor + single-writer queue; §4b the streaming/invalidation loop), `RazelDevStatus.md`
(AD7 "one demand-driven engine; CLI + daemon route through it" — currently **P**),
`RazelCodingRules.md` (RULE 4 "no stranded infrastructure"; the tiered-gate discipline).
Companion gap: `ws-razel/RazelGaps.md`.

**rev2 note.** The architecture and the red-first gate philosophy are **approved** by both
reviews and unchanged. rev2 fixes four classes of defect the reviews surfaced and verified
against the tree: (1) the §4 contracts were not actually frozen (placeholder bodies, a lossy
flat-`&[Digest]` compute signature, a non-wire-safe `GlobalFlags` payload, no actor
boundary); (2) **metric honesty** — `BuildResult.recomputes` today is *action executions*,
so the warm gate could certify a cold path (highest risk); (3) WS-C was written as
greenfield when `IncrementalBuilder` is already half the warm path; (4) the 0.3s target
ignored per-build exec-root rebuild + workspace re-analysis. §11 logs every finding→change.

**C3 follow-up correction.** rev2's first cut of C3 used a typed `BuildOptions` wire DTO; that
re-introduced a denormalized copy of the flag set — the very smell we gate against. C3 now
forwards **raw arg tokens + cwd** (bazel's `RunRequest.arg` model) and parses them server-side
with one shared parser (§4.3).

**rev3 note.** A six-lens adversarial self-review (contracts-implementability, concurrency-actor,
gate-rigor-gaming, parallelization-sequencing, bazel-fidelity-northstar, completeness-operational)
augmented the plan **additively** — no approved content was rewritten or deleted. The augmentation
falls into five classes: (1) **spec-vs-code "frozen-but-not-implemented" callouts** — C1/C2/C3 are
documented as frozen, but the engine `ComputeFn` (`lib.rs:17`), the `execute_action`/`Manifest`
types (`razel-exec`), and the wire `build` method (`razel.taut.py:133-134`, only `target: STR`
today) still carry the OLD shapes; rev3 marks these as Wave-0 BLOCKERs the freeze must land before
dependent waves; (2) **C1 borrow-shape correction** — the frozen `DepValue { &'a NodeValue }`
borrow is infeasible against the `RefCell`-stored engine state, so rev3 moves to a cloned value;
(3) **concurrency hazards** — the current daemon has NO actor boundary (RPC threads call `do_build`
directly, `rpc.rs:143-148`), so rev3 promotes the actor to a P1 correctness item and adds risks
R-9/R-9.5/R-9.6 + new gate scenarios; (4) **operational/lifecycle gaps** — idle-out, daemon-crash
recovery, socket permissions, cache GC, progress/action-output streaming, snapshot semantics,
watcher-unavailable fallback, exec-root fixup/corruption semantics — added as §3.7+ subsections,
risks, and gates; (5) **gate-honesty hardening** — wall-clock demoted to observational, counter
scopes pinned, invalidation-trigger and partial-failure variants added. New gates **G16–G23**
(§7.2), new work-stream scope notes (§5), and new risks (§9). Full finding→resolution table: §12.

**One-line summary.** The incremental engine, the daemon, the watcher, the on-disk cache,
and the UDS/CBOR transport all exist and are individually tested — but they were never
assembled into the bazel topology, so a no-op `razel build //:razel` takes ~34s by
re-hashing every input of every action on every invocation; this plan freezes the seam
contracts, fans the wiring out across parallel work-streams, and **gates** the assembled
warm path so a silently-unimplemented server can never recur.

---

## 1. Title + Status

(Above.) This document is both the **design** and the **execution plan**. §4 freezes the
interface contracts (the parallelization enabler); §5–6 define the concurrent work-streams,
the dependency DAG, and the wave schedule; §7 is the heart — the counter split + the enforced
gate suite. §11 is the review-incorporation log.

---

## 2. Why this exists

### 2.1 The bug (verified this session)

A no-op `razel build //:razel` — second invocation, no source change — takes **~34s**, of
which analysis is ~0.3s and **all the rest is `execute_jobs`**. Bazel does the identical
no-op in ~0.15s.

Root cause is a single hot path:

- `run_one_target` (`crates/razel-build/src/exec_root.rs:87-124`) loops over `t.actions`
  and, for **every input of every action on every invocation**, calls `digest_input`
  (`crates/razel-build/src/exec_root.rs:51-67`) — a full blake3 read of the file, or a
  recursive blake3 of the whole directory tree. There is **no stat fast-path, no
  persisted/warm digest, no memoization** (`exec_root.rs:96-100`).
- The on-disk OUTPUT cache (`crates/razel-exec/src/lib.rs:30-65`) is content-addressed,
  but its key is `Digest(argv || input_digests || env || tools || platform || outputs)`
  (`content_key()`), so to even *probe* the cache you must already hold every input digest.
  The re-hash is therefore **unconditional** — the cache cannot save the read.

The re-hash is correct but slow ("right, slowly"). The bug is not a wrong answer; it is the
absence of the warm layer that makes the answer cheap.

### 2.2 Root cause, stated as the mistake we gate against

**A warm incremental server was designed, the parts were built and unit-tested in
isolation, but the parts were never assembled into the live path — and nothing failed
when they weren't.** The engine proves `warm == cold` on an abstract `Digest -> Digest`
graph (`crates/razel-engine/src/lib.rs:202-284`; `incremental_equals_from_scratch` at
`lib.rs:235`), and the *real* warm execution path already exists in `IncrementalBuilder`
(`crates/razel-build/src/incremental.rs`) with its own differential + `warm==cold` tests
(`incremental.rs:267-359`) — but `IncrementalBuilder` is **exported and wired to nothing**
(`crates/razel-build/src/lib.rs:13`; never called from `rpc.rs` or the CLI). The production
daemon path never touches it. No gate counted recomputes or input-re-reads on the *real*
`//:razel` graph, so the gap was invisible.

> **The mistake, named:** *stranded warm infrastructure* — a `RazelCodingRules.md` RULE 4
> violation that hid because the "tested" path (engine + `IncrementalBuilder` unit tests)
> was not the "run" path (the cold daemon shim), a RULE 3 ("tested == run") violation in the
> same breath. §7 makes both un-hideable. **The bug is assembly + metric honesty, not
> missing design** (review 25): half the warm path is already written.

### 2.3 Provenance (git, verified)

| When | Commit | What |
|------|--------|------|
| 2026-06-07 | `8f05382` | "P6 incremental engine — Skyframe-lite w/ early cutoff" — `razel-engine` born; early cutoff at input level (`lib.rs:82`) + output level (`lib.rs:149`,`lib.rs:169`). |
| 2026-06-07 | `5aca8c9` | "P7 daemon + file-watch — warm==cold, Tier 2.5" — the warm `Workspace` core + `notify` watcher (`crates/razel-daemon/src/lib.rs:92-106`). **Test-only** (toy graph `f/a`,`f/b`,`lib`,`bin`). |
| 2026-06-13 | `03be778` | "RG 0008 (user-hit): daemon do_build rides the loader-capable build_workspace_with for //-labels … **ENGINE UNTOUCHED**" — a cold shim bolted onto `Server::do_build` (`crates/razel-daemon/src/rpc.rs:336-407`) so //-labels build *at all* through the daemon, deliberately leaving the engine unwired. |

The **design never changed**. `RazelDepsEngineV2.md` and `RazelPublicSurfaces.md` describe
exactly this warm actor. RG-0008 was an honest expedient — "make //-labels work now, wire
the engine later." This plan is the "later." We label the recurring failure mode **RG-0009**
(the 34s no-op) and the un-implemented-warm-path class it belongs to.

---

## 3. Target architecture

### 3.1 The bazel model (Gianni's directive: "look at bazel, that is the model")

1. A **thin CLIENT** auto-spawns/connects a per-workspace persistent **SERVER** and
   forwards every command, doing no build work itself.
2. The **server holds the WARM incremental graph** (Skyframe) across commands, behind a
   **single-writer per-workspace actor** (§3.5).
3. **Change detection via a watcher** (`--watchfs`) plus a **startup diff/rescan**
   invalidates only affected nodes — unchanged file digests are **never re-read**.
4. An **on-disk action cache** sits underneath.
5. The server holds a **persistent exec-root + cached workspace analysis/projection** for
   the daemon's lifetime (Bazel output-base model) — no per-build forest rebuild, no
   per-build re-analysis (§3.6).

razel already has **every piece**: engine = Skyframe (`razel-engine`), the warm execution
projection (`IncrementalBuilder`, `razel-build/src/incremental.rs`), `notify` watcher
(`razel-daemon/src/lib.rs:92-106`), on-disk cache (`razel-exec/src/lib.rs:30-65`), daemon
`Server` + UDS/CBOR transport (`razel-daemon/src/rpc.rs`). They were never assembled into
this topology.

### 3.2 Mapped onto razel's existing pieces + the design docs

```
  razel CLI (thin client)            ── RazelPublicSurfaces §1: "CLI is a CLIENT, no privileged in-process path"
     │  forwards every verb over UDS/CBOR (auto-spawn the daemon if absent)
     │  build/run forward the raw arg tokens + cwd (§4.3) — never GlobalFlags or a per-flag DTO
     ▼
  razeld Server  (one daemon, many isolated workspaces — RazelPublicSurfaces §1)
     └─ WorkspaceActor (per workspace — §1c, §3.5)  ◄── the SINGLE WRITER
          RPC threads ENQUEUE {Build, SetInput, Rescan, Shutdown}; only the actor mutates state
          ├─ Engine            warm Skyframe graph (razel-engine)        ── AD7, REQ-DEPSV2-003
          │     input nodes  = file/dir InputVersion (digest + mtime/size — REQ-DEPSV2-013, §4.3)
          │     action nodes = effectful ComputeFn → output Manifest     ── C1 (§4.1)
          ├─ Projection        IncrementalBuilder, migrated to C1+C2 (§5 WS-C)
          ├─ Persistent exec-root + cached analysis (§3.6)               ── output-base model
          ├─ Watcher           notify (lib.rs:92-106) → enqueue SetInput/Rescan → §4b loop
          ├─ Startup rescan    full scan → set_input baseline (missed-event safety, NOT mtime-trust)
          └─ Cache             on-disk content-addressed (razel-exec) — UNCHANGED
```

This realizes `RazelPublicSurfaces.md` §4b verbatim: *watcher fires → invalidation enters
the single-writer queue → on commit, the new snapshot swaps in*. It advances AD7 from **P**
toward **L+CI**, and turns the parked `razel-engine` + `IncrementalBuilder` into live callers
per RULE 4.

### 3.3 What is PRESERVED unchanged vs what changes

**PRESERVED (do not touch — the proven incremental core):**

- The Skyframe algorithm in `razel-engine/src/lib.rs:112-176` (`request_inner`):
  `verified_at`/`changed_at` revision logic, `max_dep_changed <= verified_at` early-cutoff
  (`lib.rs:149-155`), no-propagation-on-unchanged-recompute (`lib.rs:169-171`), cycle
  detection (`lib.rs:103-108`), linear scaling. **All six invariants stay green** (§4.1, G8).
- The on-disk cache (`razel-exec/src/lib.rs:30-65`) — content-addressed, already warm. Out
  of scope.
- `content_key()` semantics — `Digest(argv||inputs||env||tools||platform||outputs)`,
  canonical per `RazelCodingRules.md` RULE 22.
- `IncrementalBuilder`'s file→input / action→derived / target→derived projection shape and
  its persistent-sandbox-per-action model (`incremental.rs:105-169`) — it migrates to the
  frozen contracts but its *structure* is preserved; its existing tests stay green (§5 WS-C).

**CHANGES:**

- Engine node **value type** widens `Digest → NodeValue` (single digest *or* output
  manifest); `ComputeFn` becomes **effectful + fallible** and receives **named** dependency
  values (not a flat `&[Digest]`); a new `add_action` is added beside `add_derived`. (§4.1)
- **One shared executor callback** (`execute_action`) — extracted so the cold path and the
  warm engine closure call **identical** code (RULE 3 tested==run; RULE 5 one owner),
  returning an explicit `ExecOutcome` + a `Manifest`. (§4.2)
- `IncrementalBuilder` **migrates** to C1+C2, becomes workspace-label capable, and the
  daemon **owns it** via the WorkspaceActor; the duplicate `fs::read` re-digest in
  `run_action` (`incremental.rs:210-214`) is deleted. (§5 WS-C, §5 WS-D)
- The daemon `do_build` serves via the actor's `engine.request(target)` over a **persistent
  exec-root + cached analysis** instead of the cold shim. (§5 WS-D, §3.6)
- The CLI **defaults to the daemon** (thin client + auto-spawn) and **forwards the raw arg
  tokens + cwd** (the daemon parses them); in-process becomes the opt-in `--batch`. The
  arg→`GlobalFlags` parser moves to `razel-loading` so one parser serves both. (§5 WS-E, §4.3)
- The `bin_tree_layout` flag-pollution divergence (daemon `false` via `GlobalFlags::default()`
  at `rpc.rs:358` vs CLI `true`) is fixed by parsing the forwarded args server-side. (§5 WS-D)

### 3.4 Where correctness comes from (NOT mtime-guessing)

Correctness = **engine-proven `warm == cold`** (the six preserved invariants) **+
watcher-as-authority** (every content change enters the single-writer queue) **+ a startup
rescan** (a missed inotify event cannot cause a stale build — the rescan re-establishes the
digest baseline at daemon start).

> **Explicitly rejected:** a stat-cache that *trusts* mtime to skip a re-read. mtime+size
> can collide on a same-mtime/same-size content edit → a false-skip → a stale build
> ("wrong, faster"). Per `RazelDepsEngineV2.md` §4.3 (REQ-DEPSV2-013): mtime/size MAY be
> an `InputObservation` fast-path *hint*, but **correctness MUST rest on the digest**. The
> watcher tells us *which* files to re-digest; the digest — never the mtime — decides
> whether `changed_at` advances (`razel-engine/src/lib.rs:80-91`).

### 3.5 The single-writer actor boundary (frozen — review 55 P1)

The daemon currently accepts each connection on its own thread (`rpc.rs:140-148`) and shares
`Inner` under `Arc` (`rpc.rs:65-89`). The warm engine is built from `Cell`/`RefCell`
(`razel-engine/src/lib.rs:31-38`) — **not `Send`/`Sync`** — and `IncrementalBuilder` is
explicitly documented as single-threaded ("serialize builds behind a lock if shared",
`incremental.rs:37-45`). Sharing the engine across request threads, or wrapping it in broad
locks while watcher callbacks race builds, is a correctness hazard. So the actor boundary is
a **frozen Wave-0 deliverable**, not an aspiration:

- **Ownership.** `Server` owns **one `WorkspaceActor` per workspace**. The `WorkspaceActor`
  owns the `Engine`, the migrated `IncrementalBuilder` projection, the persistent-sandbox
  map, the persistent exec-root, the cached analysis/projection, and the source-digest
  baseline. Nothing else holds a reference to the `Engine`.
- **Message queue.** RPC threads and the watcher do **not** touch the engine; they **enqueue**
  messages to the actor's single inbox:
  - `Build { args: Vec<String>, cwd: PathBuf, reply }` — parse args → `GlobalFlags`, then `engine.request`.
  - `SetInput { path, digest }` — a *known* leaf input changed; one `set_input`.
  - `Rescan { paths }` — graph-shape or unknown-key events (BUILD/MODULE/lockfile/.bazelrc/
    .razelrc, deletions, unknown files) → re-analyze/re-project, then re-baseline inputs.
  - `Shutdown` — drain + exit.
- **Single writer.** Only the actor thread mutates `Engine`, sandboxes, the exec-root, and
  the baseline. This makes the non-`Send` engine sound by construction.

  > **WARNING (CA-1, P1 — rev3): the current code does NOT enforce this boundary.** `Server::serve`
  > (`rpc.rs:143-148`) spawns each connection on its own thread sharing `Arc<Inner>`, and each
  > thread calls `do_build` (`rpc.rs:336`) directly; `do_run` (`rpc.rs:247`) spawns *another*
  > background build thread. Two concurrent `build()`/`do_run()` calls — or a build racing a
  > `build.subscribe` read — mutate/read `Inner.state` and (once wired) the non-`Send` `Cell`/
  > `RefCell` engine with no queue. **This is the FIRST item of WS-D after the contracts freeze.**
  > Until the actor lands, the daemon must serialize with an explicit lock (test-only) — concurrent
  > builds otherwise risk a `Cell`/`RefCell` borrow panic, silent corruption of `BuildState.revision`
  > ordering, or a reply arriving mid-mutation. See R-9.

- **Build reply channel (CA-2, P1 — rev3).** The `Build` message MUST carry a synchronous (or
  bounded-async) `reply` so the *caller* blocks on the build result, not the connection thread.
  The current `do_run` (`rpc.rs:247-289`) is fire-and-forget: it spawns a thread that appends the
  result to the invocation log via `emit` (`rpc.rs:211`) and returns `InvocationStarted`
  immediately — there is no reply path and no backpressure. The reply channel is necessary for
  (1) **backpressure** (a `SetInput` arriving mid-build is applied to the engine, never corrupts);
  (2) **sequencing** (a watcher event mid-build cancels + restarts the in-flight build — R-9.5,
  cancel-and-restart); (3) **snapshot consistency**
  (subscribers see monotonically advancing committed revisions, not interleaved half-builds).
  Mechanism: a sync `crossbeam::channel` or a `Condvar` like `stream_build_state` (`rpc.rs:293-307`)
  already uses.

- **Message-queue order guarantee (PAR-5, CA-6 — rev3).** The actor inbox is **FIFO, one message
  in flight at a time** (no concurrent handlers). `Rescan` is processed atomically — analysis
  establishes `leaf_inputs`, then all engine nodes update, *before* the next message is dequeued.
  `SetInput` is valid only after a successful `Rescan` (or the startup rescan); a `SetInput` for an
  unknown key panics loudly (`lib.rs:86` `.expect("unknown input")`) and the actor shuts down —
  this is the expected failure mode for a lost-`Rescan`/deserialization bug; silent corruption is
  not possible. The toy `Workspace::sync_file` (`lib.rs:61-63`) preserves the sequence only by
  accident (single-threaded toy use); WS-D must enforce it by construction.

- **Snapshots out.** `build.subscribe` / `invocation.events` receive **committed snapshots**
  emitted by the actor after each message commits (a monotonic `revision`, mirroring
  `BuildState.revision`) — subscribers never read mid-mutation state.

  > **Correctness requirement (CA-3 — rev3).** Every committed snapshot must be a **frozen
  > `BuildState` at a single revision**: a subscriber parking on `wait_while(revision == last)`
  > (`rpc.rs:301`) must receive a clone taken *within the same `Mutex` critical section* as the
  > revision read, so no interleaving of `targets[]` mutations (`record_state` retain/push/sort,
  > `rpc.rs:410-436`) is visible. The current code clones inside the lock guard scope
  > (`rpc.rs:297-304`) — correct — but G11 must *assert* it (two subscribers on the same revision
  > see byte-identical `BuildState`, not merely the same revision number).

  > **Staleness window (CA-5 — rev5, cancel-and-restart).** A subscriber that connects *during* a
  > long build reads the *previous* committed snapshot until a build commits. A watcher event
  > arriving mid-build flips the shared cancel flag; the engine aborts the in-flight `request` at
  > the next action boundary (`Err("cancelled")`), the actor drains the queued `SetInput`/`Rescan`,
  > clears the flag, and **re-runs the build on the now-current inputs** (see R-9.6). The subscriber
  > sees the committed snapshot of the build that actually completes — never a stale-input commit.
  > The residual staleness is just the current atomic action's remaining runtime (in-flight actions
  > are never interrupted mid-spawn), not the full build duration. `razel build --batch` remains the
  > no-daemon escape (it bypasses the daemon entirely), but it is no longer the staleness mitigation.
- **Migration checklist item:** the toy `razel_daemon::Workspace` (`lib.rs:44-87`) is the
  shape the actor adopts, but the **standalone toy type is deleted/repurposed** so warm
  infra is not stranded a second time (see §5 WS-D, §8).

#### 3.5a Concrete actor shape (rev4 — codeable)

The §3.5 contract above is now a concrete design. The actor is a dedicated OWNER thread; the
`Server` keeps only the inbox `Sender`s (one per workspace) under `Arc` — nothing else holds the
non-`Send` `Engine`.

**Inbox message enum + reply channel** (`crates/razel-daemon/src/rpc.rs`, new, after the `Inner`
struct ~line 83):
```rust
pub enum ActorMessage {
    Build  { args: Vec<String>, cwd: PathBuf, reply: SyncSender<Result<BuildResult, String>> },
    SetInput { path: String, digest: Digest },           // a KNOWN leaf changed (one set_input)
    Rescan { reason: RescanReason },                      // graph-shape / unknown / delete
    Shutdown,
}
pub enum RescanReason { StartUp, WatcherBuildChange, WatcherSourceTreeChange }
```
Queue = `std::sync::mpsc` (or `crossbeam::channel`) **unbounded**; `reply` is a one-shot
`std::sync::mpsc::sync_channel(1)` so the RPC thread blocks on the actual build result, not on the
connection thread (CA-2). FIFO, **one message in flight at a time** (CA-6, PAR-5).

**WorkspaceActor state** (the owner; subsumes today's `Inner.warm`/`analyses` + the toy `Workspace`):
```rust
pub struct WorkspaceActor {
    inbox: Receiver<ActorMessage>,
    workspace: PathBuf,
    engine: Engine,                                   // non-Send; single-writer by construction
    projection: IncrementalBuilder,                   // migrated (WS-C), workspace-label capable
    exec_root: PathBuf,                               // persistent .razel-out forest (§3.6a)
    cached_analysis: Option<(AnalysisDigest, Vec<AnalyzedTarget>, String /*resolved_top*/)>, // §3.6b
    cancel: Arc<AtomicBool>,                          // shared cancel flag; engine.set_cancel (R-9.5)
    leaf_inputs: HashSet<String>,                     // authority for SetInput-vs-Rescan (§3.5)
    old_sources: HashSet<String>,                     // exec-root fixup baseline (§3.6a)
    old_external: Option<PathBuf>,                    // last .razel-crates path (§3.6a)
    // counters surfaced on each BuildResult (§7.1):
    engine_recomputes_seen: usize, actions_executed: usize, action_cache_hits: usize,
    input_digest_reads: usize, exec_root_rebuilds: usize, analysis_runs: usize,
    // snapshot publication (mirrors today's Inner.state/bump, rpc.rs:74-75,99-103):
    state: Arc<Mutex<BuildState>>, bump: Arc<Condvar>,
}
```

**The single-writer loop** (the actor's `run`):
```rust
while let Ok(msg) = self.inbox.recv() {
    match msg {
        ActorMessage::Build { args, cwd, reply } => {
            // cancel-and-restart (R-9.5): loop until a build completes uncancelled.
            let r = loop {
                let r = self.do_build_impl(&args, &cwd);  // parse_opts→GlobalFlags+daemon-derived;
                                                          //  cached-analysis (§3.6b); engine.request
                if !matches!(&r, Err(e) if e == "cancelled") { break r; }
                self.drain_invalidations();   // apply queued SetInput/Rescan from the inbox
                self.cancel.store(false, Ordering::SeqCst); // clear flag, re-run on current inputs
            };
            let _ = reply.send(r);                    // caller blocks on the FINAL (uncancelled) result
            self.commit_snapshot();                   // revision += 1; bump.notify_all() (CA-3)
        }
        ActorMessage::SetInput { path, digest } => {  // guarded: must be in leaf_inputs (§3.5)
            if !self.leaf_inputs.contains(&path) { /* loud shutdown — lost-Rescan bug (PAR-5) */ }
            self.engine.set_input(&path, NodeValue::Digest(digest)); // NO snapshot (input-only)
        }
        ActorMessage::Rescan { reason } => { self.handle_rescan(reason); } // §3.6a+§3.6b, atomic
        ActorMessage::Shutdown => break,
    }
}
```

**Startup ordering** (in `Server::serve`, `rpc.rs:134-151`, after `outlock` at :137):
1. Acquire `outlock` (workspace writer lock).
2. Spawn the actor thread with its inbox.
3. Enqueue `Rescan { StartUp }` and **block until it completes** (synchronous startup baseline):
   `validate_exec_root` (§3.6a) → `analyze_workspace_resolved` (`drive.rs:58`) → establish
   `leaf_inputs` → re-digest every leaf input → `engine.add_input(path, digest)`. After this,
   `revision = 1` and **every leaf key exists**, so a later `SetInput` can never hit the
   `lib.rs:86` unknown-key panic.
4. `transport::bind(socket)` + accept loop; each connection thread ENQUEUES messages (it no longer
   calls `do_build` directly — that is the R-9/CA-1 fix).
5. Spawn the watcher loop (§3.6, CA-7), routing to `SetInput`/`Rescan` per the §3.5 sequence. On a
   filesystem change the watcher **both** flips the shared `cancel` flag (`cancel.store(true)`) AND
   enqueues the `SetInput`/`Rescan` message(s) — so an in-flight `Build` aborts at the next action
   boundary and the actor re-runs on the invalidated inputs (R-9.5, cancel-and-restart).

**Cancel-and-restart semantics (R-9.5 flipped — bazel parity, rev5).** Single-writer is preserved:
the `Build` still runs on the actor thread via `engine.request`. The **watcher** thread (separate)
flips the shared `Arc<AtomicBool>` cancel flag on a filesystem change AND enqueues the
`SetInput`/`Rescan` message(s). When a watcher event arrives DURING an in-flight `Build`:
1. The engine's next **action-boundary** check sees the flag (helper `cancelled()`), so
   `engine.request` returns `Err("cancelled")` — an in-flight action stays atomic (never interrupted
   mid-spawn). This is the engine support committed in `4ea0c60` (`Engine::set_cancel`).
2. The actor **drains** the queued `SetInput`/`Rescan` from its inbox (applies the invalidations to
   the engine), **clears** the cancel flag, and **re-runs** `do_build_impl` (`engine.request`) on the
   now-current inputs.
3. It loops (the `Build` arm above) until a build completes uncancelled, then sends that result on
   the reply channel (CA-2 unchanged — the caller blocks on the final, uncancelled result).

A **committed snapshot is emitted only for the build that actually completes** — cancelled attempts
commit nothing, so subscribers never see a stale-input revision. A cancelled-then-restarted build
re-validates from the current revision, so **warm == cold still holds**. The message enum, the reply
channel, the snapshot path, and §3.6a/§3.6b are unchanged from the FIFO baseline; the `Build` arm
gains the cancel/drain/re-request cycle. **Residual risk (minor):** a long build can be restarted
repeatedly if edits keep arriving (bounded-livelock) — bounded in practice by edit frequency, and
mitigated because each restart reuses the action cache + warm digests, so only the changed subgraph
re-runs.

**Committed-snapshot emission (CA-3).** `commit_snapshot` clones `BuildState` **inside the same
`Mutex` critical section** as the `revision += 1`, then `bump.notify_all()` — identical to today's
`record_state` (`rpc.rs:429-435`). Two subscribers parking on `wait_while(revision == last)`
(`rpc.rs:301`) at the same revision therefore receive byte-identical clones (G11). `SnapshotId ==
revision`, epoch-disambiguated (§4.3c).

**Idle-out + panic handling.** Idle-out (§3.7): when a workspace's connection/subscriber refcount
hits 0 for `--idle-timeout` (default 5 min), the actor drains in-flight work and exits (`Shutdown`);
the daemon respawns lazily on the next hello. **Actor panic** (e.g. a re-entrancy bug in a
`ComputeFn`, §4.1b): the actor thread's `catch_unwind` wrapper drops the inbox; every pending
`Build.reply.recv()` then returns `Err` ("razel internal: actor panic") so RPC threads unblock
instead of hanging. The actor does **not** auto-restart in v1 — recovery is via daemon restart +
the startup `Rescan` (G17), which re-baselines from scratch.

- **Watcher not yet wired (CA-7 — rev3).** `watch()` (`lib.rs:92-106`) is defined and unit-tested
  in isolation (`lib.rs:204-219`) but is **never called from `Server::serve`** (`rpc.rs:134-150`).
  Until WS-D wires the watch loop into the actor queue, the daemon is **subscribe-driven** —
  on-disk changes are not detected unless a client drives a build. This is a known v1 limitation
  (R-5); document it in `razel daemon --help` ("not a live-reload server without an active
  subscriber"). G12 requires the watcher wired into the actor with correct SetInput-vs-Rescan
  routing; before then, the watcher is parked.

Gated by **G11** (§7).

### 3.6 Persistent exec-root + cached analysis (frozen — both reviews P1)

`engine.request` alone does **not** reach bazel-parity wall-clock if the daemon still rebuilds
the exec-root forest and re-analyzes the workspace every build. Verified per-build work today:

- `prepare_exec_root` (`crates/razel-build/src/drive.rs:68-75`) **unconditionally**
  `remove_dir_all` + `create_dir_all`s the `.razel-exec` symlink forest whenever
  `.razel-crates` exists — every call.
- The `//`-label branch (`rpc.rs:353-359`) always calls `build_workspace_with` (full
  analyze + `prepare_exec_root` + `execute_jobs`) with `GlobalFlags::default()`. It **never**
  uses `warm_analyze` (`rpc.rs:153-172`, BUILD-digest-keyed) — that fast path exists today
  but is wired only for single-BUILD bare labels, not `//` labels.

**Frozen decision:** the `WorkspaceActor` holds, for the daemon's lifetime:
- a **persistent exec-root** (built once, incrementally fixed up by the watcher; never
  `rm -rf`'d per build), and
- a **cached workspace analysis/projection** for `//` labels, keyed and invalidated by the
  graph-shape inputs (BUILD/MODULE/lockfile/`.bazelrc`/`.razelrc`), not rebuilt per build.

Two counters make this provable on the second no-op (§7): `exec_root_rebuilds == 0` and
`analysis_runs == 0`. Gated by **G14**; added to acceptance §10.

> **Note (EXEC-ROOT-REBUILD-COUNTER-001 — rev3).** `prepare_exec_root` is called from
> `build_workspace_with` (`drive.rs:69`) **whenever `.razel-crates` exists** (`drive.rs:67`) — which
> it does for the external-crate `//:razel` build — and it *always* `remove_dir_all` +
> `create_dir_all`s the forest (`exec_root.rs:13-14`); there is no "if missing/stale" branch and no
> counter. So G14's `exec_root_rebuilds == 0` will FAIL on the current cold path, masking the
> warm-path gap. WS-F adds the counter (a `&mut usize` passed into `prepare_exec_root`, incremented
> at the call site `drive.rs:69`); WS-D's persistent exec-root sets it to 0 by skipping the rebuild
> when the forest is persistent and unchanged.

#### 3.6a Persistent exec-root fixup semantics (CONCRETE — rev4; PAR-2, SR-EXEC-ROOT-CORRUPTION-1)

The exec-root forest is a symlink snapshot of the workspace SOURCE entries + `external/<repo>`. Today
`prepare_exec_root` (`exec_root.rs:11-35`) unconditionally `remove_dir_all` + recreates it whenever
`.razel-crates` exists (`drive.rs:67-69`). rev4 keeps `prepare_exec_root` for the **cold path**
(non-daemon `build_workspace_with`) and adds two new functions the actor owns; the source-exclusion
list is the SAME one `prepare_exec_root` uses (`exec_root.rs:19-21`: `.razel-*`, `.git*`, `target`,
`bazel-out`, `razel-out`, `razel-bin`, `razel-testlogs`).

**`fix_up_exec_root` algorithm** (`exec_root.rs`, new ~line 36, called from the actor's `handle_rescan`):
```rust
pub(crate) fn fix_up_exec_root(
    workspace: &Path, exec_root: &Path,
    old_sources: &HashSet<String>, new_sources: &HashSet<String>,
    old_external: Option<&Path>,  new_external: Option<&Path>,
) -> std::io::Result<bool> /* did_change */ {
    let mut changed = false;
    // 1. ADDED sources: symlink them. Unlink any stale entry first (broken link / E4 collision).
    for src in new_sources.difference(old_sources) {
        let link = exec_root.join(src);
        if link.symlink_metadata().is_ok() { let _ = std::fs::remove_file(&link); } // E4: real-dir/broken
        std::os::unix::fs::symlink(workspace.join(src), &link)?; changed = true;
    }
    // 2. DELETED sources: unlink (ENOENT is OK — already gone). E2: a sandbox pointing at it now
    //    sees a broken link → digest_input returns None (exec_root.rs:52) → C2 absent-input, no panic.
    for src in old_sources.difference(new_sources) {
        let _ = std::fs::remove_file(exec_root.join(src)); changed = true;
    }
    // 3. EXTERNAL (.razel-crates → external/): re-symlink only on appear/disappear/move (E3).
    let ext = exec_root.join("external");
    let ext_changed = match (old_external, new_external) {
        (None, None) => false, (Some(a), Some(b)) => a != b, _ => true,
    };
    if ext_changed {
        let _ = std::fs::remove_file(&ext);
        if let Some(c) = new_external { std::os::unix::fs::symlink(c, &ext)?; }
        changed = true;
    }
    Ok(changed)
}
```
**Properties.** Unchanged sources are never touched → **zero churn**, preserving the
`IncrementalBuilder` persistent-sandbox reuse (a sandbox's `sync_inputs` is a no-op delta unless the
input *set* changes). The return `did_change` drives the `exec_root_rebuilds` counter (§7.1): the
actor increments it **once per Rescan iff `did_change == true`** — a no-op Rescan does not increment,
so G14's `exec_root_rebuilds == 0` holds on the second no-op build.

**`validate_exec_root` (startup/crash recovery)** (`exec_root.rs`, new ~line 81; called from the
`Rescan { StartUp }` handler BEFORE the first fixup): for each expected source dir, assert
`exec_root.join(src)` is a live symlink whose target exists; for each `external/*`, assert it points
to a live `.razel-crates` entry. A broken/missing/wrong entry is unlinked + re-symlinked (best-effort,
with backoff on symlink-create failure); if a critical link is unrepairable it returns `Err` and the
actor falls back to a full `prepare_exec_root` rebuild (counts as one `exec_root_rebuilds`). A wholly
corrupted `.razel-out` is harmless — the content cache (`razel-exec`) re-populates outputs. Gated by
**G21** (manual corruption repaired) and **G17** (crash recovery).

**`handle_rescan` (the actor side that drives this — §3.5a):**
```rust
fn handle_rescan(&mut self, reason: RescanReason) {
    if reason == RescanReason::StartUp { let _ = validate_exec_root(&self.workspace, &self.exec_root); }
    let new_sources  = enumerate_sources(&self.workspace);        // read_dir minus the exclusion list
    let new_external = self.workspace.join(".razel-crates").is_dir()
                         .then(|| self.workspace.join(".razel-crates"));
    let did_change = fix_up_exec_root(&self.workspace, &self.exec_root,
                        &self.old_sources, &new_sources,
                        self.old_external.as_deref(), new_external.as_deref()).unwrap_or(true);
    if did_change { self.exec_root_rebuilds += 1; }
    self.old_sources = new_sources; self.old_external = new_external;
    self.cached_analysis = None;        // §3.6b: any Rescan marks analysis dirty (recompute next Build)
    // (re-)establish the leaf_inputs baseline; on StartUp re-digest every known input.
    self.rebaseline_leaf_inputs(reason);
}
```
This keeps the exec-root fixup (add/delete/rename → symlink/unlink) **consistent with** the §3.6b
"graph-shape event → analysis dirty" rule and the §3.5 SetInput-vs-Rescan routing: the SAME `Rescan`
that re-symlinks the forest also marks analysis dirty and refreshes `leaf_inputs`.

**Edge cases (verified against `digest_input`, `exec_root.rs:51-67`):** E1 new top-level source dir →
symlinked, no churn elsewhere. E2 deleted source with a stale sandbox ref → broken link →
`digest_input` `None` → C2 absent-input, no panic; the next Rescan invalidates the stale analysis.
E3 `.razel-crates` materialized on demand → `external/` symlink appears once (None→Some), reused
thereafter (Some==Some). E4 user creates a real dir where a symlink belongs → unlinked + re-symlinked
(best-effort; the exec-root is razel-owned). E5 case-insensitive FS → `enumerate_sources` uses the
canonical entry name so symlink paths match the action's input declarations. E6 watcher event during
Rescan → FIFO queue serializes it after Rescan completes (single-writer, §3.5).

#### 3.6b Cached-analysis key & invalidation (CONCRETE — rev4; PAR-4, OPER-7)

The actor owns a persistent workspace analysis keyed by an **`AnalysisDigest`** — content-based, not
mtime-based (an mtime fingerprint false-skips a same-mtime edit and false-invalidates a
copy-with-preserve-mtime). It migrates today's `warm_analyze` (`Mutex<Option<WarmAnalysis>>`,
`rpc.rs:157-172`, single-BUILD-digest-keyed) into the actor and generalizes it to the `//`-label
case via `analyze_workspace_resolved` (`drive.rs:58`).

**The key:**
```rust
struct AnalysisDigest { graph_shape_digest: Digest, options_digest: Digest }

fn compute_analysis_digest(root: &Path, flags: &GlobalFlags) -> AnalysisDigest {
    // graph-shape inputs = every file whose change alters BUILD-graph TOPOLOGY (NOT source files):
    //   all BUILD/BUILD.bazel (discover_packages walk), MODULE.bazel, MODULE.bazel.lock,
    //   .bazelrc, .razelrc  — each "if present".
    let mut entries: Vec<(String, Digest)> = Vec::new();
    for p in walk_buildfiles(root) { if let Ok(b)=fs::read(&p){ entries.push((rel(&p), Digest::of(&b))); } }
    for n in ["MODULE.bazel","MODULE.bazel.lock",".bazelrc",".razelrc"] {
        if let Ok(b)=fs::read(root.join(n)) { entries.push((n.into(), Digest::of(&b))); }
    }
    entries.sort_by(|a,b| a.0.cmp(&b.0));           // canonical order (RULE 22)
    let mut buf = Vec::new();
    for (p,d) in entries { buf.extend(p.as_bytes()); buf.push(0); buf.extend(d.to_hex().as_bytes()); buf.push(0); }
    AnalysisDigest { graph_shape_digest: Digest::of(&buf), options_digest: flags.options_digest }
}
```
`options_digest` already lives on `GlobalFlags` (the semantic-flag fingerprint), so a flag change
(e.g. `--copt`) invalidates analysis too. `crate_lock`/`fetched_external_base` are NOT keyed
directly — they are DERIVED from `MODULE.bazel(.lock)`, which IS in `graph_shape_digest`, so a
lockfile change invalidates correctly (closes the GlobalFlags-mutable-field hole).

**The invalidation rule (matches §3.6a's "graph-shape event → Rescan"):**
- Any `Rescan` sets `cached_analysis = None` (dirty) — §3.6a `handle_rescan` does this in one line.
- On the next `Build`, `do_build_impl` recomputes `AnalysisDigest` and compares to the cached one:
  if **different OR dirty** → re-run `analyze_workspace_resolved`, cache `(digest, targets,
  resolved_top)`, `analysis_runs += 1`; if **same** → reuse the cached projection, `analysis_runs`
  unchanged. So a no-op second build is `analysis_runs == 0`; a BUILD edit routes to `Rescan`
  (§3.5/§3.6a) then the next build re-analyzes (`analysis_runs > 0`); a further no-op re-caches
  (`analysis_runs == 0`).

**`do_build_impl` (the Build-side seam — §3.5a):**
```rust
fn do_build_impl(&mut self, args: Vec<String>, cwd: PathBuf) -> Result<BuildResult, String> {
    let flags = parse_opts(&args).resolve(&cwd).with_daemon_derived(&self.workspace); // §4.3
    let target = parse_target(&args);
    let key = compute_analysis_digest(&self.workspace, &flags);
    let fresh = self.cached_analysis.as_ref().map(|(d,_,_)| d) != Some(&key);
    if fresh {
        let (targets, resolved) = analyze_workspace_resolved(&self.workspace, &target, flags.clone())?;
        self.projection.configure(&targets, &resolved); // (re)wire the engine graph + leaf_inputs (WS-C)
        self.cached_analysis = Some((key, targets, resolved));
        self.analysis_runs += 1;
    }
    self.engine.reset_recomputes();
    let value = self.engine.request(&target_key(/*resolved top*/))?;
    self.engine_recomputes_seen = self.engine.recomputes();      // G4 reads THIS, not report.executed
    Ok(/* BuildResult from value + self.counters */)
}
```
Partial (per-package) invalidation is a future optimization; full workspace re-analysis on any
graph-shape change is the conservative, correct v1. Gated by **G14** (no-op `analysis_runs == 0` +
the invalidation trigger: edit a BUILD → `analysis_runs > 0` → re-cache → 0).

### 3.7 Server lifecycle, sockets, cache, output streaming (rev3 — operational completeness)

These were unspecified in rev2; v1 freezes them here.

- **Idle-out (SR-LIFECYCLE-1).** `razeld` idles out after a default **5 minutes** with all
  workspaces' refcount == 0 (no live connection / subscriber). On-disk `daemon.json` (in the daemon
  dir) carries `{ pid, wire_version, build_version, socket, start_epoch }`. On client startup: read
  `daemon.json`; if the PID is dead, reap + respawn; the **only** client-initiated kill is the
  hello version-skew restart (G10), which survives idle-out. Shutdown drains in-flight builds (no
  mid-build kill). `--idle-timeout` is opt-in to override the default (razel-specific; grazeld's
  scope-daemon model is separate). Authority: RazelPublicSurfaces.md §1a.
- **Output-base layout & recovery (SR-OUTPUT-BASE-MODEL-1).** The exec-root is `.razel-out` (matches
  the CLI's convenience-symlink convention). It is built once at first-workspace-open and patched
  incrementally (§3.6a), never torn down per build. The persistent sandbox (keyed by
  `action.content_key().to_hex()`, §4.2) is a write-once-per-build scratchpad — Tier-1 spec is
  "sandboxes do not leak between actions," NOT "persist across daemon restarts." The on-disk content
  cache (`razel-exec`) is the artifact store, indexed by content_key (survives restarts + snapshots).
  Cross-daemon output-base arbitration is §1b `outlock` (`rpc.rs:137`, already specified). Crash
  recovery: the startup Rescan re-baselines; no exec-root clean needed (G17, G21).
- **Socket permissions & isolation (OPER-3).** The daemon dir is `0700`; the socket is created
  `0600` (owner r/w only). A client hello naming a workspace it cannot access is rejected
  ("permission denied", naming the boundary). Each workspace's actor owns its own lock + input
  state; a hello for workspace X operates only on X's actor; cross-workspace snapshot reads are
  blocked. Gated by **G18** (isolation); socket-mode is a transcript assertion under G18's umbrella.
- **Cache lifecycle & GC (OPER-4).** The on-disk action cache is a safe append-only store keyed by
  content_key; **no automatic eviction in v1** — `razel clean --expunge` removes it. The persistent
  exec-root is fixed up (not GC'd) per §3.6a; periodic GC of unreferenced inodes is future work.
  Cached analysis is keyed by the §3.6b graph-shape digest; a Rescan frees the old snapshot when its
  reader count drops to zero. No memory-pressure policy v1 — a single workspace is expected to fit
  in RAM (R-9.8).
- **Progress / action-output streaming (OPER-1).** Today the `Progress` event
  (`razel.taut.py:86-92`) carries only phase/done/total/detail — **no action stdout/stderr**. A
  daemon build must surface a failing action's OWN stderr or the thin client cannot explain the
  failure. Freeze: add an optional `action_stderr`/`detail` field to a failure `InvocationEvent`
  (or a new `ActionEvent` variant) carrying the captured stderr on failure; large outputs are
  chunked with backpressure (future bounded-buffer + resync, per RazelPublicSurfaces.md §4b). See R-9.9.
- **Thread budget across workspaces (OPER-8).** A global thread-budget manager caps total engine
  threads across all per-workspace pools at `cores` (RazelPublicSurfaces.md §1c); each actor's pool
  is lazy (created on demand, shrinks to zero when idle). Blocking handler ops (e.g. a future
  interactive query) run on separate handler threads, never holding an engine thread. See R-6.

---

## 4. Interface contracts (FROZEN FIRST — the parallelization enabler)

These contracts are frozen in **Wave 0** before any stream starts, with **real signatures and
real semantics — no `/* sketch */` placeholders** (review 25 / 55 P1). Every work-stream codes
against the frozen signatures; integration in Wave 2 is mechanical because nothing drifted.
A Wave-0 **compile-check contract test** (§4.4) type-checks the signatures before any impl
exists. Changing a frozen contract after Wave 0 requires a one-line note here + a ping to
every consuming stream.

### 4.1 Contract C1 — the Engine API (named deps; manifest value; effectful ComputeFn)

The incremental algorithm (`razel-engine/src/lib.rs:112-176`) is **unchanged**; only what
node values *carry*, what `ComputeFn` *receives*, and what it *returns* widens.

```rust
// crates/razel-engine/src/lib.rs

use razel_core::Digest;

/// One compute error. Canonical, String-based at this seam (matches today's
/// request() Result<_, String> and IncrementalBuilder's error sink). A richer
/// razel_core error type is future work and would be a deliberate C1 re-freeze.
pub type ComputeError = String;

/// A node's computed value: a single content digest (pure node) OR an output
/// manifest (action node, one (path, digest) per *present* declared output).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeValue {
    Digest(Digest),
    /// Canonical: sorted by path (RULE 22), no duplicate paths. Empty == an action
    /// that declared no outputs (or all declared outputs were legitimately absent —
    /// see C2 §4.2 absent-output semantics). See Manifest below for path/dir rules.
    Manifest(Manifest),
}

/// Output manifest. Always canonical: entries sorted by `path`, paths normalized
/// (forward slashes, no `.`/`..`, relative to exec-root), unique. A directory
/// output's `digest` is the deterministic recursive tree-hash — the SAME tree-hash
/// `digest_input` uses for input directories (exec_root.rs:51-67), so a dir output
/// consumed downstream as an input compares byte-for-byte.
pub type Manifest = Vec<(String, Digest)>;

impl NodeValue {
    /// Early-cutoff equality: structural, digest-by-digest. Two NodeValues are equal
    /// iff same variant AND (Digest: equal digest) | (Manifest: equal length and
    /// equal (path, digest) pairwise — both are canonical so positional compare is
    /// total). Mixed variants are never equal.
    pub fn digest_equal(&self, other: &NodeValue) -> bool {
        match (self, other) {
            (NodeValue::Digest(a), NodeValue::Digest(b)) => a == b,
            (NodeValue::Manifest(a), NodeValue::Manifest(b)) => a == b, // canonical ⇒ Vec Eq
            _ => false,
        }
    }
}

/// A dependency value handed to compute, carrying the dep's KEY so the closure can
/// reconstruct the exact path→digest map for Action.inputs without re-reading the
/// filesystem and without positional conventions (review 55 P1: flat &[Digest] is
/// lossy). The `key` is the dep's NODE key (SR-GATE-15): for a file input it is the
/// file node key (== the file path); for an upstream ACTION it is that action's node
/// key (e.g. "act:app/BUILD#2"), and `value` is that action's Manifest — so a
/// downstream consumer iterates the (path, digest) pairs and picks the ONE output
/// path it needs out of a multi-output producer. The key is an ACTION identity, not a
/// path: if the SAME path appears in two upstream actions' outputs (name collision),
/// the consumer must declare BOTH as separate deps; path-based selection assumes
/// output names are unique across an action's dep set (rule-author hygiene, NOT
/// engine-enforced — see the frozen risk rule below and R-9.7).
///
/// rev3 (C1-DEPVALUE-001): the `value` is a **cloned** NodeValue, not a borrow. The
/// initial C1 cut used `&'a NodeValue` borrowed from the engine; that is infeasible
/// against the RefCell-stored node table (lib.rs:24-29,31-38) — a closure that may
/// recursively call request_inner cannot hold a transient RefCell borrow open across
/// its whole execution. The clone is negligible: the common case is `Digest` (Copy);
/// a Manifest clone is a small Vec. WS-A clones at compute time.
pub struct DepValue {
    pub key: String,
    pub value: NodeValue,
}

/// Effectful + fallible action compute. Receives NAMED dep values (not a flat digest
/// list). Actions run inside; failures surface as Err(ComputeError).
/// SAFETY (SR-C1-DEPVALUE-BORROW-SAFETY-1): the slice is valid only for the lexical
/// scope of the request() call; a ComputeFn MUST NOT store a DepValue past return and
/// MUST NOT call any Engine method (re-entrancy hazard on the non-Send engine, §3.5).
/// See §4.1b.
pub type ComputeFn = Box<dyn Fn(&[DepValue]) -> Result<NodeValue, ComputeError>>;

impl Engine {
    pub fn add_input(&self, key: &str, value: NodeValue);

    /// PURE node: deterministic function of dep digests; cannot fail at the seam
    /// (pure-graph errors are programming bugs). Used for target-aggregation nodes
    /// (combine over action manifests) — NOT for actions.
    pub fn add_derived(
        &self,
        key: &str,
        deps: &[&str],
        compute: impl Fn(&[DepValue]) -> NodeValue + 'static,
    );

    /// EFFECTFUL action node: runs the action via execute_action (C2). Returns the
    /// output Manifest (so output-level early cutoff has teeth) or an error.
    pub fn add_action(
        &self,
        key: &str,
        deps: &[&str],
        execute: impl Fn(&[DepValue]) -> Result<NodeValue, ComputeError> + 'static,
    );

    pub fn set_input(&self, key: &str, value: NodeValue); // input-level early cutoff (lib.rs:80-91)
    /// Demand the validated value. Pure/derived nodes return Digest|Manifest per
    /// their producer; callers ALWAYS match on the enum (no implicit unwrap).
    pub fn request(&self, key: &str) -> Result<NodeValue, ComputeError>;
    pub fn recomputes(&self) -> usize;           // ENGINE node recomputes (NOT actions executed)
    pub fn reset_recomputes(&self);
}
```

> **SPEC-vs-CODE (SR-GATE-13, P1 — rev3): C1 is frozen-as-documented, NOT yet implemented.**
> Today `crates/razel-engine/src/lib.rs:17` is `type ComputeFn = Box<dyn Fn(&[Digest]) -> Digest>`
> and `Node.value` is `Option<Digest>` (`lib.rs:24-29`) — the OLD signature. "Frozen in Wave 0"
> means the SIGNATURES above are agreed and the §4.4 compile-check test type-checks against them;
> the **implementation is WS-A's Wave-1 deliverable** (rewrite `lib.rs:17-21`, re-express the six
> engine tests with `NodeValue`). The compile-check is a stub-only typecheck, not the algorithm.
> Do not read "frozen" as "already in code."

**Concrete semantic decisions (frozen):**
- `set_input` against an **unknown key panics today** (`lib.rs:86` `.expect("unknown input")`).
  Callers MUST NOT call it on unknown keys. The WorkspaceActor establishes leaf inputs via
  analysis/projection FIRST (`Rescan`), then updates them (`SetInput`) — never a blind
  `set_input` (see §3.5, §5 WS-D). `IncrementalBuilder::sync_file` already guards with
  `leaf_inputs.contains` (`incremental.rs:177`); the daemon path must too.
- Empty manifest = zero-output action; downstream cutoff on it is trivially stable.
- Absent declared output: **omitted** from the manifest (matches the current executor, §4.2).
- Directory output: a single `(dir_path, tree_hash)` entry, tree-hash identical to input dirs.

**Invariants this contract must keep green** (re-asserted with manifests, gate G8): early
cutoff at input level (`lib.rs:80-91`) + output level (`lib.rs:149`,`lib.rs:169`);
`incremental == from-scratch`; no-op == zero recompute; linear scaling; cycle detection. The
early-cutoff comparison at `lib.rs:169` changes from `n.value != Some(new)` to
`!old.digest_equal(&new)`.

**Frozen risk rules** (actions must uphold these or `warm==cold` breaks): action outputs
deterministic (same input digests → same output digests); `Manifest` always canonical;
declared deps complete (undeclared reads → false-skip — caught by the differential gate G6).

### 4.1b DepValue safety & re-entrancy (frozen — SR-C1-DEPVALUE-BORROW-SAFETY-1)

The `&[DepValue]` slice handed to a `ComputeFn` is valid only for the lexical scope of the
`request()` call that drove the compute. Frozen rules:
- A `ComputeFn` MUST NOT store a `DepValue` (or its `value`) past return, and MUST NOT call any
  `Engine` method. Re-entering `request()`/`set_input()` from inside a compute is a re-entrancy
  hazard on the non-`Send`, `RefCell`-backed engine (§3.5) and is forbidden by contract.
- The §4.4 `c1_named_deps_not_lost` test gains a **compile-fail companion** (commented-out code +
  a note): an attempt to call `engine.request()` inside the closure must NOT compile (the closure
  does not capture `&Engine`). Gated by **G23** (`depvalue_not_leakable`): a test that would compile
  only if the borrow/no-engine-access constraint were removed must fail to compile as written.

### 4.2 Contract C2 — the shared executor callback

ONE executor, called identically by the cold path (`run_one_target`) and the warm engine
closure (the `add_action` body). RULE 3 / RULE 5 fix. Today the executor returns
`io::Result<RunResult>` where `RunResult { exit_code: i32, cached: bool, outputs: Vec<PathBuf> }`
(`razel-exec/src/lib.rs:23-27`, `build_action`/`build_action_in` at `lib.rs:123-163`) — it has
**no manifest** (just paths) and reports failure via `io::Result`. C2 migrates this to an
explicit outcome + manifest (review 55 P2):

```rust
// crates/razel-exec/src/lib.rs

/// Execute one action: cache hit → restore (0 work); miss → sandbox + run + capture + store.
/// Returns the output Manifest (so the engine's output-level early cutoff can track change)
/// and an explicit outcome. Action failures push to `errors` and return Failed — the boundary
/// itself does not return io::Result (transport/IO bugs still panic-or-Err internally).
pub fn execute_action(
    action: &Action,                 // argv, inputs:{path->Digest}, env, tools, platform, outputs
    cache: &Cache,                   // content-addressed; key = action.content_key()
    exec_root: &Path,
    sandbox: &mut Sandbox,           // caller-injected: transient (cold) | persistent (warm)
    errors: &Rc<RefCell<Vec<String>>>,
) -> ExecOutcome;

pub enum ExecOutcome {
    Cached(Manifest),    // served from cache, actions_executed += 0, action_cache_hits += 1
    Executed(Manifest),  // ran in sandbox, actions_executed += 1
    Failed,              // message pushed to `errors`
}
```

**Manifest semantics (frozen — review 55 P2):**
- **File output:** `(normalized_path, blake3(content))`.
- **Directory output:** `(normalized_dir_path, deterministic_tree_hash)` — the same recursive
  tree-hash `digest_input` applies to input directories (`exec_root.rs:51-67`), so a generated
  dir consumed downstream compares identically.
- **Absent declared output:** **omitted** from the manifest (not a sentinel, not an error) —
  matches the current executor, which intentionally skips absent declared outputs
  (`razel-exec/src/lib.rs:67-89`). G13 covers a directory output and an intentionally absent
  declared output.
- **Canonicalization:** sorted by path, normalized, unique (per C1 `Manifest`).

> **BLOCKER for WS-B (C2-MANIFEST-RETURN-001, P1 — rev3).** `execute_action`, `ExecOutcome`, and
> `Manifest` **do not exist yet** in `razel-exec/src/lib.rs` — today it has only `build_action`
> (`lib.rs:123`) / `build_action_in` (`lib.rs:132`) returning `io::Result<RunResult>` where
> `RunResult { exit_code, cached, outputs: Vec<PathBuf> }` (`lib.rs:23-27`) — no manifest, no
> outcome enum. These frozen types must be **defined and compiling before WS-A lands** (the §4.4
> `c2_outcome_shape` typecheck depends on them). WS-B may stub the body (e.g. always `Failed`) but
> the signatures + types must be present and used by the existing `build_action` callers as thin
> wrappers, so neither WS-B nor WS-C is blocked on a missing type.

#### 4.2b Manifest wire canonicalization (frozen — SR-C2-MANIFEST-ENCODING-1, P1)

A `Manifest = Vec<(String, Digest)>` is naturally insertion-ordered, not key-ordered. Two actions
producing the same outputs in different emit orders would yield different manifest digests, breaking
output-level early cutoff and cross-implementation parity. Frozen: the manifest is **sorted-by-path
at construction AND wire-serialized in sorted-by-path order** — the Rust impl sorts before encoding;
the `tautc`-generated codecs must preserve it. A wire golden test (§4.4 `c2_manifest_canonical_wire`)
encodes a reverse-sorted input `{("b",d1),("a",d2)}`, asserts the bytes match a forward-sorted golden
vector, and that decode + re-encode is byte-identical. Gated by **G22** (`manifest_canonical_across_implementations`):
encode a multi-output manifest in both the Rust and (tautc-generated) TS codecs, assert byte-equal
CBOR. (RazelCodingRules.md RULE 22 — canonical keys + deterministic encoding.)

Body = today's `build_action`/`build_action_in` (`razel-exec/src/lib.rs:123-163`) merged with
the warm-path error handling in `incremental.rs:224-244`, now producing a `Manifest` instead
of `Vec<PathBuf>`. Sandbox lifecycle calls unchanged: `sync_inputs` → `prepare_outputs` →
`run` → `capture_outputs`. Caller owns sandbox lifetime: cold path injects a transient
sandbox; the warm daemon injects a persistent `Sandbox` keyed by `action.content_key().to_hex()`
(reused across builds; `sync_inputs` is a no-op delta when the input *set* is unchanged).
`build_action`/`build_action_in` stay as thin wrappers during migration (WS-B).

### 4.3 Contract C3 — daemon RPC / thin-client protocol (forward raw args; parse server-side)

The wire stays TAUT/CBOR over UDS (`RazelPublicSurfaces.md` §4). The build request carries the
**raw razel arg tokens + the client working directory** — **not** the in-memory `GlobalFlags`,
and **not** a typed per-flag DTO. Two reasons:

- `GlobalFlags` (`crates/razel-loading/src/state/flags.rs:11-60`) is **not wire-safe**: it holds
  `sched_hook: Option<SchedHook>` (line 34, a process-local test seam), `crate_lock:
  Option<Arc<CrateLock>>` (line 60, daemon-derived from `MODULE.bazel.lock`), and
  `fetched_external_base` (line 25, the materializer output). It cannot go on the wire.
- A hand-curated typed `BuildOptions` message (one wire field per flag) would be a **denormalized
  copy of the flag set** — a `RazelCodingRules.md` boilerplate violation that drifts: every new
  flag would need editing in `GlobalFlags`, the message, the two-way conversion, AND the field
  numbering. It is also necessarily incomplete (a handful of fields vs. dozens of build-affecting
  flags). And it is **not the bazel model:** bazel's client forwards the command line
  (`RunRequest.arg` is `repeated string`) and the *server* parses it; the client never learns
  what a flag means. (rev2 floated this typed DTO; rev2 corrects it.)

So the wire is **flag-agnostic** — it carries what the user typed, and the daemon parses it with
the **one** parser that is the single source of truth:

```python
# crates/razel-wire/wire/razel.taut.py — extend the methods; NO new per-flag message, no Define
method("build", role="in",
       params=[("args", List(STR)),   # raw razel tokens (build options + target), as received
               ("cwd",  STR)],        # client working dir → resolve -C/--external_base/--disk_cache + relative targets
       out=Ref("BuildResult")),
method("run", role="in",
       params=[("args", List(STR)), ("cwd", STR), ("run_args", List(STR))],  # run_args = the `-- …` passthrough
       out=Ref("InvocationStarted")),
# (+ an "env" List(EnvVar) param added later ONLY if a flag such as --action_env requires it)
```

> **BLOCKED — C3 wire schema not yet updated (C3-WIRE-001, P1 — rev3).** The schema above is the
> DESIRED shape. `crates/razel-wire/wire/razel.taut.py:133-134` today is
> `method("build", role="in", params=[("target", STR)], out=Ref("BuildResult"))` — only
> `target: STR`; there are no `args`/`cwd` params. WS-E (WIP) must update `razel.taut.py` to add
> `args: List(STR)` and `cwd: STR`, run `tautc`, and commit the generated codecs **before
> `parse_opts` moves to `razel-loading`**. The Wave-0 compile-check must include the §4.4
> `c3_args_roundtrip` CBOR test so the schema/parser skew is caught before WS-A lands.

**Routing-vs-build flag separation (C3-ROUTING-002 — rev3).** `parse_opts`
(`razel-cli/src/lib.rs:414-490`) consumes ALL flags uniformly through one `dispatch` loop
(`dispatch(&mut o, spec, value)` near `lib.rs:478`) into a single `Opts` struct (`lib.rs:208`) that
mixes routing fields (`daemon`, `socket`, `workspace`) with build fields (`copts`, `linkopts`,
`jobs`). There is **no `build_args: Vec<String>`** that isolates the forwardable subset. WS-E must
refactor `parse_opts` to also yield `build_args` — the raw tokens that are NOT routing flags
(recognized via the `RAZEL_FLAGS` table at `lib.rs:282` + `BAZEL_FLAGS`). The CLI forwards only
`build_args` to the daemon, never `--daemon`/`--socket`/`--cbor`/`--batch` tokens. The §4.4
`c3_parse_equivalence` test asserts the daemon's `parse_opts(build_args).resolve(cwd)` ==
the CLI's `parse_opts(full_args).resolve(cwd)` with routing fields zeroed.

**CWD / `-C` resolution spec (C3-CWD-RESOLUTION-001 — rev3).** RG-0010 absolutizes the workspace
client-side (`lib.rs:484-490`: `if o.workspace.is_relative() { canonicalize }`). The client forwards
its **actual `cwd` before absolutization**; the daemon's `parse_opts(args).resolve(cwd)` is defined
as: if `-C` is present, absolutize by `cwd.join(c_value).canonicalize()`; if absent, canonicalize the
bare `.` relative to `cwd`. Client and daemon use the **same** `fs::canonicalize` + symlink logic, so
`--batch` and `--daemon` resolve byte-for-byte identically (no symlink/trailing-slash divergence) and
relative `//pkg:name` targets resolve to the same root. `c3_parse_equivalence` runs a relative-`-C`
case and asserts the resolved paths are identical.

**The single-source-of-truth move (required).** `parse_opts`/`global_flags()`
(`razel-cli/src/lib.rs:414`, `:237`) move out of `razel-cli` into the crate where `GlobalFlags`
already lives (`razel-loading`). One parser then serves both the CLI's `--batch` in-process path
and the daemon's forwarded args. Adding a flag touches **only** that parser — zero wire churn,
nothing to keep in sync. (`razel-loading` does not depend on `razel-wire` today and need not:
`args`/`cwd` are plain scalars/lists, so there is no `tautc`-regen burden either.)

**The daemon owns the daemon-derived fields.** The actor runs `parse_opts(args)` → `GlobalFlags`,
resolving relative paths against `cwd`, then fills `sched_hook = None`, `crate_lock` (from the
workspace lockfile), and `fetched_external_base` (the materializer) **itself, before analysis**.
`bin_tree_layout` is whatever the parsed args yield (== the CLI's value), so
`GlobalFlags::default()` and the `rpc.rs:358` pollution are gone.

The structured `EngineCommand::Evaluate` (`RazelDepsEngineV2.md`) is built by the actor *after*
parsing — it stays **internal** to the daemon. The wire is the argv/cwd intake
(`RazelPublicSurfaces.md`:134 "argv/env intake"); the engine seam stays structured. No contradiction.

```rust
// crates/razel-daemon/src/rpc.rs — Server::do_build serves via the WorkspaceActor:
//   actor.enqueue(Build { args, cwd, reply })
//   actor: GlobalFlags = parse_opts(args).resolve(cwd) + daemon-derived; then engine.request(target)

// Client (CLI) auto-spawn — forwards the post-routing tokens; parses NOTHING build-related:
//   try rpc::call(default_socket(ws), Build{ args, cwd })
//   └─ ConnRefused → spawn `razel daemon` detached → backoff-retry connect
//   └─ outlock (rpc.rs:137) live → connect; dead → reap + respawn
//   └─ hello version skew → restart the daemon, wait, reconnect (Bazel restart semantics)
```

**Cross-platform (review 25 P2).** Transport is UDS on unix, **loopback TCP on Windows**
(`rpc.rs:132-133`) — same protocol over both. Auto-spawn/backoff/reap is identical; the only
platform split is the version-skew restart mechanism: **SIGTERM on unix, process-kill on
Windows** (no SIGTERM). The socket path is `default_socket(workspace)`
(`razel-cli/src/lib.rs:492-494`, `ws/.razel-daemon.sock`); the workspace lock is
`razel-daemon::outlock` (`RazelPublicSurfaces.md` §1b — the JSON-line `workspace.lock`).

Because the client auto-spawns and restarts the daemon on version skew, client and server always
run the **same** parser version — forwarding raw args is safe (no cross-version flag interop).
Tests: an `args`/`cwd` CBOR round-trip golden, and a **parse-equivalence** test (the daemon's
`parse_opts(args).resolve(cwd)` == the CLI's in-process `GlobalFlags` for the same input, modulo
daemon-derived fields). Bump the hello protocol number so a skewed client is rejected (G10).

#### 4.3b CLI thin-client vs programmatic client (SR-THIRD-PARTY-CLIENT-TENSION-1 — rev3)

C3's raw-args forwarding is the **CLI's** intake layer. `RazelPublicSurfaces.md` §1 names gryth /
third parties as clients of the SAME server; a programmatic client must NOT have to construct razel
CLI arg strings (stringly-typed, error-prone). Resolution: the daemon's `Command` service exposes
**two request types, one handler**:
- `BuildRequest { args: Vec<String>, cwd: String }` — the CLI path (parse once → `GlobalFlags` +
  target → build the internal `EngineCommand::Evaluate`).
- `BuildCommandRequest { command: EngineCommand }` — the programmatic path (a pre-structured
  `EngineCommand` per `RazelDepsEngineV2.md`, same CBOR/taut wire, NO second implementation).

Both converge on one internal `EngineCommand::Evaluate`. This does NOT change Wave 0 or WS-E's CLI
work; it documents the public S-B surface so gryth's known use case is supported without a denormalized
second parser.

#### 4.3c Wire snapshot contract (SR-SNAPSHOT-REVISION-MAPPING-1 — rev3)

Freeze `SnapshotId = u64` == the per-daemon monotonic `BuildState.revision` (`rpc.rs:72,100,433`),
epoch-disambiguated by the daemon start time recorded in the on-disk `daemon.json` (§3.7) so revisions
do not alias across daemon restarts. Subscribers use the revision as the `SnapshotId` for a
`Query(SnapshotId)` (RazelDepsEngineV2.md). No new wire field is required if `revision` IS the
SnapshotId; if disambiguation is needed on the wire, `BuildState` gains an optional `daemon_epoch`.
Gated by **G19** (`snapshot_revision_monotonicity`): concurrent builds emit monotonically increasing
revisions, no gaps or repeats.

#### 4.3d Snapshot semantics (SR-COMMITTED-SNAPSHOT-DEFINITION-1 — rev3)

A **committed snapshot** is a point-in-time **immutable** view of the actor's engine state, indexed by
`SnapshotId` (== revision). It conceptually includes the input digests + their update revision, the
computed node values (targets/deps/action results), and the causality (which input change produced it).
The wire `BuildState` is a **projection** of the snapshot (the build-result portion only). Queries
(RazelPublicSurfaces.md §2 S-B Query service) operate on committed snapshots, never on live state. The
on-disk content cache (`razel-exec`) is indexed by `action.content_key()`, NOT by snapshot — outputs
survive snapshot boundaries. Gated by **G20** (`snapshot_immutability`): requesting the same snapshot
twice yields byte-identical results. (RazelDepsEngineV2.md REQ-DEPSV2-005/006.)

### 4.4 Wave-0 compile-check contract test (frozen-means-frozen)

Before Wave 1 opens, a `#[cfg(test)]` contract test in `razel-engine` (and a wire round-trip
test in `razel-wire`) **type-checks the frozen signatures with stub bodies** so "frozen" is
mechanical, not aspirational (review 25 P1):

- `c1_signatures_typecheck`: constructs an `Engine`, calls `add_input`/`add_derived`/
  `add_action`/`set_input`/`request` with the frozen signatures and a no-op closure; asserts
  `NodeValue::digest_equal` and the `DepValue` shape (owned `key: String`, `value: NodeValue` — rev3
  C1-DEPVALUE-001, NOT a borrow) compile.
- `c1_named_deps_not_lost` (the anti-regression for review 55 P1): an `add_action` whose
  producer emits **two** outputs and a downstream `add_action` that consumes **one** of them
  by path; asserts the consumer reconstructs the correct `(path, digest)` — **fails if the
  contract is ever narrowed back to a flat `&[Digest]`**. (This is also gate G15.)
- `c2_outcome_shape`: matches all three `ExecOutcome` variants + `Manifest`.
- `c2_manifest_canonical_wire` (SR-C2-MANIFEST-ENCODING-1): encode a reverse-sorted manifest;
  assert the bytes match a forward-sorted golden; assert decode+re-encode is byte-identical.
- `c3_args_roundtrip`: CBOR encode/decode the `args`/`cwd` build request, equal in == out;
  golden bytes pinned. Plus `c3_parse_equivalence`: `parse_opts(args).resolve(cwd)` ==
  the CLI's in-process `GlobalFlags` (modulo daemon-derived fields).
  > **Implementation note (PAR-8 — rev3).** `c3_parse_equivalence` is a snapshot test that must be
  > **authored in Wave 0** (it does not exist yet), living in `razel-loading/src/state/flags.rs`
  > with `parse_opts`. `GlobalFlags` has ~20 fields (`flags.rs:11-60`) — round-trip a 3-case fixture
  > (no args; build+test flags; complex `-C`/`--copt`) through `parse_opts` + `resolve`, asserting
  > the semantic fields (`options_digest`, `bin_tree_layout`, `copt`, `linkopt`, `jobs`, …) and
  > **excluding** the daemon-derived `sched_hook`/`crate_lock`/`fetched_external_base`. Update the
  > snapshot when `GlobalFlags` grows a field.

---

## 5. Work-streams

Each stream consumes/provides a **frozen** contract from §4, is **independently buildable +
testable** (per-stream cargo target + its own tier-gated test), and touches a (mostly)
disjoint file set. The one unavoidable coupling — the **WS-A → WS-C compile break** — is
called out explicitly and scheduled (it is NOT a parallel stub-only stream).

### WS-A — Engine value/compute change (provides C1)

- **Scope:** widen node value to `NodeValue`; widen `ComputeFn` to receive `&[DepValue]` and
  return `Result<NodeValue, ComputeError>`; add `add_action` beside `add_derived`; add
  `NodeValue::digest_equal`; keep the algorithm at `lib.rs:112-176` byte-identical in behavior
  (only the value type + the `lib.rs:169` cutoff comparison change).
- **Files:** `crates/razel-engine/src/lib.rs` (only).
- **Consumes:** nothing. **Provides:** C1.
- **Deliverable:** `razel-engine` compiles; the six existing tests (`lib.rs:202-284`) pass
  re-expressed with `NodeValue` *and* the new manifest-variant tests (`pure_node_early_cutoff`,
  `action_node_manifest`, `action_output_early_cutoff`,
  `incremental_equals_from_scratch_with_manifests`) pass; the C1 compile-check tests (§4.4) pass.
- **COMPILE BREAK NOTE:** widening `ComputeFn`/`add_derived` will **not compile** against
  today's `incremental.rs` (which uses `add_derived(.., |_| Digest)` at `incremental.rs:159`).
  WS-C lands the fix; WS-A and WS-C are sequential on this seam (§6.1).
- **Independently testable:** `cargo test -p razel-engine`.

### WS-B — Shared executor callback refactor (provides C2)

- **Scope:** extract `execute_action` (C2) from `build_action`/`build_action_in`; return
  `ExecOutcome` + `Manifest` (file/dir/absent semantics §4.2); keep `build_action`/
  `build_action_in` as thin wrappers; no behavior change.
- **Files:** `crates/razel-exec/src/lib.rs` (add `execute_action`, `ExecOutcome`, `Manifest`
  capture), `crates/razel-exec/src/sandbox.rs` (interface unchanged).
- **Consumes:** nothing (defines C2). **Provides:** C2.
- **Deliverable:** `razel-exec` compiles; existing cache/sandbox tests stay green; new
  `execute_action` unit tests (cache-hit → `Cached`, miss → `Executed`, failure → `Failed`)
  + the §4.2 file/dir/absent manifest cases (feeds G13).
- **Independently testable:** `cargo test -p razel-exec`.

### WS-C — Migrate `IncrementalBuilder` to C1+C2; make it workspace-label capable (consumes C1+C2)

> **NOT greenfield (both reviews P1).** `IncrementalBuilder` (`crates/razel-build/src/incremental.rs`)
> already maps files→input nodes, actions→derived nodes, target→derived node, with
> persistent sandboxes (`incremental.rs:105-169`), and has **differential + warm==cold tests**
> (`incremental.rs:267-359`). It is exported (`razel-build/src/lib.rs:13`) but **never called**.
> The work is *migration*, not invention.

- **Scope:**
  1. Migrate every `add_derived(.., |_| Digest)` action node to `add_action(.., |deps| -> Result<NodeValue,_>)`
     calling `execute_action` (C2) — fixing the WS-A compile break.
  2. **Delete the duplicate `fs::read` re-digest** in `run_action` (`incremental.rs:210-214`):
     action-input digests now come from the **named `DepValue`s** (C1), not a re-read of the
     filesystem (review 55 P1) — for a generated input, the digest is read out of the upstream
     action's `Manifest` by path.
  3. Make it **workspace-label capable:** `configure` consumes the cached
     `analyze_workspace_with` projection for `//` labels (not just a single BUILD), so the
     daemon can own one graph for `//:razel`.
  4. Keep `sync_file`'s `leaf_inputs` guard (`incremental.rs:177`) — it is the model for the
     daemon's unknown-key safety (C1, §3.5).
- **Files:** `crates/razel-build/src/incremental.rs` (only).
- **Consumes:** C1 (`add_input`/`add_action`/`request`/`DepValue`/`NodeValue`), **C2
  (`execute_action`/`ExecOutcome`)**.
  > **Sequential after WS-A AND WS-B (PAR-6 — rev3).** WS-A widens `ComputeFn` (compile break vs
  > `incremental.rs:159`); WS-B defines `execute_action` (the interface `run_action`,
  > `incremental.rs:201-245`, must call instead of today's `build_action_in`). WS-C migrates
  > `run_action` to the new `execute_action`(C2), fixing BOTH breaks atomically. If either WS-A or
  > WS-B is delayed, WS-C cannot land — the §6.1 DAG's WS-C edge depends on B as well as A.
- **KEEP-GREEN carve-out tests (the warm-path algorithm proof, not just engine units):**
  `rebuild_recomputes_only_the_affected_action` (`incremental.rs:267`),
  `builds_correctly_with_hardlink_materialization` (`incremental.rs:304`),
  `incremental_matches_a_fresh_build_of_the_final_state` (`incremental.rs:333`),
  `builds_under_seatbelt_isolation` (`incremental.rs:362`, macOS). These must stay green across
  the migration (re-expressed with `NodeValue` where they assert values).
- **Deliverable:** `razel-build` compiles with C1; the four keep-green tests pass; a new
  workspace-label fixture test proves a `//`-style multi-target graph recomputes O(affected).
- **Independently testable:** `cargo test -p razel-build incremental`.

### WS-D — WorkspaceActor: warm-engine ownership + persistent exec-root + watcher + rescan + flags (consumes C1+C3, WS-C)

- **Scope:** the **WorkspaceActor** (§3.5 / concrete in §3.5a) owns one `Engine` + the migrated
  `IncrementalBuilder` + persistent-sandbox map + **persistent exec-root** (§3.6a) + **cached
  analysis/projection** (§3.6b) + the watcher + the startup rescan. Concretely:
  - **Actor queue:** RPC threads + watcher ENQUEUE `Build`/`SetInput`/`Rescan`/`Shutdown`; only
    the actor mutates engine state (§3.5). `build.subscribe`/`invocation.events` get committed
    snapshots.
  - **`do_build`** enqueues `Build { args, cwd }`; the actor runs `parse_opts(args)` →
    `GlobalFlags` (resolved vs `cwd`) + daemon-derived fields (§4.3) and serves via
    `engine.request`. **No `GlobalFlags::default()`** — `bin_tree_layout` now matches the CLI
    (was `false` at `rpc.rs:358`).
  - **Watcher/rescan baseline SEQUENCE (review 55 P1):** analysis/projection establishes known
    leaf inputs FIRST (`Rescan` builds/refreshes the graph), THEN `SetInput` updates known keys.
    A content change to a **known** source → `SetInput` (one digest, guarded by the known-key
    set; never a blind `set_input` — it panics on unknown keys today, `lib.rs:86`). An
    **UNKNOWN file** / **BUILD** / **MODULE** / **lockfile** / **`.bazelrc`** / **`.razelrc`**
    event / a **deletion** → `Rescan` (mark source-universe/graph dirty, re-analyze) — NOT a
    blind `set_input`.
  - **Persistent exec-root + cached analysis (§3.6):** built once, fixed up incrementally;
    not `rm -rf`'d per build; analysis cached and invalidated only by graph-shape inputs.
  - **Startup rescan:** after `outlock` at `serve` startup (`rpc.rs:134-151`), re-establish the
    digest baseline for every known leaf input so a missed inotify event cannot stale a build.
  - **Delete the toy `Workspace`:** `razel_daemon::Workspace` (`lib.rs:44-87`) is repurposed
    *into* the `WorkspaceActor` shape; the standalone toy type + its toy tests are removed
    (migration checklist, §8) so it isn't stranded a second time. The actor is the live owner.
- **Files:** `crates/razel-daemon/src/rpc.rs` (`do_build`, `dispatch`, request decode, `serve`
  startup, the actor + queue), `crates/razel-daemon/src/lib.rs` (delete/repurpose toy
  `Workspace`; the actor owns Engine + watcher + rescan + exec-root + analysis cache).
  - **Cached-analysis invalidation (PAR-4):** implement §3.6b — move the `warm_analyze` cache
    (`Mutex<Option<WarmAnalysis>>`, `rpc.rs:157-172`) into the actor; key by `AnalysisDigest`; on
    `Rescan` mark dirty; on next build recompute the digest and re-analyze only if changed.
  - **Persistent exec-root fixup (PAR-2):** implement §3.6a `fix_up_exec_root(old, new)` (NOT a
    rebuild) + startup validation/repair; never the unconditional `remove_dir_all` (`exec_root.rs:13`).
  - **Watcher wiring (CA-7):** wire `watch()` (`lib.rs:92-106`, currently NOT called by `serve`,
    `rpc.rs:134-150`) into the actor queue, routing SetInput vs Rescan per the §3.5 sequence.
  - **Daemon observability (OPER-6):** write a structured JSON-lines log to `<daemon_dir>/daemon.log`
    (rotate at 100MB; ts/level/invocation_id/key/event); add a `daemon.status` query returning
    (pid, uptime, memory_rss, invocations_active, snapshots_retained); `BuildResult.message` carries
    the action's command + stderr excerpt on failure (daemon-internal errors labeled "razel internal:").
- **CODEABLE (PAR-7 — discharged rev4).** The design that was DESIGN-HEAVY is now concrete in
  §3.5a (actor mechanics), §3.6a (`fix_up_exec_root`/`validate_exec_root`/`handle_rescan`), and
  §3.6b (`AnalysisDigest`/`do_build_impl`). WS-D is now an **implementation** stream against those
  algorithms. Concrete sub-tasks, in landing order (each independently testable):
  1. **D1 — Actor skeleton + reply channel** (§3.5a). Add `ActorMessage`/`RescanReason`/
     `WorkspaceActor` in `rpc.rs` (after `Inner` ~:83); spawn the thread in `Server::serve` (:134);
     refactor `do_build` (:336-407) to ENQUEUE `Build { args, cwd, reply }` + block on the one-shot
     reply (the R-9/CA-1 fix). Move `Inner.warm`/`analyses`/`state`/`bump` ownership into the actor.
  2. **D2 — `do_build_impl` + cached analysis** (§3.6b). `compute_analysis_digest`; the
     fresh-vs-cached branch around `analyze_workspace_resolved` (`drive.rs:58`); `projection.configure`
     (WS-C). Drop `GlobalFlags::default()` (`rpc.rs:358`) — parse the forwarded args (§4.3), so
     `bin_tree_layout` matches the CLI. Surface `engine_recomputes` from the engine (G4).
  3. **D3 — Persistent exec-root** (§3.6a). Add `fix_up_exec_root`/`validate_exec_root`/
     `enumerate_sources` in `exec_root.rs` (~:36/:81); keep `prepare_exec_root` for the cold path;
     drive `exec_root_rebuilds` off `did_change`.
  4. **D4 — Watcher wiring + rescan routing** (§3.5/§3.6a `handle_rescan`, CA-7). Wire `watch()`
     (`lib.rs:92-106`) into the inbox: known leaf → `SetInput`; BUILD/MODULE/lockfile/`.bazelrc`/
     `.razelrc`/unknown/deletion → `Rescan`. Enqueue `Rescan { StartUp }` synchronously at serve
     startup (the missed-event baseline + `validate_exec_root`).
  5. **D5 — Delete the toy `Workspace`** (`lib.rs:44-87`) + its toy tests once D1-D4 own its role
     (migration checklist §8); the actor is the live owner (RULE 4).
  6. **D6 — Observability (OPER-6):** `<daemon_dir>/daemon.log` JSON-lines (rotate 100MB);
     `daemon.status` query (pid, uptime, memory_rss, invocations_active, snapshots_retained);
     `BuildResult.message` carries the failing action's command + stderr excerpt ("razel internal:"
     for daemon-internal errors).
- **Consumes:** C1, C3, WS-C's migrated projection. **Provides:** the warm `do_build` + the
  actor.
- **Deliverable:** daemon builds `//`-labels through the engine over a persistent exec-root;
  2nd no-op = `engine_recomputes == 0` AND `actions_executed == 0` AND `exec_root_rebuilds == 0`
  AND `analysis_runs == 0`; outputs land in `razel-out` (not in-tree).
- **Independently testable:** `cargo test -p razel-daemon` transcript tests against a fixture
  workspace + a fake watcher event source; concurrency test (G11) with two enqueued builds + a
  watcher event.

### WS-E — CLI thin-client + auto-spawn + arg-forwarding (consumes C3)

- **Scope:** flip the dispatch (`razel-cli/src/lib.rs`) from in-process-default to
  **daemon-by-default**; in-process becomes the opt-in `--batch` (CI/strict). Implement
  auto-spawn (`lib.rs:1283-1296` today only prints "start one with: razel daemon"): connect →
  on refusal spawn `razel daemon` detached, backoff-retry; on hello skew restart (SIGTERM unix
  / kill Windows) + reconnect. **Forward the raw post-routing arg tokens + `cwd`** in the
  request (§4.3) — the client parses NOTHING build-related; never the whole `GlobalFlags`.
  **Move `parse_opts`/`global_flags()` to `razel-loading`** so the daemon and the `--batch`
  path share one parser (the single source of truth).
- **Parse-entry-point audit (PARSE-OPTS-DEPS-001, PAR-3 — rev3): WS-E must move ALL of these to
  `razel-loading/src/state/flags.rs`** (not just `parse_opts`), together with their dependencies:
  (i) `parse_opts` (`razel-cli/src/lib.rs:414`); (ii) `parse_opts_with_rc` (`lib.rs:548`);
  (iii) `global_flags` (`lib.rs:237`); plus the helpers `resolve_long` (`lib.rs:384`),
  `dispatch` (`lib.rs:400`), the `FlagSpec` machinery + the `RAZEL_FLAGS`/`BAZEL_FLAGS` tables
  (`lib.rs:282`, `mod bazel_flags` at `lib.rs:36`), and the `Opts` struct (`lib.rs:208`). The
  `Opts` struct stays CLI-scoped (it keeps routing fields); `GlobalFlags` is a separate conversion
  on top. The shared `bazel_flags` table becomes the single source of truth for both the CLI and the
  daemon. **Constraint: `razel-loading` must NOT depend on `razel-wire` or `razel-daemon`** (layering).
  A `razel-loading` unit test (NOT deferred to WS-F): `parse_opts(args) + resolve(cwd)` on a 3-case
  fixture matches the expected `GlobalFlags` semantically — this lands WITH WS-E (RULE 19: no stranded
  red), confirming the move before WS-F's integration test runs.
- **Files:** `crates/razel-cli/src/lib.rs` (dispatch flip, `daemon_call`, `--batch`, forward
  args+cwd), `crates/razel-loading/src/state/` (the moved `parse_opts`/`global_flags` + helpers +
  flag tables + the `parse_opts_snapshot` unit test).
- **Consumes:** C3. **Provides:** the thin client.
- **Deliverable:** `razel build X` (no flags) routes to the daemon, auto-spawning it;
  `--batch` takes the in-process path verbatim and keeps every existing `cli_build.rs` test
  green.
- **Independently testable:** `cargo test -p razel-cli` flag-parse + dispatch-routing tests.

### WS-F — Validating-test/gate stream (consumes ALL contracts; counters FIRST, then gates)

- **Scope:** (1) wire the **counter split** (§7.1) BEFORE any gate — it is the metric-honesty
  fix and the highest-risk item; (2) author every gate in §7 (red-first where new; **extend**
  where an existing test covers it — labeled honestly); (3) slot each gate into its tier in
  `BUILD.bazel //:test_all`, and **un-ignore + non-manually wire** the no-op gate (G1).
- **Files:** `crates/razel-build/src/drive.rs` + `crates/razel-build/src/exec_root.rs` +
  `crates/razel-exec/src/lib.rs` + `crates/razel-daemon/src/rpc.rs` (counter hooks),
  `crates/razel-build/tests/dogfood_selfhost.rs`,
  `crates/razel-build/tests/incremental_differential.rs` (new),
  `crates/razel-daemon/tests/transcript.rs`, `crates/razel-cli/tests/cli.rs`, `BUILD.bazel`.
- **Counter integration points (PAR-1 — rev3):** WS-F wires the counter increments at three call
  sites, all keyed on the SAME `ExecOutcome` variants (C2): (i) the `execute_action` call in
  `run_one_target` (cold path, `exec_root.rs:110`); (ii) the `run_action` site (warm path via WS-C,
  `incremental.rs:201`); (iii) the actor's `execute_action` call in WS-D's engine closure.
  `input_digest_reads` is hooked at `digest_input` (`exec_root.rs:51-67`, called in the loop at
  `exec_root.rs:98`); `exec_root_rebuilds` at `prepare_exec_root` (`drive.rs:69`). The counters are
  opaque to WS-B; WS-F instruments the call sites AFTER WS-B lands `execute_action`. **WS-F has a
  micro-dependency on WS-B's `execute_action` signature — it is NOT a fully parallel-safe fan-out in
  isolation** (see §6.1). If the signature drifts from C2, WS-F resolves the conflict in-line (not a
  separate stream).
- **Consumes:** C1, C2, C3. **Provides:** the counter struct + the regression-prevention suite.
- **Deliverable:** the counters land first (green); every §7 gate exists, red-or-extended as
  labeled, each goes green when its stream lands, all wired into a tier.
- **Independently testable:** the counter unit tests + the gate files compile/run against Wave-0
  fixtures.

### WS-G — `run` / `test` / patterns / multi-target through the daemon (FOLLOW-ON, Wave 3)

> **Explicit scope boundary (review 55 P2).** This plan's CORE delivers **single-target
> `build`** of a concrete label and the `//:razel` alias. `run`, `test`, build-patterns
> (`//...`, `:all`), and multi-target builds are **follow-on**, with their own gates. Stating
> this keeps the "CLI forwards every command" north-star from being silently under-delivered.

- **Scope:** route `cmd_run` (`razel-cli/src/lib.rs:707-742`, today `local_build` in-process)
  and `do_run` (`rpc.rs:247-289`, today builds via `do_build` with target only) through the
  daemon, **carrying the same `args`/`cwd`** (frozen in §4.3); same for `test` and pattern/multi-target
  expansion (expand server-side).
- **Consumes:** C3 (the `run`/`test` request shapes are already frozen in §4.3).
- **`test`/`run` exit-code + result semantics (OPER-5 — rev3):** `test`/`run` forward args/cwd like
  `build` (C3); the daemon streams test-specific events (per-test pass/fail) alongside progress, and
  the final `InvocationEvent.result` carries a `test_summary` (passed/failed/skipped) + an `exit_code`
  (Bazel: 0 pass / 3 build-ok-tests-fail / 1 build-fail). **The wire does not yet carry per-test
  results or a test exit_code — §5 WS-G must freeze that contract before landing.**
- **Deliverable + gates:** `razel run X -- args` and `razel test //...` route to the daemon and
  carry options; a gate asserts `run`'s request carries identical `args`/`cwd` to `build`; a gate
  asserts `razel test X` via daemon exits with the same code as `razel test X --batch`.
- **Independently testable:** `cargo test -p razel-daemon` run/test transcript tests.

---

## 6. Parallelization plan

### 6.1 Dependency DAG (corrected for the WS-A→WS-C compile break)

```
                 ┌──────── C1 (Engine API: named deps + manifest) ───┐
   Wave 0  ──────┤  C2 (executor callback + manifest)                │  ← FROZEN seams + compile-check (§4.4)
                 │  C3 (args/cwd wire + actor msgs + parser move)   │     COUNTER SPLIT scaffold (§7.1)
                 └───────────────────────────────────────────────────┘
                        │            │             │            │
        ┌───────────────┼────────────┼─────────────┼────────────┘
        ▼               ▼            ▼              ▼
   Wave 1  WS-A(C1)  WS-B(C2)   WS-E(C3)     WS-F counters-first + gates red/extend
        │               │
        └───────┬────────┘
                ▼
            WS-C  ← SEQUENTIAL after A+B: widening ComputeFn does NOT compile against
                    today's incremental.rs until WS-C migrates it (both reviews P1).
                    NOT a parallel stub-only stream.
                ▼
            WS-D  (WorkspaceActor: C1+C3+WS-C; persistent exec-root + cached analysis)
                │
        ┌────────┴─────────────────────────────────┐
        ▼                                           ▼
   Wave 2  integration + turn WS-F gates GREEN  +  flip CLI default (WS-E land)
                ▼
   Wave 3  WS-G (run / test / patterns / multi-target through the daemon)
```

- **Serial critical path (short):** freeze §4 (C1, C2, C3) + the §4.4 compile-check + the §7.1
  counter scaffold. That is the only serialized step.
- **Concurrent fan-out:** WS-A, WS-B, WS-E, WS-F start the instant the contracts freeze.
- **The one real coupling:** **WS-C is sequential after WS-A+WS-B** — widening `ComputeFn`
  breaks `incremental.rs` compilation AND `run_action` (`incremental.rs:201-245`) must call WS-B's
  `execute_action`(C2), so WS-C *is* the fix for both and cannot be a parallel stub. WS-D
  follows WS-C. (rev2 correction; the rev1 DAG drew WS-C as parallel-in-Wave-1 — wrong. rev3,
  PAR-6: the WS-B→WS-C edge is now explicit, not just WS-A→WS-C.)
- **Micro-dependency (rev3, PAR-1):** WS-F's counter wiring depends on WS-B's `execute_action`
  signature (the cache-hit/miss outcome it counts), so WS-F is not fully isolated — it wires its
  hooks after WS-B lands.

### 6.2 Wave table

| Wave | Work | Workers | Gate to advance |
|------|------|---------|-----------------|
| **0** | Freeze C1, C2, C3 (§4, real signatures). §4.4 compile-check tests green. **§7.1 counter split scaffold** (the 6 counters; `BuildReport`/wire fields). Fixtures: 2-target graph (WS-C), 5-target DAG (differential), `//app:lib+bin` (3-way parity), a 2-output→1-consumer fixture (G15). | 1 (lead) | §4 signatures merged; §4.4 + counter tests green; fixtures compile. |
| **1** | WS-A · WS-B · WS-E · WS-F(counters-first, then gates) — concurrent. | 4–5 | Each stream: own crate compiles + own unit tests green; counters green; WS-F gates red/extend as labeled in §7. |
| **1.5** | **WS-C** (sequential after A+B land) — migrate `IncrementalBuilder` to C1+C2; keep-green carve-out (§5 WS-C). | 1 | `razel-build` compiles with C1; the 4 keep-green tests + the workspace-label test pass. |
| **2** | WS-D (WorkspaceActor, persistent exec-root, watcher, rescan, flags) → WS-E flips default → WS-F gates flip GREEN as their stream lands. Delete the RG-0008 cold shim + the toy `Workspace` (§8). | 2–3 | Every §7 gate green at its tier; carve-out byte-identical; `//:test_all` green. |
| **3** | WS-G (run/test/patterns/multi-target through the daemon). | 1–2 | WS-G gates green; `run` carries `args`/`cwd` == `build`. |

### 6.3 Integration points + keep-green discipline

- **Integration points:** (i) C2's `execute_action` → `ExecOutcome`/`Manifest` is the single
  seam shared by cold `run_one_target` and the warm `add_action` closure. (ii) C1's
  `request → NodeValue` + `DepValue` named deps is where WS-A meets WS-C/WS-D. (iii) C3's
  the `args`/`cwd` request is where WS-E meets WS-D. (iv) the actor queue (§3.5) is where RPC threads +
  the watcher meet the engine. All frozen in Wave 0.
- **Keep-green (RULE 19 + tiered-gate):** the carve-out gate stays **byte-identical-green per
  step** — WS-B's `execute_action` is a *pure extraction* (existing `razel-exec` tests pass
  unchanged); WS-C's migration keeps its four carve-out tests green. Per-step gate =
  `cargo test -p <crate> --lib` for the touched crate + its carve-out; `--workspace` only at
  wave tags. No stream lands red.
- **No stranded infra (RULE 4):** the engine + `IncrementalBuilder` stop being parked the
  moment WS-D wires the actor; G4 + G11 prove they are *live*, not merely *present*; the toy
  `Workspace` is deleted so it can't strand again.

---

## 7. Validating tests & gates (the regression-prevention suite)

This is the heart of the document. **The counter split comes FIRST (§7.1)** — it is the
metric-honesty fix the reviews flagged as highest-risk; the gates are meaningless without it.
Then the gate table (§7.2). Tiers follow the `RazelPublicSurfaces.md` §4b pyramid: **Tier-1** =
mandatory-for-release carve-out, daily against the real target; **Tier-2** = `//:test_all`
daily CI; **Tier-2.5** = warm-daemon-specific, mandatory before the daemon becomes default.

### 7.1 Counter split (DO THIS FIRST — the metric-honesty fix)

**The bug the reviews caught.** `BuildResult.recomputes` on the wire is set to
`report.executed` (`crates/razel-daemon/src/rpc.rs:381`), and `BuildReport.executed`
(`crates/razel-build/src/drive.rs:111-117`) is **action cache misses** — *not* engine node
recomputes. The engine's own recompute counter (`razel-engine/src/lib.rs:36,94-99,158`) is a
**different number** and stays internal. So G4 ("daemon is warm") as a `recomputes == 0` check
would **pass on a cold path that re-hashes every input and then gets all-cache-hits**
(`executed == 0`). That is the exact failure class wearing a warm costume. Fix the metric
before the gate.

**Six distinct counters (defined, sourced, frozen):**

| Counter | Means | Source today | Action |
|---------|-------|--------------|--------|
| `engine_recomputes` | Skyframe node recomputes | `razel-engine/src/lib.rs:158` (internal) | **Surface** it via the actor + `BuildReport`. |
| `actions_executed` | action cache **misses** (ran in sandbox) | `BuildReport.executed` (`drive.rs:111-117,196-199`) | Keep; rename the wire field off `recomputes`. |
| `action_cache_hits` | actions served from cache | `ExecOutcome::Cached` (C2, new) | Add via C2. |
| `input_digest_reads` | **content reads for action-INPUT key formation only, per-build** | `digest_input` (`exec_root.rs:51-67`, called in the loop at `:98`) — **no counter yet** | **Add.** Counter hook = a counter param into `run_one_target` (`exec_root.rs:87`), incremented per `digest_input` call at `:98`. Excludes output-artifact reporting (`digest_of`, `rpc.rs:509`) and the duplicate warm re-read deleted by WS-C (`incremental.rs:212`). The watcher's `SetInput` = **exactly one digest per changed path** (counts on edit builds, **zero on no-op**). **Startup rescan (WS-D) is EXCLUDED** (one-time baseline at serve startup, not per-build). Scope (SR-GATE-4): G1 asserts `== 0` on the **2nd build within the same daemon session** (no daemon restart between builds, same socket); daemon-restart scenarios are G9's, not G1's. |
| `exec_root_rebuilds` | `prepare_exec_root` rm+recreate calls | `drive.rs:69` (called when `.razel-crates` exists, `:67`) | **Add** (EXEC-ROOT-REBUILD-COUNTER-001). Pass a `&mut usize` into `prepare_exec_root`; increment once at the start (counts in the cold path; skipped in the warm path when the exec-root is persistent + unchanged). NB: it is not strictly per-build — it fires whenever `.razel-crates` exists, which it does for `//:razel`; it *always* `remove_dir_all`s (`exec_root.rs:13`). Persistent exec-root (§3.6a) drives it to 0. |
| `analysis_runs` | full workspace analyze passes | **two sites** | **Add** (ANALYSIS-RUNS-COUNTER-001). Instrument BOTH: (1) bare-label single-BUILD `warm_analyze` (`rpc.rs:166`, today increments the `analyses` counter); (2) `//`-label workspace analysis `analyze_workspace_resolved` (called in `build_workspace_with`, `drive.rs:58`) — the `//` path bypasses `warm_analyze` entirely (`rpc.rs:354`). Both increment on call; the cached analysis (§3.6b) drives this to 0 for `//` labels on a no-op by caching the analyze RESULT keyed on the graph-shape digest. |

**Wire change (frozen):** **rename/replace `BuildResult.recomputes`** so the wire carries
`engine_recomputes` distinctly (and the others as a `Counters` sub-message). G4 asserts
`engine_recomputes` from the **actor**, never `report.executed`.

`BuildReport` grows a `counters: Counters` field; `Counters { engine_recomputes,
actions_executed, action_cache_hits, input_digest_reads, exec_root_rebuilds, analysis_runs }`.

> **BLOCKER — the wire split must land FIRST (SR-GATE-3, P1 — rev3).** Today the wire carries a single
> `BuildResult.recomputes` (`razel.taut.py` field 3, `razel-wire/.../razel.taut.py:59`) and the daemon
> sets it to `report.executed as i64` (`rpc.rs:381`) — the **action-executed** count, NOT engine
> recomputes. Until (1) `razel.taut.py` adds a `Counters` message with all 6 fields and replaces the
> bare `recomputes`, (2) `tautc` codegen is run + committed, and (3) `rpc.rs` populates all 6 from the
> engine + cache + exec-root tracking, **G4 is unimplementable (it would assert against the wrong
> number)**. This counter-split scaffold is the FIRST Wave-0 deliverable of WS-F, before any gate can
> turn green.

### 7.2 Gate table

| # | Name | Asserts | Mechanism | Location | Tier | Red-first vs Extend |
|---|------|---------|-----------|----------|------|---------------------|
| G1 | `no_op_rebuild_zero_work_real_razel_target` | 2nd build, no edits: `actions_executed == 0` **AND** `input_digest_reads == 0` (counters, NOT wall-clock) | **Two sequential daemon builds over the SAME socket, no daemon restart between them** (SR-GATE-4); read both counters from `BuildResult.counters` (§7.1) — MUST NOT mock/stub them; assert both 0. Builds the fully-**resolved label `//crates/razel-cli:razel`** (NOT the `//:razel` alias string; the actor resolves the alias during Rescan/analysis so resolution is cached and `analysis_runs == 0` holds — SR-GATE-2). Wall-clock ≤ ~0.3s is a **secondary observational metric, not a gate** (SR-GATE-1; flaky under load). **Companion anti-regression** (SR-GATE-5): `cold_path_with_cache_hits_still_re_reads_inputs` — deliberately `digest_input` before each cache restore and assert `input_digest_reads > 0` even on all-cache-hits, proving `actions_executed == 0` alone does NOT imply zero reads. | `crates/razel-build/tests/dogfood_selfhost.rs` (un-ignore + wire non-manually) | **T1** | **EXTEND** the dogfood harness (today `#[ignore]`, `TOP="//crates/razel-cli:razel"` at `dogfood_selfhost.rs:13`, not in `//:test_all`). |
| G2 | `daemon_local_bazel_three_way_action_parity` | daemon == local == bazel: byte-identical action graph + output digests on `//app:lib` | Serialize action graphs to canonical sorted JSON; assert set-equality + output-digest equality. **Bazel reference (SR-GATE-14):** require a Bazel binary (`which bazel` or a fixture path); if absent, fall back to a committed pre-golden graph (`crates/razel-build/tests/data/bazel_graphs/app_lib_graph.json`); if BOTH absent, fail loudly ("Bazel parity gate requires Bazel or pre-golden data"). | `crates/razel-daemon/tests/transcript.rs` | **T2** | RED-first |
| G3 | `output_tree_never_polluted_source_tree` | zero files changed outside `razel-out`/`bazel-out`; `bin_tree_layout==true` by default AND under the daemon (parsed from the forwarded args, not `GlobalFlags::default()`) | Snapshot source-tree before/after; assert no change. **Assert the daemon's resolved `bin_tree_layout` flag VALUE == true (SR-GATE-9)** — read it from `BuildResult` (extend with a `flags_used` field, at minimum `bin_tree_layout`) or a daemon diagnostic; do NOT infer it from output location alone (a `false` default could still land outputs correctly by accident). Fail loudly if the daemon is still on `GlobalFlags::default()`. | `crates/razel-build/tests/dogfood_selfhost.rs` | **T1** | RED-first |
| G4 | `daemon_is_actually_warm` | 2nd daemon build of the same target: **`engine_recomputes == 0`** (from the actor, NOT `executed`) | Drive two sequential daemon builds over UDS; read the **engine** counter via the actor seam; assert the cold shim path is not taken. | `crates/razel-daemon/tests/transcript.rs` | **T2.5** | RED-first (was the metric-conflation bug — §7.1) |
| G5 | `cli_defaults_to_daemon_not_in_process` | `razel build X` no-flag routes to the daemon branch; `--batch` required for in-process | Parse argv; assert dispatch branch. | `crates/razel-cli/tests/cli.rs` | **T2** | RED-first |
| G6 | `warm_cold_differential_incremental_audit` | 5-target DAG: edit ONE source → only the affected subgraph recomputes; unchanged targets keep identical action digests; `engine_recomputes == |affected|` | Build cold (record digests); edit `lib_a`; rebuild warm; assert unchanged digests + recompute count. **Variant `incremental_differential_with_partial_failure` (SR-GATE-6):** 3-action DAG A→B→C; edit A's source; warm build where A FAILS; assert (1) failure surfaced, (2) B/C SKIPPED (not re-executed), digests ABSENT, (3) a 2nd rebuild (no edit) re-runs A (still fails) + still skips B/C — deterministic, no silent false-skip from the sentinel-digest + missing-input path (`incremental.rs:241`, `exec_root.rs:98`). | new `crates/razel-build/tests/incremental_differential.rs` | **T2.5** | RED-first |
| G7 | `action_cache_key_stable_no_op_rebuild` | no-op rebuild → every action's `content_key` identical | Hook the executor to capture keys; build twice; assert key-set equality. | `crates/razel-build/tests/dogfood_selfhost.rs` | **T2** | RED-first |
| G8 | `engine_manifest_invariants` | the six preserved invariants hold for `NodeValue::Manifest`: input+output early cutoff, incremental==fresh, no-op==0, linear, cycle-detect; manifests canonical | The existing `razel-engine` tests (`lib.rs:202-284`) re-expressed with manifest values (C1). | `crates/razel-engine/src/lib.rs` (tests) | **T1** | **EXTEND** (re-express the six existing tests with `NodeValue`) |
| G9 | `file_changed_while_daemon_down_detected_on_reconnect` | a file edited while the daemon is down is detected on restart (digest changes → affected targets recompute) | Build warm; kill daemon; edit a source; restart; assert the startup rescan `set_input`s the new digest and `engine_recomputes != 0`. | `crates/razel-daemon/tests/transcript.rs` | **T2.5** | RED-first (R-8) |
| G10 | `daemon_client_version_skew_rejected` | hello rejects a mismatched wire version, naming **both** client and daemon versions explicitly | **Extend** `hello_handshake_accepts_and_rejects` (`transcript.rs:162-177`, already rejects protocol 999 and asserts the message contains "999"+"protocol", `rpc.rs:223-227`) to assert the text names BOTH `h.build_version` and `me.version`. | `crates/razel-daemon/tests/transcript.rs` | **T1** | **EXTEND** (not red-first — mostly implemented) |
| G11 | `actor_serializes_concurrent_builds_and_watcher` | two concurrent daemon builds + a watcher event → serialized revision order, no panic, deterministic final state | Enqueue 2 `Build`s + 1 `SetInput`/`Rescan` from separate threads; assert monotonic committed revisions + identical final digests across runs. **(CA-3)** two subscribers on `build.subscribe` never see the same revision with different target sets — they receive a byte-identical `BuildState` clone per revision. **(SR-GATE-12, real threading, requires WS-D's queue) `concurrent_edit_during_engine_request`:** spawn thread-A doing a slow `run_action` (enqueue `Build`); thread-B enqueues `SetInput` before it completes; assert both queue, `SetInput` is processed AFTER the build (single-writer FIFO), no panic/corruption. **(CA-4)** 2 concurrent `do_run`s: both invocations appear in `invocation.events` with correct per-invocation seq (interleaving allowed; appends serialized by the `events` Mutex, `rpc.rs:211-215`). **(CA-8)** 3 concurrent `invocation.events` subscribers: all receive every event gap-free per invocation, none starved >1s (Condvar `notify_all` + Mutex contention). **DEPENDS-ON WS-D** (unimplementable until the actor message queue is wired). | `crates/razel-daemon/tests/transcript.rs` | **T2.5** | RED-first (review 55 P1 actor) |
| G12 | `watcher_semantics_known_unknown_delete` | known-source edit → `SetInput` (1 read); unknown-file/BUILD/MODULE edit → `Rescan` (re-analyze, no blind `set_input`); deletion handled; daemon UP and DOWN | Drive a fake watcher event source through the actor; assert message kind per event; assert no panic on unknown keys (guards `lib.rs:86`). **Order constraint (PAR-5):** a `Rescan` populates `leaf_inputs`; a subsequent `SetInput` for a known key succeeds; a `SetInput` for an unknown key panics (expected). **BUILD-edit invalidation (OPER-7):** an edit to a BUILD file routes to `Rescan` and triggers re-analysis (`analysis_runs` increments). **`concurrent_edit_during_engine_request` (SR-GATE-7):** a `SetInput` enqueued while an `engine.request` is in flight is serialized by the queue, not applied mid-compute — no panic, no stale dep_values. | `crates/razel-daemon/tests/transcript.rs` | **T2.5** | RED-first (review 55 P1 watcher) |
| G13 | `manifest_directory_and_absent_output` | a directory output → one `(dir, tree_hash)` entry (== input-dir tree-hash); an intentionally absent declared output → omitted (not sentinel, not error) | C2 unit + an engine round-trip; assert manifest canonical. **Variant `absent_declared_output_consumed_by_downstream` (SR-GATE-10):** A declares `out.txt` but does NOT produce it (omitted from manifest); B consumes `out.txt`. Cold: A runs (misses out.txt), B runs without it. Warm no-op: B recomputes because A's absence changed B's input-set. Assert (1) the absence is detected (recompute > 0), (2) behavior identical to cold — an absent output is an input-set change, not a silent skip. | `crates/razel-exec/src/lib.rs` (tests) + `crates/razel-engine` | **T1** | RED-first (review 55 P2 C2-dirs) |
| G14 | `no_op_skips_exec_root_rebuild_and_analysis` | 2nd no-op `//:razel`: `exec_root_rebuilds == 0` **AND** `analysis_runs == 0`; **AND the invalidation trigger fires** | Counters from §7.1; two no-op daemon builds → assert both 0. **THEN edit the BUILD file; rebuild; assert `analysis_runs > 0` (cache invalidated, SR-GATE-8). THEN rebuild with no further edit; assert `analysis_runs == 0` again (re-cached).** Proves the persistent analysis cache (§3.6b) actually invalidates — a daemon that never re-analyzes would otherwise pass the no-op check while being catastrophically broken. | `crates/razel-daemon/tests/transcript.rs` | **T2.5** | RED-first (both reviews P1 perf) |
| G15 | `c1_path_name_not_lost_two_output_action` | producer emits 2 outputs; downstream consumes ONE by path; correct `(path,digest)` reconstructed; **multi-level re-use** | The §4.4 `c1_named_deps_not_lost` contract test, promoted to a gate. **Fails if C1 narrows back to flat `&[Digest]`.** **Extended (SR-GATE-11):** add a 3rd action C consuming outputs from BOTH A (re-used) and B; assert C's `DepValue` iterator names all inputs so C picks the right output by path, digests correct end-to-end — guards against a positional regression that only bites at 3+ levels. | `crates/razel-engine/src/lib.rs` (tests) | **T1** | RED-first (review 55 P1 C1-lossy) |
| G16 | `watcher_unavailable_falls_back_to_rescan` | when `notify` is unavailable/lossy, builds fall back to per-build full rescan; correctness == cold (no silent staleness) | Force-break the watcher (or simulate `notify::recommended_watcher` Err); verify the fallback per-build rescan runs and the result matches a cold build; assert a daemon warning is logged. (SR-WATCHFS-FALLBACK-1) | `crates/razel-daemon/tests/transcript.rs` | **T2.5** | RED-first |
| G17 | `daemon_crash_recovery_build_green` | a build after a SIGKILL'd daemon restart completes correctly (startup rescan re-baselines; content cache re-used) | Build warm; SIGKILL the daemon; restart; build again; assert green + correct digests (no exec-root clean needed). (SR-OUTPUT-BASE-MODEL-1, OPER-2) | `crates/razel-daemon/tests/transcript.rs` | **T2.5** | RED-first |
| G18 | `multi_workspace_isolation` | two workspaces (overlapping source dirs) open concurrently in one daemon produce independent correct results; no cross-contamination of cached analysis or counters | Two workspace fixtures; concurrent builds + watcher events; assert per-workspace final digests differ and recompute/analysis counters are per-workspace. (SR-MULTI-WORKSPACE-ISOLATION-1) | `crates/razel-daemon/tests/transcript.rs` | **T2.5** | RED-first |
| G19 | `snapshot_revision_monotonicity` | concurrent builds emit monotonically increasing snapshot revisions, no gaps/repeats | Drive concurrent builds; collect emitted `BuildState.revision`s; assert strictly monotonic. (SR-SNAPSHOT-REVISION-MAPPING-1, §4.3c) | `crates/razel-daemon/tests/transcript.rs` | **T1** | RED-first |
| G20 | `snapshot_immutability` | requesting the same committed snapshot twice yields byte-identical results | Query a snapshot by id twice; assert byte-equal. (SR-COMMITTED-SNAPSHOT-DEFINITION-1, §4.3d) | `crates/razel-daemon/tests/transcript.rs` | **T2.5** | RED-first |
| G21 | `exec_root_rebuild_on_corruption` | manual exec-root corruption is detected + repaired on next build | Delete a symlink / replace it with a file in `.razel-exec`; restart daemon; assert the next build repairs it + completes correctly; assert a warning logged. (SR-EXEC-ROOT-CORRUPTION-1, §3.6a) | `crates/razel-daemon/tests/transcript.rs` | **T2.5** | RED-first |
| G22 | `manifest_canonical_across_implementations` | a multi-output manifest encodes byte-equal in the Rust and (tautc) TS codecs, sorted-by-path | The §4.4 `c2_manifest_canonical_wire` golden + a cross-language CBOR byte-equality check. (SR-C2-MANIFEST-ENCODING-1, §4.2b) | `crates/razel-wire` (tests) | **T1** | RED-first |
| G23 | `depvalue_not_leakable` | a `ComputeFn` cannot store a `DepValue` past return or call an `Engine` method | A compile-fail test (`#[compile_fail]`/commented + note): code that would compile only if the borrow/no-engine-access constraint were removed must NOT compile. (SR-C1-DEPVALUE-BORROW-SAFETY-1, §4.1b) | `crates/razel-engine/src/lib.rs` (tests) | **T1** | RED-first |
| G24 | `exec_root_fixup_zero_churn_and_diff` | `fix_up_exec_root` is a TRUE incremental delta: an unchanged source keeps its exact symlink (same inode); a Rescan with no source-set change returns `did_change == false` (so `exec_root_rebuilds` does NOT increment); an added dir gets a symlink, a deleted dir is unlinked, neither touches siblings | Unit-test `fix_up_exec_root` directly (§3.6a): capture a source symlink's `ino()`; Rescan with identical sources → assert same `ino()` + `did_change == false`; add a dir → assert only its link appears; delete a dir → assert only its link is gone. (PAR-2, §3.6a edge cases E1/E2) | `crates/razel-build/src/exec_root.rs` (tests) | **T1** | RED-first |
| G25 | `exec_root_external_symlink_on_demand` | `.razel-crates` materialized on a later build → `external/` symlink created exactly once (None→Some); a subsequent same-deps build reuses it (Some==Some, no re-symlink, same inode); removal unlinks it | Workspace with an external dep; first build materializes `.razel-crates` → assert `external/` symlink live; second build (same deps) → assert same `symlink_metadata().ino()`, `did_change == false`. (§3.6a edge case E3) | `crates/razel-daemon/tests/exec_root_fixup.rs` (new) | **T2.5** | RED-first |
| G26 | `actor_cancel_and_restart_on_watcher_event` | the `Build` reply channel delivers the build result to the CALLER (not the connection thread); a `SetInput`/`Rescan` enqueued (+ the cancel flag set) during a slow in-flight `Build` CANCELS it (the first `engine.request` returns/aborts `Err("cancelled")`) and the actor RE-RUNS so the final reply reflects the POST-edit inputs (cancel-and-restart, bazel parity) | Drive a slow `Build` (sleep-injected `do_build_impl`); from another thread set the cancel flag + enqueue `SetInput` before it replies; assert (1) the engine saw a `Cancelled` then a successful request, (2) the single `Build` reply is the restarted (fresh) result reflecting the post-edit inputs, (3) no panic. Distinct from G11 (which asserts snapshot byte-identity); this asserts the reply-path + cancel-and-restart loop (§3.5a, R-9.5/R-9.6, CA-2). | `crates/razel-daemon/tests/transcript.rs` | **T2.5** | RED-first |
| G27 | `startup_rescan_baselines_before_serving` | the synchronous `Rescan { StartUp }` establishes EVERY leaf-input key + the exec-root forest BEFORE the accept loop binds, so the first `SetInput` after startup cannot hit the `lib.rs:86` unknown-key panic | Start the actor; before any `Build`, drive a `SetInput` for a known source leaf → succeeds (key exists); drive a `SetInput` for an unknown path → panics/loud-shutdown (expected, PAR-5). Assert `analysis_runs == 1` and the exec-root forest exists after startup, with `revision == 1`. (§3.5a startup ordering, CA-6) | `crates/razel-daemon/tests/transcript.rs` | **T2.5** | RED-first |

### 7.3 How each becomes an ENFORCED gate (red-first/extend → tiered)

- **Counters first.** §7.1 lands before any gate; G1/G4/G14 are *defined in terms of* the new
  counters — without the split they would certify a cold path as warm.
- **Honest labels.** RED-first: G2,G4,G5,G6,G7,G9,G11,G12,G13,G14,G15. **EXTEND existing**:
  G1 (un-ignore + wire the dogfood harness), G8 (re-express the six engine tests), G10 (name
  both versions). A reviewer who sees a red-first gate still red knows the warm path is not
  done — *the silent non-implementation is loud.*
- **G1 wiring (both reviews P1/P2):** the dogfood harness is `#[ignore]` and outside
  `//:test_all` (`dogfood_selfhost.rs:73,95`; `BUILD.bazel:34-91`). WS-F **un-ignores** the
  no-op gate, wires it into a **non-manual** Bazel target (promote dogfood or add a `rust_test`
  in `//:test_all`), and documents the `RAZEL_PROCESS_WRAPPER` / self-host prerequisite. Builds
  the **resolved label** `//crates/razel-cli:razel` for stability (the `//:razel` alias resolves
  to it). Without this, G1 is green in cargo while Tier-1 is hollow.
  > **Bazel target name + prereq (PAR-9 — rev3).** Create
  > `rust_test(name="g1_no_op_rebuild_zero_work", srcs=["dogfood_selfhost.rs"], …)` in
  > `crates/razel-build/BUILD.bazel` (or promote the existing dogfood target), remove the `#[ignore]`,
  > drop any `["manual"]` tag, and tag `["razel_self_host"]` so CI includes it selectively. Document
  > the prerequisite in a target comment: `# REQUIRES: RAZEL_PROCESS_WRAPPER + razel on PATH (self-host)`.
- **Tier-1 (G1, G3, G8, G10, G13, G15, G19, G22, G23, G24):** carve-out, mandatory for release,
  daily against the real target.
- **Tier-2 (G2, G5, G7):** `//:test_all`, daily CI — pin the design contracts.
- **Tier-2.5 (G4, G6, G9, G11, G12, G14, G16, G17, G18, G20, G21, G25, G26, G27):** the warm-daemon
  deliverable — **mandatory before the daemon becomes the default build path** (before WS-E's flip
  ships unguarded).

### 7.4 What is now gated that was silently un-gated before

1. **"the daemon is actually warm"** → **G4** (`engine_recomputes == 0` from the actor, NOT
   `executed` — the metric-honesty fix).
2. **"no-op == zero work on the real build"** → **G1** (`actions_executed == 0` AND
   `input_digest_reads == 0`) + **G14** (`exec_root_rebuilds == 0` AND `analysis_runs == 0`).
3. **"daemon == local == bazel"** → **G2**.
4. **"no source-tree pollution + correct flags over the wire"** → **G3** (+ `bin_tree_layout`
   parsed from the forwarded args).
5. **"the engine is single-writer safe"** → **G11** (actor serialization).
6. **"watcher events do the right thing / never panic"** → **G12**.
7. **"C1 never loses output path names"** → **G15**.

None existed before; their absence is *exactly why* the 34s no-op shipped unnoticed.

---

## 8. Sequencing, migration & rollback

1. **Land behind a flag, cold path intact.** WS-A/B/C/D land while the CLI still defaults to
   in-process. The warm path is reachable via `--daemon` and exercised by the Tier-2.5 gates
   against fixtures — but not the default; the cold path is untouched. Rollback = do nothing.
2. **Gate the warm path green on the REAL graph.** G1 + G14 (no-op zero-work + zero exec-root/
   analysis on `//crates/razel-cli:razel`), G4 (warm), G6 (differential), G2/G3 (parity + no
   pollution), G11/G12 (actor + watcher) all green. This is the go/no-go for the default flip.
3. **Flip the default (WS-E).** `razel build X` routes to the daemon (auto-spawn); `--batch`
   preserves in-process for CI/strict and as the rollback lever. Rollback = revert the one-line
   dispatch flip; the daemon stays opt-in.
4. **Delete the RG-0008 shim AND the toy `Workspace`.** Only after the default flip is green in
   daily CI for a full bank: remove the cold `build_workspace_with(... GlobalFlags::default())`
   branch (`rpc.rs:353-359`) **and** delete the standalone toy `razel_daemon::Workspace`
   (`lib.rs:44-87`, repurposed into the actor by WS-D). G2/G3/G4/G11 protect both deletions.
   The `--batch` in-process path remains the documented escape hatch (it is *not* the RG-0008
   shim — it is the sanctioned no-daemon mode per `RazelPublicSurfaces.md` §1).
5. **Wave 3 (WS-G):** route `run`/`test`/patterns/multi-target through the daemon (own gates).

Migration honors `RazelCodingRules.md` RULE 19 (roll-build green-gated; commits ≤3 lines;
commit only when asked) and "keep the cold path until the warm path is gated green."

---

## 9. Risks & open questions

- **OQ-1 (the one real design question): engine-as-EXECUTOR vs
  engine-as-incremental-algorithm-only.** Should the engine node *run the action* (effectful
  `ComputeFn`, C1), or stay a pure `Digest→Digest` graph that only *decides* what to run?
  **Recommendation: the engine IS the executor (effectful node), per the bazel model.**
  Skyframe nodes *are* actions; output-level early cutoff (`lib.rs:169`) only has teeth if the
  node owns the output manifest. The risk (non-deterministic actions → false-skip) is bounded
  by the differential harness G6 + hermetic-env enforcement (`RazelCodingRules.md` F5).
  Reversible: C2's `execute_action` is a free function callable from either topology.
- **R-2 Action determinism (CRITICAL) — NAMED AUDIT.** Same input digests must yield same
  output digests or G6/G1 false-skip. **Action item (WS-F):** audit the rust/cc rules for
  embedded build timestamps / UUIDs / absolute paths, tied to a **parity test** (extend G2 with
  a timestamp-sensitivity assertion); not left "open." Mitigation: hermetic env (F5).
- **R-3 Manifest ordering / path canonicalization.** Outputs must be sorted-by-path and
  normalized (C1/C2 frozen) or `digest_equal` false-rebuilds. Mitigation: G8 + G13 assert
  canonical manifests.
- **R-4 Multi-output dependency naming — RESOLVED in C1.** A downstream action consuming one of
  an upstream's N outputs needs the **path**, not a positional digest. **Fixed:** C1 hands
  `&[DepValue]` (keyed) and upstream actions return a `Manifest`, so the consumer selects by
  path. Gated by G15 (fails if C1 narrows back to flat `&[Digest]`). (Was deferred in rev1 —
  cannot be deferred for a Wave-0 frozen contract.)
- **R-5 Persistence / daemon restart.** The engine graph is in-memory; a restart clears it. The
  startup rescan (WS-D) re-establishes the *input* baseline; the on-disk cache makes the
  *output* recompute cheap. Persisted node values (`EngineAbiDigest`, REQ-DEPSV2-014) are future
  work. G9 proves restart safety with the rescan alone.
- **R-6 Thread budget under the actor model.** `RazelPublicSurfaces.md` §1c caps engine threads
  daemon-wide; this plan adds one resident `WorkspaceActor` per workspace — confirm the
  per-workspace actor stays lazy and shrinks when idle.
- **R-7 BUILD/MODULE graph-shape invalidation (NEW).** A BUILD/MODULE/lockfile/`.bazelrc`/
  `.razelrc` change alters the source universe and graph shape, not just a digest — a blind
  `set_input` is wrong (and panics on a new key, `lib.rs:86`). Mitigation: such events route to
  `Rescan` (re-analyze), and the cached analysis (§3.6) is invalidated by these inputs. Gated by
  G12.
- **R-8 Persistence/restart rescan (NEW).** A file edited while the daemon is down must be
  caught by the startup rescan, not missed. Mitigation: full digest re-baseline at `serve`
  startup. Gated by G9.
- **R-9 Concurrent `do_build` races on engine mutation (CRITICAL — rev3 CA-1; DESIGN RESOLVED
  rev4).** The current code spawns each RPC thread (`rpc.rs:143-148`) and each `do_run` background
  build (`rpc.rs:247`) on its own thread sharing `Arc<Inner>`, with **no actor queue** — violating
  the frozen single-writer boundary (§3.5). Risk: a `Cell`/`RefCell` borrow panic, silent corruption
  of `BuildState.revision` ordering, or a reply arriving mid-mutation. **rev4: the actor is now fully
  specified** (§3.5a: inbox enum, one-shot reply channel, FIFO single-writer loop, startup ordering,
  panic→reply-Err) — the PAR-7 "design-heavy" caveat is discharged; WS-D sub-task **D1** is the fix
  and is the FIRST WS-D item. Until D1 lands, an explicit serialization lock (test-only). Gated by
  G11 (snapshot byte-identity under concurrency) + G26 (cancel-and-restart on watcher event)
  + G27 (startup baseline before serving).
- **R-9.5 Build cancellation on watcher event (DECIDED rev5 — cancel-and-restart, bazel parity).**
  When a watcher `SetInput`/`Rescan` arrives during an in-flight build, the build **cancels and
  restarts** on the updated inputs (Gianni's decision; matches Bazel semantics). The engine support
  is **implemented** (committed `4ea0c60`, razel-engine 12/0): `Engine::set_cancel(Option<Arc<
  AtomicBool>>)` installs/clears a shared cancel flag (None default); `request` checks it between
  node validations (helper `cancelled()`) and aborts with `Err("cancelled")` at the next **action
  boundary** — an in-flight action stays atomic. The actor side (§3.5a) drains the queued
  invalidations, clears the flag, and re-runs `engine.request`; the reply carries the final
  uncancelled result. A cancelled-then-restarted build re-validates from the current revision, so
  warm == cold still holds. Gated by G26 (cancel-and-restart on watcher event) + G11.
- **R-9.6 Staleness window under a long build (largely CLOSED — rev5, CA-5).** Cancel-and-restart
  (R-9.5) closes the build-duration staleness window: a watcher event no longer waits a full build
  duration — it cancels the in-flight build at the next action boundary and restarts on fresh inputs
  (bazel semantics). The **residual** window is just the current atomic action's remaining runtime
  (in-flight actions are never interrupted mid-spawn). **Bounded-livelock note:** a long build can be
  restarted repeatedly if edits keep arriving; bounded in practice by edit frequency and mitigated
  because each restart reuses the action cache + warm digests, so only the changed subgraph re-runs.
  `razel build --batch` remains the no-daemon escape but is no longer the staleness mitigation.
- **R-9.7 Output-name collision in multi-output deps (NEW — rev3, SR-GATE-15).** If two upstream
  actions emit the same output path, a downstream consumer cannot disambiguate by path alone — it must
  declare both as separate deps. The engine does NOT validate uniqueness; rule-author hygiene is
  required. A violation makes the build non-deterministic (the consumer picks whichever was evaluated
  first). Gated by G15 (2-output + 3-level re-use); enforcement is a future analysis check.
- **R-9.8 Daemon memory growth (NEW — rev3, OPER-4).** The actor's in-memory engine state + cached
  analysis + retained snapshots grow with workspace complexity; no memory-pressure eviction in v1.
  Assumption: a single workspace fits in RAM. Mitigation: idle-out (§3.7) frees a whole workspace;
  bounded snapshot retention is future work.
- **R-9.9 Daemon crash + action-output diagnostics (NEW — rev3, OPER-2/OPER-1).** (a) If the daemon
  crashes mid-build (OOM/segfault/panic), the client must detect socket closure/timeout (heartbeat
  ~30s inactivity), print "daemon crashed, reconnecting", and on reconnect rely on the startup rescan
  + retry — not hang forever on an open socket. (b) A failing action's stderr must reach the client
  (§3.7 progress streaming) or failures are unexplainable. Gated by G17 (crash recovery) + G9
  (down→edit→reconnect).

---

## 10. Acceptance / done-criteria

The work is DONE when **all** hold:

1. **No-op is zero-work on the real graph.** `razel build //crates/razel-cli:razel` (2nd invocation,
   no edits) achieves `actions_executed == 0` **and** `input_digest_reads == 0` (**G1**) **and**
   `exec_root_rebuilds == 0` **and** `analysis_runs == 0` (**G14**) — i.e. the persistent exec-root
   + cached analysis are real, not per-build. **The COUNTERS are the acceptance gate (reproducible
   in CI); wall-clock ≤ ~0.3s is a secondary observational metric, not a gate** (SR-GATE-1 — a
   loaded test machine makes wall-clock flaky; the counter suite is the enforcer). Tier-1 + Tier-2.5.
2. **Differential incremental proven on a real DAG.** Editing one file recomputes only the
   affected subgraph; unchanged targets keep identical action digests — **G6 green.**
3. **The daemon is actually warm.** A second daemon build does **`engine_recomputes == 0`** via
   the actor's `engine.request` (NOT `report.executed`) — **G4 green at Tier-2.5.**
4. **Single-writer + watcher correctness.** Concurrent builds + watcher events serialize
   deterministically with no panic (**G11**); known/unknown/delete events route correctly
   (**G12**) — both green at Tier-2.5.
5. **Three-way parity + no pollution.** daemon == local == bazel byte-identical (**G2**); zero
   source-tree pollution, `bin_tree_layout` consistent (parsed from the forwarded args) (**G3**) — both green.
6. **CLI is a thin client.** Default routes to the daemon (auto-spawn); `--batch` is the only
   in-process path — **G5 green.**
7. **Restart + version + manifest safety.** **G9**, **G10**, **G13**, **G15** green at tiers.
8. **All gates green at their tiers** in daily CI for a full bank; `//:test_all` green.
9. **The RG-0008 cold shim AND the toy `Workspace` are deleted** (`rpc.rs:353-359`,
   `lib.rs:44-87`), protected by G2/G3/G4/G11; the engine + `IncrementalBuilder` are live callers
   (RULE 4); AD7 advances to **L+CI** in `RazelDevStatus.md` §3.

> **Scope acceptance:** items 1–9 cover **single-target `build`** (concrete label + `//:razel`
> alias). `run`/`test`/patterns/multi-target (WS-G, Wave 3) are tracked separately with their
> own gates; `run` carries identical `args`/`cwd` to `build`.

---

## 11. Review incorporation log

Every P1/P2 finding from both reviews → the section(s) changed → one-line resolution. (P3/minor
at the end.)

| Finding | Section(s) changed | Resolution |
|---------|--------------------|------------|
| **R55-P1-flags-wire** — `GlobalFlags` not wire-safe (`sched_hook`, `crate_lock`, `fetched_external_base`) | §4.3, §3.2, §3.3, §3.5, §5 WS-D/WS-E | The wire forwards **raw arg tokens + cwd** (bazel `RunRequest.arg` model), NOT `GlobalFlags` and NOT a typed per-flag DTO (a typed DTO would be a denormalized copy of the flag set that drifts — caught on review of rev2's first cut). `parse_opts`/`global_flags()` move to `razel-loading` as the single source of truth; the daemon parses server-side + fills daemon-derived fields; args/cwd round-trip + parse-equivalence tests + hello bump; Windows split (TCP/kill). |
| **R55-P1-C1-lossy** — flat `&[Digest]` loses input/output path names | §4.1, §4.4, §9 R-4, §7.2 G15 | C1 compute receives `&[DepValue]` (keyed) and actions return `Manifest`; downstream selects outputs by path; G15 + the §4.4 contract test fail if narrowed back to `&[Digest]`. |
| **R55-P1-actor** — actor named but no single-writer boundary | §3.5, §5 WS-D, §7.2 G11 | Added the frozen `WorkspaceActor` contract (RPC threads enqueue Build/SetInput/Rescan/Shutdown; sole mutator; committed snapshots out); gate G11. |
| **R55-P1-watcher** — watcher/rescan can panic or go stale | §4.1, §5 WS-D, §9 R-7/R-8, §7.2 G12/G9 | Baseline SEQUENCE frozen (analysis establishes leaf inputs FIRST, then SetInput); known→SetInput (guarded), unknown/BUILD/MODULE/lockfile/rc/delete→Rescan; gates G12 (semantics) + G9 (restart). |
| **both-P1-WS-C** — WS-C is migration, not greenfield | §5 WS-C, §2.2, §6.1/§6.2 | Renamed WS-C to "migrate `IncrementalBuilder` to C1+C2 + workspace-label"; listed the 4 keep-green carve-out tests; deleted the duplicate `fs::read`; flagged the WS-A→WS-C compile break + scheduled WS-C sequentially (Wave 1.5). |
| **both-P1-counters** — `recomputes` conflates engine recomputes with action executions | §7.1, §3.6, §7.2 G1/G4/G14 | Split into 6 counters (engine_recomputes, actions_executed, action_cache_hits, input_digest_reads, exec_root_rebuilds, analysis_runs), defined + sourced; rename/replace `BuildResult.recomputes`; G4 asserts engine counter from the actor; counter split scheduled FIRST in WS-F/Wave 0. |
| **both-P1-perf-execroot** — 0.3s target ignores per-build exec-root + re-analysis | §3.6, §5 WS-D, §10.1, §7.2 G14 | Frozen: daemon holds a persistent exec-root + cached analysis (output-base model); added `exec_root_rebuilds`/`analysis_runs` counters + G14 + acceptance criterion. |
| **both-P1-G1-label** (R25-P1 / R55-P2) — G1 label + Tier-1 wiring mismatch | §7.2 G1, §7.3, §10.1 | G1 builds the resolved label `//crates/razel-cli:razel` (note the `//:razel` alias); WS-F un-ignores the dogfood test + wires a non-manual Bazel target; documented `RAZEL_PROCESS_WRAPPER` prereq; labeled G1 as EXTEND. |
| **both-P1-C1-placeholders** (R25-P1) — frozen contracts had `/* sketch */` bodies | §4.1, §4.2, §4.4 | Real signatures + real semantics (digest_equal, empty/single/multi-output, dir tree-hash, absent-output omitted, one ComputeError type); added the Wave-0 compile-check contract test (§4.4). |
| **both-P2-two-workspaces** (R25-P2 / R55-P2) — two `Workspace` types, no merge plan | §3.5, §5 WS-D, §8 | Decision: `Server` owns one `WorkspaceActor` holding the Engine; the toy `razel_daemon::Workspace` is repurposed then **deleted** (added to the §8 migration checklist). |
| **both-P2-G10** (R25-P2 / R55-P2) — G10 already mostly implemented | §7.2 G10, §7.3 | Marked G10 as EXTEND existing transcript test (`transcript.rs:162-177`); precise missing assertion: name BOTH client + daemon versions. |
| **R55-P2-C2-dirs** — C2 missing dir/absent-output manifest semantics | §4.2, §7.2 G13 | Frozen: file=(path,blake3); dir=(path,tree-hash == input dirs); absent=omitted; canonical; gate G13. |
| **R55-P2-thin-client-scope** — architecture forwards every command but plan only does `build` | §5 WS-G, §3.2, §4.3, §10 scope note | Added Wave-3 WS-G for run/test/patterns/multi-target with their own gates; froze the `run`/`test` request shapes (carry `args`/`cwd`/`run_args`); explicit scope boundary stated. |
| **R25-P2-windows** — auto-spawn contract Unix-centric | §4.3 | Added: Windows uses the same protocol over loopback TCP; version-skew restart uses process-kill, not SIGTERM. |
| **R25-P2-counter-scope** — `input_digest_reads` undefined | §7.1 | Defined: content reads for action-INPUT key formation only (excludes output reporting + the deleted duplicate re-read); watcher SetInput = exactly one digest/changed path (zero on no-op). |
| **R25-P3-citations** — `no_op_rebuild_does_zero_work` mis-cited under razel-engine | §2.2 | Corrected: it lives in `razel-daemon/src/lib.rs:154-161` (toy graph); cited the real engine tests at `razel-engine/src/lib.rs:202-284` (`incremental_equals_from_scratch` at `:235`). |
| **R25-P3-R2** — embedded timestamps risk only "open" | §9 R-2, §5 WS-F | Promoted to a NAMED audit step in WS-F tied to a parity test (extend G2), not left open. |
| **R25-P3-add_action-note** — WS-A should note incremental.rs uses add_derived for actions | §5 WS-A/WS-C | Noted: today's `incremental.rs` uses `add_derived` for actions (compile break); WS-C migrates them to `add_action`. |

---

## 12. Self-review findings & resolutions

A six-lens adversarial self-review (2026-06-20). Every premise was verified against the tree before
acting; line numbers cited by the reviewers were trusted-but-checked and corrected where off (see
"line fixes" below). Decisive resolutions only — no option lists.

| Finding | Lens | Sev | Resolution (section) | One-line |
|---------|------|-----|----------------------|----------|
| C3-WIRE-001 | contracts | P1 | §4.3 blocker note, §4.4 | `razel.taut.py:133-134` carries only `target: STR`; add `args`/`cwd` + `c3_args_roundtrip` before parse_opts moves. |
| C1-DEPVALUE-001 | contracts | P1 | §4.1 DepValue | Borrowed `&'a NodeValue` infeasible vs RefCell engine; `DepValue` now holds a **cloned** value. |
| C2-MANIFEST-RETURN-001 | contracts | P1 | §4.2 blocker note | `execute_action`/`ExecOutcome`/`Manifest` don't exist yet; freeze types + stub before WS-A. |
| EXEC-ROOT-REBUILD-COUNTER-001 | contracts | P1 | §3.6 note, §7.1 counter | `prepare_exec_root` (`drive.rs:69`) always `rm -rf`s; add counter; G14 fails on cold path until persistent exec-root. |
| ANALYSIS-RUNS-COUNTER-001 | contracts | P1 | §7.1 counter, §3.6b | Two analyze sites — `warm_analyze` (`rpc.rs:166`) AND `analyze_workspace_resolved` (`drive.rs:58`); instrument both. |
| C3-ROUTING-002 | contracts | P2 | §4.3 routing note | `parse_opts` has no `build_args` split; WS-E must yield `(Opts, build_args)`; forward only build_args. |
| C3-CWD-RESOLUTION-001 | contracts | P2 | §4.3 cwd note | Freeze identical client/daemon `-C`+canonicalize resolution; `c3_parse_equivalence` runs a relative-`-C` case. |
| PARSE-OPTS-DEPS-001 | contracts | P2 | §5 WS-E audit | Move `parse_opts`+`parse_opts_with_rc`+`global_flags`+`resolve_long`+`dispatch`+flag tables+`Opts` to razel-loading; no wire/daemon dep. |
| INPUT-DIGEST-READS-SCOPE-001 | contracts | P2 | §7.1 counter | Pin scope to `digest_input` in `run_one_target`; exclude startup rescan; fixed `digest_of` cite to `rpc.rs:509`. |
| CA-1 | concurrency | P1 | §3.5 WARNING, R-9 | No actor boundary today (`rpc.rs:143-148`); promote actor to FIRST WS-D item; G11 gates. |
| CA-2 | concurrency | P1 | §3.5 reply-channel, R-9.5 | `Build` needs a reply channel for backpressure/sequencing; carries the final uncancelled result under cancel-and-restart (R-9.5). |
| CA-3 | concurrency | P1 | §3.5 snapshots-out, G11 | Snapshot clone must be inside the lock; G11 asserts two subscribers see byte-identical state per revision. |
| CA-5 | concurrency | P2 | §3.5 staleness, R-9.6 | Cancel-and-restart bounds staleness to the current action's remaining runtime (not build duration); `--batch` = no-daemon escape. |
| CA-6 | concurrency | P2 | §3.5 queue-order | FIFO one-at-a-time; SetInput valid only after Rescan; unknown-key panics loudly. |
| CA-7 | concurrency | P2 | §3.5 watcher note, §5 WS-D, G12 | `watch()` defined (`lib.rs:92-106`) but never called by `serve`; WS-D wires it. |
| CA-4 | concurrency | P2 | G11 | invocation.events log order under concurrent do_run — clarified (code correct); G11 asserts per-invocation seq. |
| CA-8 | concurrency | P3 | G11 | 3 concurrent invocation.events subscribers, no starvation (thundering-herd check). |
| SR-GATE-1 | gate-rigor | P1 | §7.2 G1, §10.1 | Wall-clock demoted to observational; counters are the gate. |
| SR-GATE-2 | gate-rigor | P1 | §7.2 G1, §3.6b | G1 builds the RESOLVED label; alias resolution cached at Rescan so `analysis_runs == 0` holds. |
| SR-GATE-3 | gate-rigor | P2 | §7.1 wire blocker | `recomputes` wire field = `report.executed` (`rpc.rs:381`); split into a `Counters` message FIRST. |
| SR-GATE-4 | gate-rigor | P2 | §7.1, §7.2 G1 | `input_digest_reads == 0` asserted on the 2nd build SAME session; restart is G9's scope. |
| SR-GATE-5 | gate-rigor | P2 | §7.2 G1 companion | Anti-regression: `actions_executed==0` does NOT imply zero reads; companion proves the counter flows. |
| SR-GATE-6 | gate-rigor | P2 | §7.2 G6 variant | Partial-failure differential: B/C skip deterministically on A's failure. |
| SR-GATE-7 | gate-rigor | P2 | §7.2 G12 variant | concurrent edit during engine.request serialized by the queue. |
| SR-GATE-8 | gate-rigor | P2 | §7.2 G14 | G14 must prove invalidation: edit BUILD → `analysis_runs > 0` → re-cache → 0. |
| SR-GATE-9 | gate-rigor | P2 | §7.2 G3 | Assert the daemon's `bin_tree_layout` VALUE == true, not just output location. |
| SR-GATE-10 | gate-rigor | P2 | §7.2 G13 variant | Absent declared output consumed downstream = input-set change, not silent skip. |
| SR-GATE-11 | gate-rigor | P2 | §7.2 G15 | 3-level re-use variant guards positional regression. |
| SR-GATE-12 | gate-rigor | P2 | §7.2 G11 | Real-threading concurrent-edit-during-request; G11 DEPENDS-ON WS-D. |
| SR-GATE-13 | gate-rigor | P1 | §4.1 SPEC-vs-CODE | `lib.rs:17` still `Fn(&[Digest])->Digest`; "frozen" = signatures agreed, WS-A implements. |
| SR-GATE-14 | gate-rigor | P2 | §7.2 G2 | Bazel reference required-or-pre-golden-or-fail-loudly. |
| SR-GATE-15 | gate-rigor | P1 | §4.1 DepValue, R-9.7 | `DepValue.key` is the ACTION key; output-name collision = rule-author hygiene risk. |
| PAR-1 | parallelization | P2 | §5 WS-F, §6.1 | WS-F counter hooks micro-depend on WS-B's `execute_action`; named the 3 call sites. |
| PAR-2 | parallelization | P2 | §3.6a | Persistent exec-root `fix_up_exec_root` design + broken-symlink failure mode. |
| PAR-3 | parallelization | P2 | §5 WS-E | parse_opts move needs a `razel-loading` unit test landing WITH WS-E, not deferred to WS-F. |
| PAR-4 | parallelization | P2 | §3.6b, §5 WS-D, G14 | Analysis-cache `AnalysisDigest` key + Rescan-dirty invalidation spec. |
| PAR-5 | parallelization | P2 | §3.5 queue-order, G12 | FIFO order guarantee; G12 asserts Rescan-before-SetInput + unknown-key panic. |
| PAR-6 | parallelization | P1 | §5 WS-C, §6.1 | WS-C is sequential after WS-A AND WS-B (not just A); explicit WS-B→WS-C edge. |
| PAR-7 | parallelization | P2 | §5 WS-D, R-9 | WS-D is design-heavy (exec-root fixup + analysis invalidation); design before coding. |
| PAR-8 | parallelization | P3 | §4.4 | `c3_parse_equivalence` must be authored in Wave 0; ~20-field GlobalFlags snapshot. |
| PAR-9 | parallelization | P3 | §7.3 G1-wiring | Name the Bazel target (`g1_no_op_rebuild_zero_work`) + RAZEL_PROCESS_WRAPPER prereq. |
| SR-LIFECYCLE-1 | bazel-fidelity | P1 | §3.7 idle-out | 5-min idle-out, `daemon.json` schema, reap/respawn, version-skew survives idle-out. |
| SR-WATCHFS-FALLBACK-1 | bazel-fidelity | P2 | §3.7 (G16) | notify-unavailable → per-build full rescan fallback; G16. |
| SR-THIRD-PARTY-CLIENT-TENSION-1 | bazel-fidelity | P2 | §4.3b | CLI raw-args + programmatic `BuildCommandRequest{EngineCommand}` converge on one handler. |
| SR-OUTPUT-BASE-MODEL-1 | bazel-fidelity | P2 | §3.7, §3.6a (G17) | `.razel-out` layout, sandbox scope, crash recovery via startup rescan. |
| SR-MULTI-WORKSPACE-ISOLATION-1 | bazel-fidelity | P2 | §3.7 (G18) | Per-workspace actor isolation gated by G18. |
| SR-SNAPSHOT-REVISION-MAPPING-1 | bazel-fidelity | P2 | §4.3c (G19) | `SnapshotId = u64 == revision`, epoch-disambiguated; G19. |
| SR-COMMITTED-SNAPSHOT-DEFINITION-1 | bazel-fidelity | P2 | §4.3d (G20) | Snapshot = immutable point-in-time engine view; BuildState is a projection; G20. |
| SR-EXEC-ROOT-CORRUPTION-1 | bazel-fidelity | P2 | §3.6a (G21) | Startup exec-root validation/repair; G21. |
| SR-C2-MANIFEST-ENCODING-1 | bazel-fidelity | P1 | §4.2b (G22) | Manifest wire-serialized sorted-by-path; cross-language byte-equal golden; G22. |
| SR-C1-DEPVALUE-BORROW-SAFETY-1 | bazel-fidelity | P1 | §4.1b (G23) | ComputeFn may not store DepValue or call Engine; compile-fail G23. |
| OPER-1-PROGRESS-STREAMING | completeness | P1 | §3.7 progress, R-9.9 | `Progress` lacks action stderr; add a failure-event field. |
| OPER-2-DAEMON-CRASH-CLIENT-RECOVERY | completeness | P1 | R-9.9, §3.7 (G17) | Client heartbeat-detects crash + reconnect-retry; G17. |
| OPER-3-SOCKET-PERMISSIONS | completeness | P2 | §3.7 sockets (G18) | Socket 0600 / dir 0700; cross-workspace hello rejected. |
| OPER-4-CACHE-AND-DISK-GC | completeness | P2 | §3.7 cache, R-9.8 | No auto-eviction v1; `clean --expunge`; idle-out frees a workspace. |
| OPER-5-TEST-SEMANTICS-EXIT-CODES | completeness | P2 | §5 WS-G | `test`/`run` Bazel exit codes (0/1/3) + per-test results; WS-G freezes the contract. |
| OPER-6-DAEMON-OBSERVABILITY | completeness | P2 | §5 WS-D | `daemon.log` JSON-lines + `daemon.status` query + failure stderr in BuildResult. |
| OPER-7-ANALYSIS-INVALIDATION-INTERACTION | completeness | P2 | §3.6b, §7.2 G12/G14 | Content-based `AnalysisDigest`; merged with PAR-4 (dedup). |
| OPER-8-CROSS-WORKSPACE-SCHEDULER | completeness | P3 | §3.7 thread budget, R-6 | Global core cap across lazy per-workspace pools. |

**Line-number fixes applied (reviewer cites trusted-but-checked):** `digest_of` is `rpc.rs:509` (reviewers said `rpc.rs:391`); the cold-path `execute_action`/`build_action` call site is `exec_root.rs:110` (PAR-1 said `drive.rs:110`); `prepare_exec_root` is conditional on `.razel-crates` existing (`drive.rs:67`), not strictly unconditional-per-build (EXEC-ROOT-REBUILD-COUNTER-001 softened accordingly); `run_action` is `incremental.rs:201` and the warm fs::read re-digest is `incremental.rs:212`. All other cites (ComputeFn `lib.rs:17`, set_input `lib.rs:86`, taut `build` method `:133-134`, BuildResult.recomputes `:59`, `recomputes: report.executed` `rpc.rs:381`, serve thread-spawn `rpc.rs:143-148`, GlobalFlags fields `flags.rs:11-60`) verified correct.

**Deduplication / reconciliation:** OPER-7 (analysis invalidation) and PAR-4 (analysis-cache key) target the same gap — both resolved in a single §3.6b subsection (content-based `AnalysisDigest`, Rescan-dirty), with G14 extended for the trigger and G12 noting BUILD-edit routing. SR-WATCHFS-FALLBACK-1's proposed §3.7b was folded into the §3.7 lifecycle subsection + G16 (one location). The exec-root fixup design appears once in §3.6a, referenced from §5 WS-D (PAR-2 + PAR-7).

**Findings deliberately NOT taken / down-scoped:** none rejected outright. The two **P3** completeness/concurrency items were taken as lightweight gate/spec notes rather than new sections (CA-8 → a scenario inside G11; OPER-8 → a §3.7 bullet + R-6), since spinning up dedicated machinery for them would over-build ahead of need. SR-GATE-3 (P2) was elevated to a §7.1 BLOCKER note because it gates G4's implementability (a P2 finding with P1 blast radius on a gate).

---

## 12.1 WS-D design spike (2026-06-20) — finding → resolution

The spike took WS-D from DESIGN-HEAVY (blocked-before-coding) to CODEABLE. Three coupled
sub-designs (actor mechanics, exec-root fixup, analysis cache) were integrated into one consistent
WS-D design: the SAME `Rescan` re-symlinks the forest (§3.6a), marks analysis dirty (§3.6b), and
refreshes `leaf_inputs` (§3.5) — verified against the tree at every cited line.

| Finding | Sev | Resolution (section) | One-line |
|---------|-----|----------------------|----------|
| PAR-7 (design-heavy → codeable) | P2 | §3.5a/§3.6a/§3.6b, §5 WS-D (D1-D6) | The "design before coding" blocker is discharged: concrete algorithms + seams + the D1-D6 sub-tasks replace it. |
| R-9 (actor loop unspecified) | P1 | §3.5a, §9 R-9 | Inbox enum, one-shot reply channel, FIFO single-writer loop, startup ordering, panic→reply-Err — fully specified; D1 is the fix. |
| CA-2 (reply channel) | P1 | §3.5a, §7.2 G26 | `Build` carries a `sync_channel(1)` reply so the CALLER blocks on the FINAL (uncancelled) result; G26 proves cancel-and-restart. |
| CA-5 (staleness window) | P2 | §3.5a cancel-and-restart, R-9.6 | Cancel-and-restart (R-9.5 flipped) bounds staleness to the current action's remaining runtime; `--batch` = no-daemon escape, not the mitigation. |
| CA-6 (FIFO + SetInput-after-Rescan) | P2 | §3.5a loop + startup, §7.2 G27 | One message in flight; startup `Rescan` baselines every leaf key BEFORE serving so SetInput can't hit the `lib.rs:86` panic; G27. |
| R-9.5 (cancel-and-restart) | P1 | §3.5a cancel-and-restart loop, R-9.5 | DECIDED (Gianni): cancel-and-restart for bazel parity; engine `set_cancel` implemented (`4ea0c60`), actor drains+re-runs; was frozen NO-CANCEL, now flipped. |
| PAR-2 (exec-root fixup) | P2 | §3.6a `fix_up_exec_root`, §7.2 G24/G25 | True incremental delta (add→symlink, delete→unlink, external on appear/move); `did_change` drives the `exec_root_rebuilds` counter; zero churn on unchanged sources. |
| SR-EXEC-ROOT-CORRUPTION-1 | P2 | §3.6a `validate_exec_root`, §7.2 G21/G17 | Startup validation/repair of broken/missing/wrong links; unrepairable → full `prepare_exec_root` fallback; crash recovery via the startup Rescan. |
| PAR-4 / OPER-7 (analysis key + invalidation) | P2 | §3.6b `AnalysisDigest`/`do_build_impl`, §7.2 G14 | Content-based key (sorted BUILD/MODULE/lockfile/rc digests + `options_digest`); any Rescan → dirty; recompute-on-change only; lockfile change keyed via `graph_shape_digest` (closes the GlobalFlags-mutable-field hole). |
| EXEC-ROOT-REBUILD-COUNTER-001 | P1 | §3.6a, §7.1 | The persistent fixup returns `did_change`; the actor increments `exec_root_rebuilds` once per Rescan iff `did_change` — drives G14's `== 0` on the no-op. |
| ANALYSIS-RUNS-COUNTER-001 | P1 | §3.6b, §7.1 | `analysis_runs` increments only in the fresh-vs-cached branch of `do_build_impl`; cached reuse → 0. |

**Consistency check (the three sub-designs are coupled — reconciled):** (1) §3.6b's "graph-shape
event → Rescan → analysis dirty" == §3.6a's `handle_rescan` setting `cached_analysis = None` in the
same handler that calls `fix_up_exec_root`. (2) §3.6a's add/delete/rename handling (symlink/unlink)
== the source-set diff `enumerate_sources` feeds, which is the SAME source-universe whose change
routes to `Rescan` in §3.5. (3) The actor (§3.5a) OWNS the exec-root + analysis cache; no other
thread mutates them — so the fixup, the analysis recompute, and the engine update are one atomic,
FIFO-ordered message. No contradiction across the three.

**Line-number corrections (rev4, trusted-but-checked against the tree):** the workspace analyze
entry point is **`analyze_workspace_resolved`** (`drive.rs:58`), not `analyze_workspace_with` (the
sub-design drafts used the latter in two spots — corrected here). The `prepare_exec_root` source
exclusion list is **`exec_root.rs:19-21`** (one draft said 19-22). `record_state`'s retain/push/sort
is `rpc.rs:430-432` (within the cited 410-436 block). All other cited lines (ComputeFn `lib.rs:17`,
set_input panic `lib.rs:86`, `prepare_exec_root` 11-35, `digest_input` 51-67, `run_one_target`
87-124, `Inner` 65-83, `serve` 134-150, `warm_analyze` 153-172, `do_run` 247-289,
`stream_build_state` 293-307, `do_build` 336-407, toy `Workspace` `lib.rs:44-87`, `watch`
`lib.rs:92-106`, `run_action` `incremental.rs:201-245`, `sync_file` guard `incremental.rs:177`)
verified correct.
