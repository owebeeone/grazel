# RazelDevStatus — the developer's current-state design (2026-06-18)

**What this is.** The single-entry, *living* description of how razel is designed **and what is
actually built**, for a developer or agent picking up the codebase. It distills the V2
architecture-of-record — `history/RazelV2FinalArchProposal.md` + `history/RazelV2Contracts.md` (now
archived; read them for the full designed substrate + the seam-contract specs) — and reconciles it
with the V3 reality (the `RazelCrateUniverse*` workstream that actually shipped). **When this doc and
an archived V2 doc disagree, this doc wins for "what is true now"; the V2 docs win for the full
designed substrate.** Companions: `RazelCodingRules.md` (enforceable rules), `ws-razel/RazelGaps.md`
(the gap backlog + bazel feature-parity inventory), `RazelCrateUniverseDesign.md` (the live
`@crates`-build + `razel query` design, owner RR).

---

## 1. What razel is (the mission — unchanged from V2 §0)

razel exists **for grip-lab** — a distributed, human+AI-agent IDE. The product is a
**bazel-compatible build-graph *derivation server*** (over iroh p2p, serving MCP agents + a UI):
producing binaries is *one* derivation among many (IDE/LSP index, affected-sets, lint, coverage,
provenance). **Bazel compatibility is input-fidelity table-stakes; the derivation server is the
differentiator** over "just another Bazel."

**The current proof (live + gated).** razel **self-hosts**: `razel build //crates/razel-cli:razel`
with NO Bazel builds the razel binary — the full ~150-crate `@crates` closure + ~22 workspace crates
+ the final link — and `razel query` matches live `bazel query` across the whole supported verb
surface. (`RazelCrateUniverseCheckpoints.md`.)

---

## 2. The shape (the designed architecture — V2 §1/§3)

The **canonical contract is the center**: every input surface *lowers into* a typed, serializable
fact graph; the engine consumes only it; the build is one consumer (binaries, MCP query/explain,
F17 derivations all read the same contract).

```
  L5 surfaces    razel-cli · razel-daemon · razel-mcp(query/explain) · razel-mesh(iroh)
  L4 adapters    razel-adapter-bazel  (Starlark eval + effect-capturing ctx → facts)  ← imperative boundary
  L3 dialect     razel-rulepack  (facet API · pure lower · kernel primitives fold_deps/match_toolchain)
  L2 engine/exec razel-engine (demand graph · typed node values) · razel-actions/-exec (action kernel)
  L1 SPINE     ► razel-dds ◄  facts · keys/identity · ProviderSchema · merge-classes · DdsRead⊥DdsWrite
  L0 primitives  razel-core (Digest, RepoId, encodings) · razel-wire (taut/CBOR codec)
```

**The one dependency rule (CI-enforced):** everything depends **down** toward the DDS spine
(`razel-dds`, L1); the spine depends on nothing above it (core + wire only). That inversion is what
stops the spine becoming the next gravity well. See §6 for what's actually enforced today.

---

## 3. The decision record (AD1–AD9) — with *current* status

The architectural decisions (V2 §2) are still the record. The status column is the honest reconcile
with V3 — **D**esigned (specced, not the live path), **P**artial, **L**ive (on the running path),
**L+CI** (also gate-enforced).

| AD | Decision (abbreviated) | Status |
|----|------------------------|--------|
| AD1 | Canonical typed/serializable contract is the center; the engine never knows "cc_library" | **P** — the live loader uses a typed loaded graph (`RawAttr`→`LoadedTarget`/`AnalyzedTarget`), not the full DDS fact DB |
| AD2 | No ambient state; the DDS is the one explicit passed store; `thread_local!`/`static mut` banned | **L+CI** — `xtask gates` denies ambient state across `crates/` |
| AD3 | Forcing: razel-authored producers are pure, get read-only `DdsRead`, *return* facts — imperative doesn't compile | **D** — the type-level `DdsRead⊥DdsWrite` wall lives in `razel-dds` (parked); the live loader's producers are pure-ish but don't route through the DDS write seam |
| AD4 | yidl-lite rule packs = declarations (schema + pure lower); kernel built once, packs added in parallel | **D/parked** — `razel-rulepack` exists but is unwired; live rules are **native Rust reimplementations** |
| AD5 | Bazel is an *adapter*, not the core (effect-capturing `ctx` → facts) | **P** — razel runs Starlark BUILD/`.bzl` + a `rule()`/`provider()`/`aspect()` engine (L2/L5 **MVP**), but the standard rulesets are native reimplementations + host-repo stubs, not the fetched `.bzl` |
| AD6 | Typed, serializable providers (`ProviderKey`); ship/merge across the mesh | **D** — `razel-wire` taut/CBOR codec is live; the mesh fact-substrate is future |
| AD7 | One demand-driven engine; CLI + daemon route through it | **P** — analysis is demand-driven (targets analyzed on demand); the full typed-node `razel-engine` model is the designed execution substrate |
| AD8 | Cross-platform = multi-instance, not in-graph `(Target×Config)`; `TargetKey` carries `AnalysisInstanceId` | **D** — live is **host==target, single-instance**; cross-compile is a named gap |
| AD9 | Sound, bounded composition (additive-default + declared merge-classes + definition-time confluence) | **D** — live `select`/merge is the pragmatic subset; the merge-class engine is parked with the spine |

