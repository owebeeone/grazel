# RazelPublicSurfaces — the formal public APIs (CLI + server)

*2026-06-12. Decision (Gianni): gryth requires a local server working the workspace —
razel adopts the same model (as Bazel does internally) and, UNLIKE Bazel, makes the server
API a FORMAL, VERSIONED, THIRD-PARTY surface. Companion to `RazelReleaseSpike.md` (V3sh1),
which builds the first slice; subordinate to `RazelV3Plan.md`'s invariants.*

## §1 The model — ONE daemon, MANY isolated workspaces

**The deliberate break from Bazel (Gianni, 2026-06-12): Bazel runs one server per
(workspace, output base); razel runs ONE resident daemon serving MULTIPLE isolated
workspaces concurrently** — gryth clients work across workspaces, and the daemon is the
one fabric they all reach. Mechanics:

- **Workspace handles.** Lifecycle.open(path) → a workspace handle; every other call is
  handle-scoped. Opening the same root twice yields the same context (refcounted); the
  daemon idles out when no workspace is open.
- **Isolation = one core ACTOR per workspace** (§1c): own Session lifecycle, own
  single-writer command queue, own committed snapshot + views, own watches. No shared
  mutable state between workspaces. Shared infra is read-only/content-addressed only:
  the download cache is global by construction (content-addressed); materialized
  externals and output trees stay per-workspace-hash (the round-36 layout already keys
  this way — the cache decision survives intact).
- **Bazel-compat is unaffected:** the CLI still resolves ITS workspace by boundary
  walk-up and opens that one handle; `--batch` bypasses the daemon entirely. The
  multi-workspace surface is razel-native (S-B), invisible to the Bazel story.

Clients: the **razel CLI** (client #1), **gryth** (the driving consumer — graph oracle,
builds, live views across its workspaces), and any third party.

**The architectural rule that keeps the API honest: the CLI is a CLIENT, with no
privileged in-process path.** Every verb goes through the same server API a third party
would use, from the first implementation (V3sh1 S3) onward.

## §1b Hosting: gryth inside the daemon's surface, outside its process

The gryth service registers INTO the daemon's protocol namespace (one endpoint, one
fabric) and is SUPERVISED by the daemon's Lifecycle service, but runs as its own OS
process (ts/npm stays ts/npm; crashes isolate; declared as a workspace target —
`razel_service(...)` in an E-mode package: server-as-declaration, the glade thesis in
the daemon). Clients see one server whose capabilities include gryth; gryth sees the
Command/Query/View/Events services over the same connection every client gets.

## §1c Thread/async isolation (decided: EVENT QUEUE into the core)

**The whole model in one sentence (Gianni): clients talk to the razel core via an event
queue.** Concretely:

- Each workspace context is an ACTOR: ONE command queue in, event streams out. The
  single-writer invocation model IS the queue discipline — commands (build/fetch/
  invalidate, incl. watch-triggered re-evaluations) execute one at a time per workspace,
  on the actor's own engine threads (the loading pool, min(6, cores)).
  DIFFERENT workspaces proceed concurrently (actor per workspace).
- The async edge (a small tokio runtime: UDS accept, connection framing, subscription
  fan-out, watchers, service supervision) NEVER touches engine state — it only enqueues
  commands and forwards events. Engine code never runs on async threads; razel-loading
  stays runtime-free (enforced by crate boundary).
- Queries/Views read the last COMMITTED snapshot (swapped atomically at invocation
  commit), never the live Session — consistent reads with zero locking against the
  writer; the versioned store later upgrades the mechanics, not the contract.
- Failure: each invocation runs behind a catch_unwind boundary — an engine panic fails
  the invocation, never the actor or daemon. Cancellation v1 is coarse (cooperative
  checks at package/action boundaries).
- **Global thread budget (the multi-workspace consequence):** per-workspace pools are
  lazy, and a DAEMON-LEVEL cap bounds total engine threads across actors (active
  workspaces share the machine; v1 policy: cap = cores, actors acquire pool slots
  on-demand and shrink when idle).

## §2 The surfaces (enumerated — nothing else is public)

- **S-A: The CLI.** Verbs (`build`/`run`/`test`/`query`/`fetch`), flags (the
  Bazel-compatible set with Bazel semantics + razel-only flags incl. `--strict_bazel`),
  exit codes, output contracts. Bazel-compat portions track Bazel's semantics (strict mode
  is the oracle); razel-only portions version with the server API.
- **S-B: The server API** (the third-party programmatic surface). Five services, all
  STREAM-FIRST (§4b — nothing long-running ever blocks a caller):
  - **Command** — build/run/test/fetch. Returns an invocation id IMMEDIATELY; results,
    diagnostics and PROGRESS ("300/2000 BUILD files loaded", "action k/n") arrive as
    events on the invocation's stream. The engine's existing observation seam
    (`sched_hook`, round 28) is the progress source — instrumentation already in place.
  - **Query** — one-shot graph oracle against the last COMMITTED snapshot: targets,
    deps/rdeps, providers, actions, packages (razel-analysis formalized).
  - **View** — a SUBSCRIBED query: initial snapshot + ordered DELTAS as the workspace
    changes (file edit → invalidation → re-evaluation → view delta). The gryth-ui
    primitive: live build-graph views updating in real time. Deliberately GRIP-SHAPED —
    a view is a producer gryth's taps consume 1:1; razel-side the natural seam is the
    DDS (demand-driven by design).
  - **Events** — the global feed: file-change events, invalidations, build
    failures/diagnostics, service lifecycle. BEP-*shaped* in taut (a literal-protobuf
    BEP adapter stays a later optional bridge — §4).
  - **Lifecycle** — workspace open/status/invalidate, watch control, hosted-service
    supervision, shutdown.
