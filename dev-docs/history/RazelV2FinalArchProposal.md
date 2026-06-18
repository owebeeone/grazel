# Razel V2 — Final Architecture Proposal (Architecture of Record)

**The architecture of record is this document *plus* `RazelV2Contracts.md`** (55-2 H2 — the
contracts carry the load-bearing seam specs; don't read this alone). Distilled from
`ArchFundamentals.md`, `ArchBazelConstraints.md`, `ArchPatterns.md`, `ArchProposals.md`,
`GrazelForecast.md`, `GrazelProposal.md`, `ArchitectSkillRules.md`, `YIDLDigest.md`; the
realization plan is `RazelV2FinalArchProposalPlan.md`.

---

## 0. What razel V2 *is* (and is not)

razel exists **for grip-lab** — a distributed, human+AI-agent IDE. razel V2 is therefore a
**distributed, agent-facing build-graph *derivation server*** (over iroh p2p, serving MCP
agents and a UI), **not** a Bazel CLI clone. Producing binaries is *one* derivation among
many (IDE/LSP index, affected-sets, lint, coverage, provenance). **Bazel compatibility is
table-stakes input fidelity; the product is the derivation server** (the differentiator
over "just another Bazel").

---

## 1. The shape (one diagram)

```
  INPUT SURFACES (adapters)            CORE (the kernel)                 OUTPUT SURFACES
  ┌───────────────────────┐                                          ┌────────────────┐
  │ Bazel BUILD/.bzl       │──lower──┐                          ┌────▶│ binaries (a     │
  │  (imperative, mandated)│         │                          │     │  derivation)    │
  ├───────────────────────┤         ▼      ┌──────────────┐     │     ├────────────────┤
  │ Grazel .razel descrip. │──lower─▶│ CANONICAL CONTRACT │────▶│     │ MCP: query /    │
  │  (declarative; later)  │         │  typed, serializable│ Engine    │ explain /       │
  ├───────────────────────┤         │  fact graph (taut)  │ (demand-  │ provenance      │
  │ MCP transactional edit │──lower─▶│  + provenance       │ driven,   ├────────────────┤
  ├───────────────────────┤         │                     │ cached)   │ F17 derivations │
  │ deterministic inference│──derive─┘      └──────────────┘     └────▶│ (IDE/lint/...)  │
  └───────────────────────┘                       ▲                   └────────────────┘
                                   RULE PACKS (declarative, parallel-authored)
                                   schemas + lowering + matchers + inference
                                   cc · rust · py · sh · skylib · …  (one file + 1 row each)
```

The **canonical contract is the center**; every surface *lowers into it*; the engine
consumes only it; derivations and execution read it. The build is one consumer.

---

## 2. Architectural decisions (the record)

- **AD1 — The canonical contract is the architecture's center.** A **typed, serializable**
  (taut/CBOR) build contract — packages, targets, typed dependency edges, **typed
  providers**, declared inputs/outputs, toolchain/platform constraints, config dimensions,
  lowered actions, and **provenance for every fact**. All surfaces lower into it; the
  engine knows only it (never "cc_library"). *The IR is the product.*
- **AD2 — No ambient state; the DDS is the one explicit store.** The analysis state is the
  **DDS (Data Definition System)** — an **in-memory typed fact database**, the declaration
  registry, the denormalization point, and the data source every query/matcher reads
  (`RazelV2Contracts.md` §0). The `Analysis`/Session is a **DDS instance handle** (carried via
  the evaluator's `extra` seam). **"No ambient state" ≠ "no mutable state"** — the DDS *is*
  mutable ("kind of imperative": facts are **asserted** into it), but it is the single
  **explicit, passed, scoped, plural** store, not a thread-local/global. **`thread_local!`/
  `static mut` are mechanically banned** (CI deny). Forcing (AD3) holds via the **producer/
  store split**: producers (`rule_pack.lower`, matchers) are pure and *return* facts over a
  read-only `&FactView`; only the assembler/evaluator **asserts** them (the one controlled
  imperative seam, under the §3 merge discipline). This is what makes many graph instances —
  in one process and across the mesh — possible.
- **AD3 — Forcing: declarative-by-construction for razel's *own* authoring.** Native rule
  packs, derivations, and matchers are **pure, closed, returning** contracts —
  `lower(target, &DdsRead) -> Declared{providers, actions, facts}` — receiving a **read-only
  `DdsRead`/`FactView`** (never the `DdsWrite` handle, 55-3 B1) and **returning** their facts;
  no reachable mutable store, no editable core. The imperative shortcut *does not compile*.
  (The YIDL test: the easy path is the *forced* path.)
- **AD4 — The yidl-lite rule-pack layer is the support infra (the enabler).** A rule pack
  is a **declaration**, not imperative code: it declares target-kinds + attr schemas +
  provider schemas + a pure **lowering** (facts → actions) + optional inference/validation,
  and composes kernel-provided primitives (transitive fold, toolchain/`select` matchers).
  The kernel is built **once**; rule packs are added **in parallel, by declaration** (see §4).
- **AD5 — Bazel is an *adapter*, not the core.** The Bazel `BUILD`/`.bzl` surface is
  *mandated imperative* (we don't own it), so it is the one place forcing can't apply: it's
  a **lowering adapter** that evaluates Starlark and turns `rule()`/`ctx.actions`/providers
  into contract facts + rule-pack invocations. The core never absorbs Starlark/Bazel
  packaging assumptions.
- **AD6 — Typed, serializable providers.** The inter-unit contract is a **typed provider
  with stable identity (`ProviderKey`)**, serializable so it ships/merges across the iroh
  mesh. (`razel-wire` taut/CBOR expands from daemon-RPC to *the distributed fact
  substrate*.) Consumers name a provider contract, never a producer.
- **AD7 — One demand-driven engine on the live path.** The incremental graph engine
  (content-addressed, early-cutoff) is *the* execution model; **both CLI and daemon route
  through it** (no straight-loop bypass). Parallel *execution* is a budgeted later step (a
  real `Send+Sync` rewrite); parallel *analysis* is deferred.
- **AD8 — Cross-platform = multi-instance, not in-graph config.** N single-config graph
  instances (one per platform/VM) + **cross-instance derivation** over the mesh — **not**
  Bazel-style `(Target×Config)` transitions inside one graph. The **stored/serialized
  `TargetKey` always carries `AnalysisInstanceId`** (`{instance, label}`); a label-only key
  is just in-instance shorthand, never globally "this label under all configs" (55-2 H2/B4).
  Full in-graph `(Target×Config)` and arbitrary transitions are deferred (a bounded exec/host
  transition produces a *second instance*, only if forced).
- **AD9 — Sound, bounded composition (F25).** Cross-module behavior-change
  (`select`/aspects/derivations/override) is **additive-by-default** (confluent), with
  behavior-change only via **schema-declared merge-classes at explicit precedence scopes**
  (never "last file wins", never silent global mutation), **confluence checked at
  definition time** (start Eq-decidable: `select`/toolchain), **provenance mandatory**, and
  the general non-monotonic case **deferred** — not a general logic engine.

---

## 3. Crate & boundary model (the structure the reviews derived)

The iterative review process didn't just find bugs — it **derived the architecture's real
internal boundaries**. Consolidated, the kernel is a **layered crate DAG with one dependency
rule** (55-4 B6): *everything depends down toward the DDS; the DDS depends on nothing above
it.* That inverted dependency is what stops the spine becoming the next gravity well.

```
  L5 surfaces      razel-cli · razel-daemon · razel-mcp(query/explain/provenance) · razel-mesh(iroh export/merge)
                        │ depend on engine + adapters + dds
  L4 adapters      razel-adapter-bazel  (Starlark eval + effect-capturing ctx → facts)   ← THE imperative boundary
                   (future: grazel-descriptors, mcp-txn)        │ depend on rulepack + dds (+ starlark)
  L3 dialect       razel-rulepack  (facet API · pure lower · capability negotiation · kernel
                   primitives fold_deps/match_toolchain · bounded matcher/derivation evaluator)  │ depends on dds (DdsRead only)
  L2 engine/exec   razel-engine (demand graph · node taxonomy · *uses* dds snapshots, stores
                   ReadSetDigest as memo metadata) · razel-actions/-exec (action kernel)   │ depend on dds + core
  L1 SPINE         ► razel-dds ◄  facts · keys/identity · ProviderSchema · merge-classes ·
                   DdsRead⊥DdsWrite · transactions · snapshot+read-set *mechanism* · query
                   ordering · export/merge validation · provenance      │ depends on core + wire ONLY
  L0 primitives    razel-core (Digest, RepoId, encodings) · razel-wire (taut/CBOR codec)
```

**`razel-dds` is an independent crate (55-4 B6), and the dependency rule is CI-enforced:**
it owns fact/key/schema/provenance types, the read/write traits, merge-classes,
transactions, deterministic query ordering, and import/export validation — and it **must not
import** adapter, rule-pack, engine, MCP, mesh, CLI, daemon, exec, or Starlark code. A
boundary check fails CI on a forbidden import. The DDS is reusable in-process by analysis,
derivations, MCP explain/query, and mesh merge **without importing any of those surfaces**.

**The boundaries derived (each a hard seam, not a convention):**
- **`DdsRead` ⊥ `DdsWrite`** (the producer/store wall, type-level) — producers get read-only;
  only commit code mutates (AD3, §0).
- **declaration stratum ⊥ analysis stratum** — two fact namespaces; the declaration set has
  its own identity (`DeclarationSetId`, incl. rule-pack *code* digest) that is part of
  analysis identity.
- **live store ⊥ per-node snapshot/read-set** — an engine node computes against a frozen
  snapshot; its read-set is its memo key (the anti-ambient-state seam).
- **imperative Starlark ⊥ pure contract** — *all* effect-capture happens in the adapter
  (L4); everything below it (L0–L3) is pure over facts. The single imperative boundary in
  the stack.
- **transaction boundary** — `commit`/`merge` are atomic (no partial fact sets).
- **mesh boundary** — an `ExportBundle` carries facts *plus* the declarations needed to
  validate them; nothing else crosses a node.

**Honest build status of each crate (verified against code, Review-48 + 48-3):**
- *Genuine reuse:* `razel-exec` sandbox/CAS; the **taut/CBOR codec *mechanism*** in
  `razel-wire`.
- *Rebuild / unify (existing-but-not-a-base):* `razel-engine` (`String`→`Digest` toy → typed
  node values); `razel-vfs` (`ContentProvider` only → the loading nodes are new); the
  **`Action` model** — `razel-actions::Action` plus the *ad-hoc* `ActionNode`s that
  `razel-build`/`-exec` actually carry today are **unified** into one `ActionKey`-keyed
  contract Action (not greenfield, not clean reuse — a unify); the **`Target`/`Label`**
  identity — orphaned `razel-core::Label` vs the live `String` `canon_label` — likewise
  unified into the typed keys (48-3 B-3).
- *Greenfield (does not exist):* **`razel-dds`**; the provider type system + `provider()` +
  rule-impl return-capture (return discarded, `attrs` ignored, only a side-effecting
  `DefaultInfo`); `razel-rulepack`; the adapter's fact-capture; **the fact/`ExportBundle` IR +
  merge codec** (the taut *substrate* is reuse, the fact codec itself is new — 48-3 HR).

**Rule packs** (cc, rust, py, sh, skylib, config-repos) are declarations against
`razel-rulepack` (L3) — one file + one manifest row each, parallel-authored (§4).
**Surfaces:** Bazel adapter now; MCP query/explain/provenance (mission); Grazel `.razel`
descriptors are a deferred option the clean contract keeps free.

---

## 4. The enabler: how rule-pack support parallelizes (the anti-bottleneck)

The whole point of AD3/AD4: **build the support structure once, then add Bazel/`.bzl`
support at speed, in parallel.** A rule pack is a *declaration* against a stable API:

```
rule_pack "cc" {
  target_kind "cc.library" { attrs = { srcs: label_list(files), hdrs: ..., deps: label_list, copts: string_list } }
  provider    "CcInfo"     { headers: depset<File>, libs: depset<File>, cflags: set<string> }
  lower(target, a) -> Declared {            // PURE; no side effects, no globals
     toolchain = a.match_toolchain("cc")    // kernel matcher (Eq-decidable, confluent)
     objs = target.srcs.map(s => compile_action(toolchain, s, fold_cflags(target, a)))  // kernel primitives
     lib  = archive_action(toolchain, objs)
     return Declared{ actions: objs+[lib], providers: [CcInfo{ ... fold_deps(target, a) ... }] }
  }
}
```

Because the contract is **closed and pure** (AD3) and the kernel provides the hard
primitives (`fold_deps`, `match_toolchain`, action templates), **a rule pack cannot reach
into the core, cannot hold global state, and cannot take an imperative shortcut** — so two
agents authoring `rust` and `py` packs *cannot collide* (disjoint files + one manifest row
each). This is exactly the earlier rust/py/sh fan-out, now the *intended* model at scale:
**N agents flesh out stock `.bzl` support concurrently against the frozen kernel.** Getting
to "build TensorFlow-class repos" becomes a throughput problem (more packs), not a
serialized one — *once the kernel + the cc reference pack prove the API.*

---

## 5. Invariants (true of every part)

1. **No ambient state** — state is passed (`Analysis`); thread-locals/statics banned (AD2).
2. **Forcing** — razel-authored extension points are pure, closed, returning; imperative
   doesn't compile; ambient state is CI-denied (AD3). *Acceptance test:* an agent cannot
   extend razel imperatively and have it build.
3. **Quarantined effects (two kinds).** *Loading* reads — BUILD/`.bzl`, `glob`,
   repo-mapping, workspace paths — go through a **content-addressed VFS / source
   snapshot** and are **engine invalidation keys**; the **action kernel** is the only
   *process-execution* effect. Everything else is pure. (Caching/parallelism/distribution
   fall out *only if loading reads are graph inputs, not ambient* — Review-55 B2.)
4. **Clean front-end→contract seam** — the engine knows action/target/provider, never a
   Bazel rule name (AD1/AD5).
5. **Typed, serializable contract** — providers/facts are typed + taut-serializable so they
   ship/merge across the mesh (AD6).
6. **Bounded sound composition** — additive-default + declared-merge + definition-time
   confluence + provenance; no general non-monotonic engine (AD9).

---

## 5b. Required seam contracts (Review-55 hardening — Phase-2 deliverables, not slogans)

The direction is sound; these are the load-bearing seams that must be *written contracts*
before Phase-2 code and proven by the `cc` gate. (Each maps to a Review-55 requirement.)

- **Adapter = the effect-capturing boundary (B1; `REQ-ADAPTER`).** V2 **executes** evaluated
  Starlark `rule()` implementations via an **effect-capturing `ctx`** that records
  `ctx.actions.*` and returned providers into the contract. **Greenfield, not "redirect
  existing machinery" (Review-48 BL-1, verified):** today `rules.rs` *discards* the impl's
  return (`:564`), has **no `provider()`/typed `Provider`** (`DefaultInfo` is a side-effecting
  builtin, `:701`), and *ignores* `attrs` (`:587`). Only the state-store half (thread-local→
  Session) is a redirect; `provider()` + typed `Provider` + return-capture + the attr-schema
  are **new** (Plan Step 2.13a) and are prerequisites of this adapter. Three input
  classes, named explicitly: **(1)** macro-only `.bzl` → expands to rule-pack targets;
  **(2)** evaluated `rule()` impl → effect-capturing adapter `ctx` → contract facts;
  **(3)** native-shimmed rules (cc/rust/…) → **pure** rule pack. Shimming `@rules_cc` to a
  native pack is an *optimization*, not the only path. The adapter is explicitly **not** the
  pure rule-pack layer — it is the imperative boundary that captures effects into the
  contract.
- **Loading is a graph effect (B2; `REQ-LOAD`).** BUILD/`.bzl`/`glob`/repo-mapping reads go
  through a content-addressed **VFS/source snapshot** (`razel-vfs`); these are **engine
  invalidation keys**, so editing a BUILD, editing a loaded `.bzl`, adding/removing a
  globbed file, or changing a repo mapping invalidates *exactly* the affected nodes.
- **Contract identity (B3; `REQ-CONTRACT`).** `TargetKey`/`ProviderKey`/`FactKey`/`ActionKey`
  have **canonical encodings + equality rules**. Provider identity is **two-level** — a stable
  **`ProviderTypeId`** (Starlark: `BzlModuleKey + ProvId` of the *defining* provider object;
  native: a stable namespace) + a **`ProviderSchemaId`** (version digest); lookup matches the
  type first, then schema-compat. Depset ordering, stable sets, path normalization, and
  action argv/env ordering are **deterministic** (taut/CBOR fixtures). The **declaration
  stratum** (rule-packs/schemas/matchers) is itself part of analysis identity (`DeclarationSetId`).
  *Required because facts merge across the mesh.*
- **Analysis-instance identity (B4; `REQ-CONFIG`).** An **`AnalysisInstanceKey`** =
  **`DeclarationSetId` (incl. rule-pack code identity)** + repo-mapping + target platform +
  exec platform/toolchain context + selected config values (via a pre-analysis
  `RequestedInstanceKey` + a bounded `ToolchainResolution` node — no bootstrap cycle).
  `TargetKey` is label-like **only within one immutable analysis instance** — never globally
  "this label under all configs." **Multi-instance *is* the configuration boundary** (this
  is how AD8's "key by Label" and "the contract has config dimensions" reconcile). A bounded
  exec/host transition is in scope *only if the `cc` proof needs it* — then it is not a later
  escape hatch.
- **Engine on the live path from Phase 2 (B5; `REQ-ENGINE`).** The `cc` gate runs through
  `razel-engine`, not a straight loop. The node taxonomy is defined up front: source
  snapshot · `.bzl` load · package load · target/rule-pack analysis · action plan · action
  exec · (derivation/query). **CLI and daemon share the engine path before Phase 3.**
- **Rule-pack = capability facets, not a god-constructor (H1; `REQ-RULEPACK`).** Declarations
  compose as typed facets (target-kind, attrs, providers, lowering, matchers, validation,
  inference) — never one ever-widening rule object (the S5 scar). Deferred capabilities
  (aspects, test semantics, runfiles, exec-groups, transitions, external-repos) get
  **reserved extension points** now, even if unimplemented.
- **Declarative repository identity (H2; `REQ-REPO`).** `@repo` identity, repo-mapping, and
  canonical labels live in the contract **from Phase 2** (they cross repo boundaries
  immediately — labels, provider keys). The local-path resolver is *shaped like* a future
  lockfile-backed resolver. (Fetch is deferred; identity is not.)
- **Composition negative tests + provenance early (H3; `REQ-COMP`).** Conflict tests
  (duplicate providers/facts, incompatible merge-classes, ambiguous `select`/toolchain) and
  "why does this provider/action exist?" provenance for the `cc` graph are **Phase-2**
  deliverables, not Phase-5 distribution details.

---

## 5c. Scope of the architecture-of-record (55-5 H6)

**The V2 architecture-of-record contracts the *substrate*: the DDS spine + identity + loading
+ engine + rule-pack/adapter + the cc proof.** F17 (orthogonal derivation) and F24
(mesh distribution) — *the product* — are **downstream phases (Phase 5) enabled by that
substrate**, not yet contracted. Their thin contracts (`DerivId` identity, `Scope`, consumed
`FactView`, output-fact keying, mesh merge behavior + one F17 acceptance fixture) are written
**before Phase-5 implementation**, not now. The DDS boundary (§3) is what makes this safe:
MCP/iroh/UI/derivation specifics **must not bleed into `razel-dds`** — they sit *above* the
fact substrate. So "the product is the derivation server" is the *mission*; the *V2
deliverable* is the substrate that makes it cheap to build.

---

## 6. Explicitly deferred / out of scope for V2

- **Full TensorFlow-class builds** — need `cc_common`-equivalent native runtime + external
  *megaproject* sources; that's beyond V2's bar (V2 targets self-contained real repos and
  broad rule-pack coverage).
- **External-dep *fetch* + lockfile** — V2 does `@repo`→local-path mapping behind a resolver
  interface; fetching is later.
- **In-graph `(Target×Config)` transitions / config-as-axis** — cross-platform via
  multi-instance instead (AD8); only a bounded exec/host transition if forced.
- **Parallel *analysis*** — deferred (execution-parallelism is the budgeted step).
  *Single-threaded* analysis-level early-cutoff is **NOT deferred** (55-3 H1): Phase 2 requires
  stable `ProviderSet`/`ActionPlan` hashing + DDS snapshot/read-set digests so a `.bzl` edit
  invalidates exactly the affected `Analyze` nodes. Only *parallel* analysis is out of scope.
- **The `.razel` (Grazel) authoring surface + inference passes** — a deferred *option* the
  clean contract preserves for free; not built in V2.
- **General non-monotonic composition** — bounded to declared-merge/decidable matchers.

---

*Architecture of record = this document **+ `RazelV2Contracts.md`** (the seam specs +
the DDS spine). The realization plan is `RazelV2FinalArchProposalPlan.md`.*
