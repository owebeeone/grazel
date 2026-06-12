# RazelPublicSurfaces — the formal public APIs (CLI + server)

*2026-06-12. Decision (Gianni): gryth requires a local server working the workspace —
razel adopts the same model (as Bazel does internally) and, UNLIKE Bazel, makes the server
API a FORMAL, VERSIONED, THIRD-PARTY surface. SHARED design — both workstreams answer to
it: `ws-razel/RazelReleaseSpike.md` (V3sh1) and `ws-grazel/GrazelWorkstream.md` (GR0–GR5),
which builds the first slice; subordinate to `RazelV3Plan.md`'s invariants.*

## §1 The model — ONE daemon, MANY isolated workspaces

**The deliberate break from Bazel (Gianni, 2026-06-12): Bazel runs one server per
(workspace, output base); razel runs ONE resident daemon serving MULTIPLE isolated
workspaces concurrently** — gryth clients work across workspaces, and the daemon is the
one fabric they all reach. (Grazel partitions this into named SERVICE SCOPES — one such
daemon per scope, §1e; everything in this section describes each daemon.) Mechanics:

- **Workspace handles.** Lifecycle.open(path) → a workspace handle; every other call is
  handle-scoped. Opening the same root twice yields the same context (refcounted); the
  daemon idles out when no workspace is open. *(Amended 2026-06-12, Gianni via the
  GR1 round: GRAZELD's default is NO idle-out — a scope daemon runs indefinitely as
  the scope's service; `--idle-timeout` is opt-in. razeld keeps the idle-out default;
  workspace handles stay refcounted in both.)*
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

## §1b Distributions: razel (boring) and GRAZEL (the node) — naming decision, Gianni 2026-06-12

ONE engine, TWO installables:

- **razel** — the boring Bazel-compatible build tool: CLI + razeld, the five S-B
  services over a local UDS, nothing else. No iroh, no gryth, no p2p anywhere in its
  dependency graph. This is the thing the strict-mode goldens and the TF floor certify,
  and the only thing the Bazel-compat claim attaches to.
- **grazel** — the all-singing variant: the SAME engine crates PLUS an in-process gryth
  p2p node (iroh endpoint) in one binary. grazel nodes talk iroh to OTHER grazel nodes;
  some nodes serve a gryth client or two (gryth-ui reaches the mesh through whichever
  node it attaches to). **Web clients don't make good p2p nodes (the browser/UDP
  story), so a grazel node ALSO serves HTTP/WS to gryth clients — through a
  still-to-be-defined GLADE layer** (the share kernel as the gryth-facing surface;
  its definition is the glade arc's, not this doc's — here it is a named hole).
  grazeld is razeld-plus: every S-B service verbatim, plus the node/p2p services,
  plus the HTTP edge.

**The compatibility arrow extends one level up** (the §3b not-locked-out doctrine,
recursively: each layer needs the one below, never the reverse): **bazel ⊂ razel ⊂
grazel.** A razel workspace builds identically under grazel; `--strict_bazel` works in
grazel; razel NEVER grows an iroh dependency — enforced the same way razel-loading
stays runtime-free: the node code lives in grazel-only crates that link the razel
crates, never the other way.

Local plumbing when both are installed on one machine:

- **Separate UDS namespaces.** grazel↔grazeld comms get their OWN sockets — one per
  service scope under `~/.grazel/.uds/<scope>` (§1e) — while razeld keeps
  `_razel_<user>/daemon/`, so razeld and grazelds coexist without ambiguity; each CLI
  dials its own daemon. Same wire protocol and IR — the grazel services are additional
  taut services in the same namespace, so a razel client pointed at grazeld just sees
  the S-B subset.
- **Shared caches, arbitrated outputs.** The content-addressed download cache is shared
  by construction. Output bases are NOT keyed by distribution — switching a workspace
  between razel and grazel must not rebuild the world — so the single-writer rule
  extends ACROSS daemons: one output-base lock per workspace (Bazel-mirrored mechanics);
  whichever daemon holds it is that workspace's writer, the other fails loud with a
  "held by <daemon>" message. **Concrete contract (agreed RG↔RR 2026-06-12, inbox
  0005 both lanes):** `<workspace>/.razel-cache/workspace.lock`, created with
  `create_new`, ONE JSON line `{"pid":N,"daemon":"razeld"|"grazeld"|"razel-local",
  "scope":"<name>"}`; live holder → fail loud naming daemon+scope+pid, dead holder →
  reap and retake; holder removes pid-checked on every exit path. ONE implementation:
  `razel-daemon::outlock` (grazeld consumes it through the lib — the crate arrow).
  When output bases move out-of-tree the lock moves with them; same contract.

