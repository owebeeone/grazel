# GrazelWorkstream — phases GR0–GR5 (the gryth-backend lane)

*2026-06-12. The work-item plan for the GRAZEL side of the two-tree split
(`RazelPublicSurfaces.md` §1d): a separate agent executes this in the `razel-grazel/`
clone, forked from the razel tree's post-S0 state. The razel/bazel lane's plan is
`dev-docs/ws-razel/RazelReleaseSpike.md` (V3sh1). Shared DESIGN lives at the dev-docs
root — `RazelPublicSurfaces.md` is the contract this workstream implements (§1b
distributions, §1d crates, §1e service scopes, §4/§4b taut + streaming).*

**Naming note:** "grazel" here = the razel+gryth-node DISTRIBUTION (PublicSurfaces §1b,
decided 2026-06-12). The older `dev-docs/GrazelProposal.md` / `GrazelForecast.md` use
"Grazel" for an unrelated earlier idea (Model G, a clean-slate build surface) — they are
NOT this workstream's design and are banner-marked accordingly.

## §0 Ground rules (non-negotiable)

- **Ownership:** this tree creates and owns `grazel-*` crates ONLY. It NEVER modifies a
  `razel-*` crate. A needed seam change (razel-cli lib API, razel-daemon lib,
  razel-wire IR) is a REQUEST to the razel side, landed there and pulled — the crate
  dependency arrow as a labor arrow. The CI deny rule (no razel-* → grazel-*/iroh dep)
  must stay green in this tree too.
- **TDD, e2e-grained (Gianni, 2026-06-12):** the grazel infrastructure is verified
  END-TO-END through the real CLI against a real daemon over a real socket — via the
  **`grazel ws test`** command — NOT by per-endpoint tests. Every GR step below is
  delivered as ws-test STAGES WRITTEN FIRST (red), then implementation to green.
  Unit tests in `grazel-cli-lib` are welcome where logic warrants, but ACCEPTANCE is
  always a ws-test stage. (Per-endpoint coverage of the shared services remains the
  razel side's T1 transcript suite — this workstream consumes those services, it does
  not re-test them endpoint-by-endpoint.)
- **Taut everywhere:** ALL protocol payloads — UDS IPC, HTTP bodies, WS frames — are
  taut messages (deterministic CBOR from the `wire/razel.taut.py` IR). There is never a
  JSON shadow protocol; HTTP carries the SAME bytes UDS carries. IR changes land
  razel-side (both trees consume the generated types).
- **Iroh: coming, not discussed.** `grazel-node` exists as the placeholder crate;
  nothing in GR0–GR5 designs iroh, and nothing may bake in an assumption that
  contradicts per-scope identity (§1e). The iroh arc gets its own phase doc when it
  opens.
