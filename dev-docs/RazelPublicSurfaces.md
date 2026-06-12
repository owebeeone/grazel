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
  - **Events** — a structured build-event stream, BEP-*shaped* (modeled in taut; a
    literal-protobuf BEP adapter is a later optional bridge to bazel-ecosystem tooling —
    see §4).
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