The `razel_service(...)` hosting mechanism (supervised out-of-process services declared
as E-mode targets) survives for ts/npm pieces gryth may add, but the p2p node does NOT
ride it — the node is in-process by construction (iroh is Rust; the node IS the daemon's
fabric, not a supervised child). How much gryth server logic lives in the rust node vs
hosted TS services is gryth-dev's call; this surface stays agnostic.

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

## §1d Where grazel lives (repo/crate layout)

**Decision: grazel is sibling CRATES in the razel cargo workspace, not a separate repo
(for now).** During the spike razel and grazel co-evolve too fast for cross-repo version
skew; one workspace gives atomic changes and one CI. The dependency direction is
enforced mechanically, not socially: a CI deny rule (cargo-deny / dep-graph check) that
no `razel-*` crate depends on any `grazel-*` crate or on iroh — the same enforcement
class as razel-loading's runtime-free rule. BECAUSE the arrow only points one way, the
later split-out is cheap; the revisit trigger is release cadence divergence (when gryth
productizes and grazel needs its own release train).

The crate shape that makes "grazel CLI = razel CLI + extra params" true by construction
rather than by porting:

- **`razel-cli` becomes a LIBRARY** (verb dispatch, flag parsing, daemon dialing, stream
  rendering) with a thin `razel` bin over it.
- **The grazel side is TWO crates** (Gianni, 2026-06-12): `grazel-cli` — the bin
  (binary named `grazel`) kept SMALL, nothing but rust/OS mechanics (argv/env intake,
  exit codes, signals, process entry) that just invokes — and `grazel-cli-lib`, where
  ALL business logic lives, linking the SAME razel-cli lib plus `grazel-node` (the
  iroh endpoint + node services). The CLI surface is razel's verbatim — same parser,
  so razel flag evolution reaches grazel automatically — plus grazel-namespaced
  verbs/flags for the node side. A razel flag never behaves differently under grazel.
  (The thin-bin rule is also what makes T1 cheap: the lib is testable in-process,
  no binary spawning to exercise logic.)
- **One binary per distribution:** razeld is `razel` in daemon mode, grazeld is `grazel`
  in daemon mode (Bazel's client-launches-server pattern, without a second artifact to
  version or ship).
- Strict-mode goldens (T3) run against BOTH binaries' build verbs — cheap, same lib, and
  it keeps the "grazel is still boring bazel underneath" claim tested rather than assumed.

**Development topology (Gianni, 2026-06-12): two working TREES, one crate arrow.**
razel is cloned to a sibling `razel-grazel/` tree where a separate agent builds out the
grazel components (the `grazel-*` crates: bin, node/iroh, scope daemon, HTTP/glade
edge); the `razel/` tree continues everything truly razel/bazel-specific. The crate
dependency arrow becomes a LABOR arrow: **the grazel tree never modifies `razel-*`
crates** — a needed seam change is a request to the razel side, landed there and pulled.
With that rule the two trees' file sets are disjoint by construction, so cross-pulls
are routine and the conflict surface is confined to the declared seams (the razel-cli
lib API and the razel-wire IR — IR changes land razel-side, since both trees consume
the generated types).

**Share + comms mechanics (Gianni, 2026-06-12):** the share layer is a LOCAL BARE
repo, `razel.git`, sibling to both trees; both work on the SAME branch (`razelv3`) and
sync through it (remote `share` in razel/, `origin` in razel-grazel/) — merges stay
trivial because the file-ownership rule keeps the trees' edits disjoint. The slow
comms channel between the agents is a pair of INBOX folders that travel with the
branch: `dev-docs/ws-razel/inbox/` (notes FOR the razel agent) and
`dev-docs/ws-grazel/inbox/` (notes FOR the grazel agent) — one numbered note per
requirement, `Status: open → done`, checked at every sync (protocol in each inbox's
README). Sequencing: the clone exists from day one; work that CONSUMES the S0 seam
waits for S0 to arrive by pull (announced via inbox), but grazel's design + additive
crate creation start immediately. The doc split mirrors the tree split: work items
live in `dev-docs/ws-razel/` (V3 + spike) and `dev-docs/ws-grazel/`
(`GrazelWorkstream.md`, GR0–GR5); DESIGN docs — this one included — stay at the
dev-docs root, shared by both.

## §1e Finding the daemon: SERVICE SCOPES (discovery + configuration — nailed)