- Process: each GR step lands green (`cargo test --workspace` + `grazel ws test` on the
  fixtures), is committed and tagged `grazel/GR<n>`, and records debts in this folder
  (not in ws-razel's RazelGaps).

## §1 `grazel ws test` — the harness itself

The self-test verb and this workstream's acceptance instrument. `grazel ws test` runs
in (or against) a workspace and executes the STAGED end-to-end checks accumulated by
the GR steps, reporting one line per stage (name, PASS/FAIL, duration) and exiting
nonzero on first principle: any failure. Properties:

- Stages are registered in `grazel-cli-lib` (declarative stage list — stage = name +
  fixture requirements + check fn); CI runs the full ladder; `--stage=<name>` runs one.
- It tests INFRASTRUCTURE end-to-end (CLI → scope resolution → socket → daemon →
  workspace handle → engine → stream back → render), deliberately NOT endpoints in
  isolation — a stage that could pass against a mock is wrongly written.
- It is itself business logic in the lib (thin-bin rule §1d) — runnable in-process by
  `cargo test` AND as the shipped verb; the same stages serve both.

## §2 The phases

**GR0 — fork & baseline.**
Precondition: S0 (plan 0) has landed in the razel tree — razel-cli lib split,
`grazel-cli` + `grazel-cli-lib` stubs, deny gate, byte-identical fixture test.
Scope: clone → `razel-grazel/`; full suite green in the clone; deny gate RE-red-tested
here; the sync ritual defined and exercised once (pull razel main → rebuild → suite
green; documented in this folder's README).
Exit: green tree, one proven sync round-trip, `grazel ws test` exists as a verb with a
single trivial stage (`harness-selftest`) so the ladder has a rung to grow from.

**GR1 — grazeld bring-up (default scope) + ws-test v1.**
Scope, stages first: `daemon mode` entry (`grazel daemon run --scope=default`, riding
the razel-daemon LIB — allowed direction); scope filesystem contract from §1e
(`~/.grazel/.uds/default` socket, `~/.grazel/scopes/default/` with daemon.json
{pid, build+wire versions}, launch lock); autostart-on-dial; taut hello carrying
build/wire versions + WORKSPACE ROOT; graceful shutdown verb; idle-out timer.
ws-test stages added: `cold-autostart` (no daemon → dial → daemon up → hello),
`warm-dial`, `version-handshake` (wrong wire version → scope-local restart),
`stale-socket-recovery` (kill -9 → dead-pid detect → clean → relaunch),
`graceful-shutdown`, `idle-out`.
Exit: all GR1 stages green from BOTH cold and warm starts on a fixture workspace;
no razel-* crate diffs.

**GR2 — service scopes for real.**
Scope: `.grazelrc` parsing (the grazel-only rc layer: `service_scope=<name>`; a grazel
key in `.razelrc` is an ERROR per §1e); override chain `--scope` > `GRAZEL_SCOPE` > rc >
`default`; per-scope state dirs + per-scope daemons; scope config with PINNED workspaces
(pre-opened at daemon start) + DYNAMIC membership (Lifecycle.open, refcounted,
idles out); cross-daemon output-base lock honored (mis-bound workspace fails loud
naming the holder).
ws-test stages: `two-scopes-concurrent` (two daemons, distinct sockets, isolated state),
`scope-routing` (command in workspace bound to A reaches A's daemon — hello root lands
on the right handle), `pinned-membership`, `dynamic-membership-idle-out`,
`double-claim-fails-loud`, `grazel-key-in-razelrc-errors`.
Exit: stages green; two fixture workspaces in different scopes build concurrently.

**GR3 — verbs + streaming pass-through.**
DEPENDS razel-side: S3 (Command service, `run` verb, sched_hook progress events on the
wire). Expect seam-request traffic on razel-wire here — IR grows razel-side, pulled.
Scope: `grazel build/query/run` end-to-end against grazeld via the shared razel-cli lib
over the scope socket; invocation-id discipline (immediate id, results as events);
streamed progress rendering ("300/2000 BUILD files loaded" — the §4b contract).
ws-test stages: `build-streamed` (fixture build; assert id-before-events, progress
events strictly before completion, per-invocation ordering), `query-snapshot` (query
answers from last committed snapshot during a running build), `run-verb`.
Exit: stages green; `grazel build` output on the cpp-tutorial fixture byte-identical to
`razel build` (the §1d shared-lib claim, re-proven over the daemon path).

**GR4 — the HTTP/WS edge (taut payloads, transport equivalence).**
Scope: per-scope localhost HTTP listener (port recorded in daemon.json); one-shot calls
as taut bodies, streams as taut WS frames — the SAME messages as UDS, byte-for-byte
(transport equivalence IS the contract; no JSON anywhere). LOCALHOST ONLY in this
phase; any non-local bind waits for auth (iroh-era, out of scope). The GLADE layer
(how this edge is PRESENTED to gryth) stays TBD by the glade arc — this phase builds
the transport face only and must not invent presentation semantics.
ws-test stages: `http-equivalence` (same hello/query over HTTP returns byte-identical
taut payload as over UDS), `ws-stream-equivalence` (WS subscription sees the same event
sequence as a UDS subscription), `http-localhost-only` (non-local bind refused).
Exit: equivalence stages green — provable "one protocol, three transports".

**GR5 — the gryth attach point.**
Scope: generate the TS client from the SAME taut IR (taut's TypeScript backend — the
§4 promise made real); a node smoke client (host node, non-hermetic digest-logged
posture) that connects over WS, opens a workspace, subscribes a View, and receives a
delta after a file touch. This is the gryth-dev BOOTSTRAP moment (the spike's decision
point): from here gryth builds against grazel for real, and T5 (gryth acceptance)
becomes implementable.
ws-test stages: `ts-client-roundtrip` (spawn the node smoke client as a subprocess;
assert hello + query decode), `view-delta-roundtrip` (touch → invalidate → delta
reaches the WS client).
Exit: stages green; gryth-dev unblocked end-to-end.

## §3 Dependencies & risks (named)

- **GR3 blocks on razel-side S3.** GR0–GR2 are buildable against today's razel-daemon
  lib + a minimal hello; sequence the trees so neither idles.
- **Seam-request latency** is the split's tax: razel-wire IR and razel-cli lib changes
  must land razel-side first. Mitigation: batch requests per GR step; keep them small.
- **Host node dependency** (GR5): same non-hermetic posture as ws-razel S3 — digest-log
  it, don't pretend hermeticity.
- **Name collision** (GrazelProposal/Forecast = old Model G): banner-marked; do not
  cite them as this workstream's design.