---

## 4. Designed vs built — the honest gap (read this before trusting the diagram)

The V2 substrate above is the **designed** architecture. V3's `RazelCrateUniverse` workstream built
the **`@crates`-build + query proof on a pragmatic path** that realizes part of it and **parks** the
rest. The three layers, by reality:

- **LIVE path = `razel-loading`.** A Starlark loader that captures a typed loaded graph
  (`RawAttr` → `LoadedTarget`/`AnalyzedTarget`), **hand-rolls** the transitive fold, and reimplements
  the standard rules (rust/cc) natively. It does **NOT** depend on `razel-dds`.
- **PARKED parallel spine = `razel-dds` + `razel-rulepack`.** The schema-driven fact DB + provider
  engine **exist** but have ~zero live callers (`razel-dds::DdsRead::fold_depset` is unused outside
  its own tests). Wiring the loader through the spine — so tested==run and the per-field fold
  duplication collapses — is the Phase-C "provider-map" epic. Tracked: **coding rule 4 (no stranded
  infra)** + `RazelGaps.md` "Parallel-spine reconciliation." *This is a deliberate park, not silent
  drift — but a developer must know the live loader is not the DDS spine.*
- **Aspirational = the product.** The derivation server (MCP/F17), the iroh mesh, multi-instance
  config. The DDS boundary (§6) keeps these cheap to add later — they sit *above* the substrate.

---

## 5. Current build / feature state

- **Build:** Phases 0–5 done; **Phase 6 closed** (query parity consolidated; query verbs all ride
  live-bazel parity). Self-hosts; outputs land in a clean `razel-out`/`bazel-out` tree (source
  pristine).
- **Query:** load-only `razel query`, live-bazel parity for
  `deps`/`rdeps`/`kind`/`filter`/`attr`/`labels`/`somepath`/`allpaths`/`siblings`/`same_pkg_direct_rdeps`/`tests`/`visible`
  and `--output` `label`/`label_kind`/`package`/`graph`. Accepted query **deviations** are documented
  in `RazelCrateUniverseDesign.md` §12 (e.g. `buildfiles`/`--output=build` — razel stubs the rule
  `.bzl`).
- **Languages:** rust (deep), cc (native, two backends — only Adopt-Bazel is golden-tested), js/py/sh
  (partial), **java (carve-out** — the two standing graph-parity reds).
- **Gaps & non-goals:** the verified **bazel feature-parity inventory** in `ws-razel/RazelGaps.md`
  (cross-compile, hermetic toolchain fetch, visibility *enforcement*, `rust_test` execution, RBE,
  bzlmod resolution, `.bazelrc`).

---

## 6. Enforced invariants (the live, CI-checked subset — `cargo xtask gates`)

These are the V2 invariants that are **actually enforced today**:

- **No ambient state** anywhere in `crates/` (AD2; `thread_local!` / `static mut` banned, env-derived
  state threaded through `Session`/`GlobalFlags`).
- **`razel-dds` boundary intact** — the spine imports core + wire **only** (the L1 dependency rule).
- **`razel-query` reads-only** — no analysis dependency (the load-only query seam, P1.0).
- **No language name in the engine core** (C3c — generic machinery projects by the consuming rule's
  declared schema, never branches on per-language providers).
- **grazel arrow intact** — no `razel-*` → grazel/iroh dependency (the distribution layer stays
  strictly downstream).

The rest of the V2 invariants (the type-level forcing wall, the full determinism/CBOR-fixture
discipline) are **designed in the parked spine**, not yet on the live path. The enforceable code-level
rules distilled from V2 live in `RazelCodingRules.md`.

---

## 7. Where to read what

| Topic | Doc |
|-------|-----|
| Current design state (this doc) | `RazelDevStatus.md` |
| The live `@crates`-build + `query` design + query deviations | `RazelCrateUniverseDesign.md` (§12 deviations) |
| Per-rung build record (Phases 0–6, commit hashes) | `RazelCrateUniverseCheckpoints.md` |
| Enforceable coding rules (incl. the V2 invariants distilled) | `RazelCodingRules.md` |
| Gap backlog + the bazel feature-parity inventory | `ws-razel/RazelGaps.md` |
| The full designed substrate (DDS spine + seam contracts) | `history/RazelV2FinalArchProposal.md` + `history/RazelV2Contracts.md` |
| The release/distribution (grazel) direction | `RazelPublicSurfaces.md`, `ws-razel/RazelReleaseSpike.md` |