**A workspace does not pick a daemon; it names a SCOPE (Gianni, 2026-06-12).** A scope
is a named workspace COLLECTION served by its own grazeld instance — the isolation unit
ABOVE workspaces: own process, own UDS, own iroh identity, own watch set. The motivating
case is trust separation: `customerA` and `customerB` workspace collections never share
a process, a key, or a fabric — customerA's mesh cannot even learn that customerB
exists. Scopes are a GRAZEL concept; razel has no mesh, nothing to separate, and keeps
one boring per-user daemon at `_razel_<user>/daemon/` (socket + daemon.json + launch
lock there).

- **Sockets:** one per scope at `~/.grazel/.uds/<scope>` — a flat dir of short,
  user-only-perm socket paths (flat and short deliberately: macOS caps UDS paths at
  104 chars). Per-scope daemon state lives in `~/.grazel/scopes/<scope>/`: daemon.json
  (pid, build + wire versions, public node id), the iroh secret key, the launch lock,
  scope config.
- **Binding:** the workspace's `.grazelrc` carries `service_scope=customerA`. A grazel
  command run anywhere in that workspace dials the customerA socket and sends the
  WORKSPACE ROOT in its hello — the daemon discriminates which workspace handle the
  command applies to (the §1 handle model unchanged; the scope only chooses WHICH
  daemon). No `service_scope` → the `default` scope: zero config stays zero config.
- **Membership is BOTH static and dynamic** ("use both"): the scope's config MAY pin
  workspaces (pre-opened and watched from daemon start — the long-lived-service
  posture), and any invocation whose rc names the scope dynamically ADDS its workspace
  (Lifecycle.open, refcounted, idles out per §1). Pinned = present from start;
  dynamic = present while used.
- **One scope per workspace at a time:** the rc binding makes every invocation in a
  workspace dial the same daemon, and the §1b cross-daemon output-base lock backs it
  mechanically — it already arbitrates N daemons (razeld + any number of scope
  grazelds), so a mis-bound or doubly-claimed workspace fails loud, naming the holder.
- **Identity is per-SCOPE, not per-user:** each scope dir holds its own iroh secret
  key; meshes are joined per scope; daemon.json publishes only the public node id.
  There is no separate `--profile` mechanism — scopes ARE the profiles.
- **Override chain** (highest wins): `--scope=<name>` flag → `GRAZEL_SCOPE` env →
  `.grazelrc` `service_scope` → `default`. The rc delta chain stays recursive:
  `.bazelrc` → `.razelrc` → `.grazelrc`, each a pure delta the layer below never reads;
  razel ignores `.grazelrc` entirely, and a grazel-only key in `.razelrc` is an ERROR
  (razel must never become grazel-aware — the §1b crate arrow, expressed in config).