- **S-C: Workspace file contracts.** BUILD[.bazel]/MODULE.bazel/.bazelrc consumed with
  Bazel semantics; `BUILD.razel`/`MODULE.razel`/`.razelrc` per the V3sh1 §3 definitions.
- **S-D: Artifact formats.** `razel-lock.json`, the cache layouts (Bazel-mirrored below
  the root — the round-36 decision), output-tree conventions.

## §3 Stability policy

The server API and razel-native CLI surface version together (semver discipline; a
compatibility window once gryth depends on it — pre-1.0, breaking changes allowed but
CHANGELOG'd per release). Bazel-compat surfaces have no independent version: their
contract is "what bazel-7.7.0 does," enforced by strict-mode goldens; the pinned bazel
version is itself part of the public claim and bumps deliberately.

## §4 Protocol: TAUT, not gRPC (decision: Gianni 2026-06-12)

The wire contract is authored as **taut IR** and generated — this is not new
infrastructure, it is the EXISTING razel-wire architecture promoted to the public
surface: `wire/razel.taut.py` is the single source of truth; `tautc` generates the native
Rust types + deterministic-CBOR codec into `razel-wire` (server + razel-cli share them),
and taut's **TypeScript backend** generates the gryth/node client from the SAME IR —
cross-language type identity by construction, no protobuf/gRPC toolchain anywhere.
Properties this buys over gRPC: deterministic encoding (golden-testable wire bytes),
one governed IR for every language razel touches (rust/ts today; taut also carries
go/java/kotlin/swift/cpp backends), and codegen outside the cargo graph (the established
xtask discipline).

Transport: framed deterministic-CBOR taut messages over a LOCAL unix domain socket first;
the service definitions are transport-agnostic and future transports ride unchanged — the
iroh path (remote/distributed workspace access, gryth's native fabric) is explicitly
anticipated and explicitly NOT v1. Ecosystem adapters are exactly that — adapters over
S-B, never the native surface: a literal-protobuf BEP emitter for bazel-ecosystem tools,
a BSP shim for IDEs, both optional and later; the native Events service is BEP-*shaped*
in taut.

## §4b The streaming model (Gianni, 2026-06-12 — multi-client, event-driven)

**Connections are long-lived and multiplexed.** Many clients hold open connections
concurrently; the framing carries subscription/stream ids over the same framed-CBOR
transport (UDS first; the iroh fabric later carries it unchanged). Browser clients are
GRYTH'S edge concern — the gryth service bridges daemon streams to WebSocket for
gryth-ui; the daemon itself stays transport-minimal.

**Nothing long blocks.** A 5-minute graph load answers in milliseconds with an invocation
id and streams progress; the CLI is just a renderer of the same stream gryth consumes
(client #1 discipline holds — the progress bar IS the public API).

**The live loop:** watcher fires → invalidation enters the single-writer queue (§1c — a
watch-triggered re-evaluation is an ordinary invocation) → on commit, the new snapshot
swaps in → per active View, the daemon diffs committed snapshots and emits deltas.
V1 delta mechanics are deliberately COARSE (snapshot diff per commit — correct, and cheap
at gryth scale); the versioned store upgrades this to fine-grained deltas without
changing the View contract — the API is shaped for where the store is going, not for
what it is today.

**Fan-out and dedup:** subscriptions are keyed by canonical query — N clients asking the
same view share ONE materialized view with N subscribers. Event delivery is per-client
ordered; slow consumers get bounded buffers + drop-with-resync (a client can always
re-request the snapshot), never unbounded daemon memory.

(Open, marked not-yet-defined: the view query language's expressiveness v1 — start with
the razel-analysis primitives (targets/deps/rdeps by pattern) and grow by gryth's pull;
resync/backpressure tuning; auth for non-local transports — iroh-era.)

## §5 Gryth's MVP slice (what the driving consumer needs first)

Open workspace → build/run/test with streamed progress → query deps/rdeps/targets →
SUBSCRIBE a live view → file-change events drive view deltas. This slice IS V3sh1 S3+S5's
server: the `run` verb lands as the Command service's first method with the CLI as its
first client; Query formalizes what razel-analysis already computes; Events/Views begin
as the sched_hook stream structured + coarse snapshot-diff views, growing toward BEP and
fine-grained deltas respectively.

## §6 Anti-goals

No remote execution protocol (REAPI) claims; no Bazel-server wire-compat (Bazel's command
protocol is private and version-entangled — mimicking it would chain razel to internals
Bazel itself won't stabilize); no multi-workspace federation in v1 (the iroh arc owns
that).
