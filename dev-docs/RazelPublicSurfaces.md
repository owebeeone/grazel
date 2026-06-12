# RazelPublicSurfaces — the formal public APIs (CLI + server)

*2026-06-12. Decision (Gianni): gryth requires a local server working the workspace —
razel adopts the same model (as Bazel does internally) and, UNLIKE Bazel, makes the server
API a FORMAL, VERSIONED, THIRD-PARTY surface. Companion to `RazelReleaseSpike.md` (V3sh1),
which builds the first slice; subordinate to `RazelV3Plan.md`'s invariants.*

## §1 The model

A **resident workspace server**: one razel server per (workspace root, output base),
auto-started by the first client, owning the hot state — the loaded/analyzed graph, the
caches, file watches — with idle shutdown and a `--batch` escape (Bazel-parity lifecycle).
Clients: the **razel CLI** (client #1), **gryth** (the driving consumer — graph oracle,
builds, events for its agent/IDE surface), and any third party.

**The architectural rule that keeps the API honest: the CLI is a CLIENT, with no
privileged in-process path.** Every verb goes through the same server API a third party
would use, from the first implementation (V3sh1 S3) onward. A public API the first-party
tool bypasses rots into a second-class surface; this rule makes that structurally
impossible.

## §2 The surfaces (enumerated — nothing else is public)

- **S-A: The CLI.** Verbs (`build`/`run`/`test`/`query`/`fetch`), flags (the
  Bazel-compatible set with Bazel semantics + razel-only flags incl. `--strict_bazel`),
  exit codes, output contracts. Bazel-compat portions track Bazel's semantics (strict mode
  is the oracle); razel-only portions version with the server API.
- **S-B: The server API** (the third-party programmatic surface). Four services:
  - **Command** — build/run/test/fetch invocations with structured results.
  - **Query** — the graph oracle: targets, deps/rdeps, providers, actions, packages;
    razel-analysis's existing machinery formalized.
  - **Events** — a structured build-event stream, **BEP-aligned** (Bazel's Build Event
    Protocol is its one public server-ish surface; emitting BEP-compatible events buys the
    existing ecosystem's tooling and is itself a bazel-compat claim, golden-testable).
  - **Lifecycle** — workspace open/status/invalidate, file-watch subscription, shutdown.
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

## §4 Protocol direction (decided enough to build; finalized at S3)

Service definitions are TRANSPORT-AGNOSTIC; the first transport is LOCAL (unix domain
socket). The wire-format decision is taken at S3 with one thumb on the scale: **gryth is
ts/npm**, so the protocol must be first-class from node (JSON-RPC or connect/gRPC-web
shaped both qualify; a hand-rolled JSON-RPC over UDS is an acceptable first cut that can
gain a schema'd transport later — the service SHAPE is the contract, the framing may
harden). Future transports ride the same services: the iroh path (remote/distributed
workspace access, the gryth architecture's native fabric) is explicitly anticipated and
explicitly NOT v1. BSP (Build Server Protocol) is noted as a possible later ADAPTER over
S-B for IDE ecosystems — never the native surface.

## §5 Gryth's MVP slice (what the driving consumer needs first)

Open workspace → build/run/test a target → query deps/rdeps/targets → subscribe to events
→ invalidate on file change. This slice IS V3sh1 S3+S5's server: the `run` verb lands as
the Command service's first method with the CLI as its first client; Query formalizes
what razel-analysis already computes; Events can begin as the existing progress lines
structured, growing toward BEP.

## §6 Anti-goals

No remote execution protocol (REAPI) claims; no Bazel-server wire-compat (Bazel's command
protocol is private and version-entangled — mimicking it would chain razel to internals
Bazel itself won't stabilize); no multi-workspace federation in v1 (the iroh arc owns
that).