- **Dial procedure** (shared razel-cli lib code, both CLIs): resolve scope → connect
  its socket → taut hello (client build + wire version + workspace root) → on
  wire-version mismatch, graceful-shutdown and relaunch THAT scope's daemon only
  (Bazel's restart semantics, scope-local); on stale socket (refused + dead pid),
  clean and autostart `grazeld --scope=<name>`. `--no_autostart` for CI/scripting.
- **Thread budget consequence:** the §1c cap (= cores) is PER DAEMON; concurrent scopes
  contend at the OS level. Accepted for v1 — scopes are coarse and typically one is hot
  at a time; a machine-level broker is versioned-store-era work.
- **When grazel is installed:** recommended setup is grazel CLI everywhere (grazeld is a
  strict superset); razel CLI remains for `--strict_bazel` parity work. Running razeld
  beside scope daemons is SAFE (the output-base lock arbitrates writers) but means two
  engines warming the same workspace — a cost, not a hazard.

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
iroh path (remote/distributed workspace access, the inter-node fabric) is GRAZEL's
transport (§1b), never razel's: razel stays UDS-local forever, grazel carries the same
framed streams over iroh between nodes. Ecosystem adapters are exactly that — adapters over
S-B, never the native surface: a literal-protobuf BEP emitter for bazel-ecosystem tools,
a BSP shim for IDEs, both optional and later; the native Events service is BEP-*shaped*
in taut.

## §4b The streaming model (Gianni, 2026-06-12 — multi-client, event-driven)

**Connections are long-lived and multiplexed.** Many clients hold open connections
concurrently; the framing carries subscription/stream ids over the same framed-CBOR
transport (UDS first; the iroh fabric later carries it unchanged). Browser clients
attach over HTTP/WS served by the GRAZEL node itself (§1b — web clients can't be p2p
nodes), through the to-be-defined glade layer; RAZEL stays transport-minimal (UDS only —
no HTTP anywhere in the boring distribution).

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
resync/backpressure tuning; the GLADE LAYER — how grazel's HTTP edge presents
daemon streams to gryth web clients (§1b's named hole, owned by the glade arc); auth
for non-UDS transports — needed at grazel's HTTP/iroh edges, never for razel.)

## §5 Gryth's MVP slice (what the driving consumer needs first)

Open workspace → build/run/test with streamed progress → query deps/rdeps/targets →
SUBSCRIBE a live view → file-change events drive view deltas. This slice IS V3sh1 S3+S5's
server: the `run` verb lands as the Command service's first method with the CLI as its
first client; Query formalizes what razel-analysis already computes; Events/Views begin
as the sched_hook stream structured + coarse snapshot-diff views, growing toward BEP and
fine-grained deltas respectively.

## §6 The test regime (T0–T5 — defined BEFORE S1 starts, per Gianni's gate)

A pyramid: each tier is cheap relative to the one above it, runs in CI for every bank,
and every tier below a change's layer must be green before it lands (the V3 roll-build
invariant extended to the server surface). Nothing on the public surface ships untested
at its own tier.

- **T0 — wire goldens.** The taut layer's determinism made executable: golden byte
  vectors per message type, checked into the repo. The generated Rust codec must
  ENCODE to the golden bytes and the generated TS client must DECODE them (and
  round-trip) — cross-language type identity is asserted by bytes, not by review.
  Schema evolution = a deliberate golden update in the same commit as the IR change;
  an accidental wire break cannot land silently.
- **T1 — service-contract transcripts.** Each S-B service method gets transcript tests
  against a REAL daemon over a REAL unix socket: scripted request/response/stream
  sequences with deterministic fixtures (fixture workspaces, fake watcher events).
  Covers the unhappy paths the engine battery can't see: connect/reconnect, subscription
  resync after drop, slow-consumer buffer bounds, invocation cancel, daemon idle-out,
  two workspaces open concurrently (the §1 isolation claim as a test, not a sentence),
  and scope routing (a `.grazelrc` `service_scope` binding dials the right socket and
  the hello's workspace root lands on the right handle — §1e as a test).
  **The CLI corollary of client #1 discipline: every CLI integration test IS a server-API
  test** — the CLI has no privileged path, so its test suite exercises S-B for free, and
  a CLI-visible behavior with no transcript equivalent is a missing T1 test.
  **Grazel's posture differs by decision (Gianni, 2026-06-12): grazel INFRASTRUCTURE is
  tested END-TO-END via the `grazel ws test` command** (staged self-test through real
  CLI → scope → socket → daemon → engine; see `ws-grazel/GrazelWorkstream.md` §1), NOT
  per-endpoint — per-endpoint coverage of the shared services stays razel-side T1;
  grazel consumes those services and never re-tests them endpoint-by-endpoint.
- **T2 — the engine battery.** What exists today: `cargo test --workspace` + gates +
  probe sentinels. Unchanged, still the bulk of the pyramid; the daemon work must keep
  razel-loading runtime-free so T2 never grows an async dependency.
- **T3 — strict-mode examples goldens.** The `third-party/examples` corpus under
  `--strict_bazel`, graph + output parity vs tools/bazel-7.7.0 byte-diffed (the V3sh1 §2
  harness). This is the Bazel-compat contract of §3 enforced mechanically; tiers go
  green in S4 and ratchet — a tier once green never regresses.
- **T4 — the TF floor.** Full TF sweep ≥ 455/835 at every bank (the depth verifier;
  re-baselined only at S6 with `--deleted_packages`, per the spike's decision points).
- **T5 — gryth acceptance.** The §5 MVP slice as ONE executable end-to-end test: open
  workspace → build the gryth-shaped TS server via razel → subscribe a view → touch a
  file → assert the view delta arrives. This is the spike's definition of done running
  in CI, and the first test written against the gryth-bootstrap bar (S1–S3 exit).

Ownership mapping: T0/T1 land with S3 (the server skeleton brings its harness with it —
no skeleton without transcripts); T2 guards every step from S1; T3 lands with S4; T4 is
continuous; T5 closes the bootstrap bar. S1's own exit fixture (E-mode workspace loads;
XOR errors; strict mode hides E-packages) is a T2 citizen.

## §7 Anti-goals

No remote execution protocol (REAPI) claims; no Bazel-server wire-compat (Bazel's command
protocol is private and version-entangled — mimicking it would chain razel to internals
Bazel itself won't stabilize); no multi-workspace federation in v1 (grazel's iroh arc owns
that — and it ships as grazel, never as a razel feature).
