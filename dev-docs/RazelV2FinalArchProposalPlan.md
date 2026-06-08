# Razel V2 — Realization Plan

Actionable plan for `RazelV2FinalArchProposal.md` + `RazelV2Contracts.md`. **Governing
principle:** *build the support structure (the kernel + the yidl-lite rule-pack API)
**once**, prove it on a single reference rule pack, and only **then** fan out — so adding
Bazel/`.bzl` support becomes parallel throughput, not a serialized bottleneck.* We do
**not** add rule support one-at-a-time before the infra exists.

**Granularity (this revision):** every step is scoped to **≲500 LOC of complexity
exposure** (a soft ceiling — a unit small enough to review, test, and land green on its
own). Steps carry inline **[R#]** risk tags resolved in the consolidated *Risks* section.
LOC figures are rough budgets, not contracts.

Confidence: **high** on Phases 0–1 (no-regret; verified). **Phase 2 is heavier than a prior
draft claimed (Review-48): four of its biggest items are *from-scratch subsystems*, not
"reuse"/"refactor"** — the typed engine value-model (`razel-engine` is a `String`→`Digest`
toy today), the loading graph (`razel-vfs` is unwired), the provider type system +
return-capture (today the rule impl's return is discarded, no `provider()`, `attrs` ignored),
and `select`/toolchain matchers (today `select` picks the first branch). So Phase 2 is
**medium confidence**, budget-heavy, and several steps split (below). **Gated** thereafter on
the Phase-2/3 proof. The make-or-break is the **provider system + effect-capturing adapter on
typed `Analyze` engine nodes** — de-risked by dogfooding `cc` through the public API *and the
engine* before parallelizing.

---

## Phase 0 — Branch + lock the record + forcing gates
- **0.1** Create the **`razelv2`** branch; adopt `RazelV2FinalArchProposal.md` +
  `RazelV2Contracts.md` as the architecture of record. *(≈0 LOC)*
- **0.2** Forcing CI gates *(≈120 LOC)*: clippy `disallowed_*` + CI grep denying
  `thread_local!`/`static mut` in loading/kernel crates; **negative tests** (`REQ-TEST-004`)
  — the *ambient-state* negatives (`thread_local!`, mutable global) land **now**; the
  *unregistered-provider* and *rule-pack-kernel-edit* negatives presume machinery that
  doesn't exist yet, so they land with **2.3** and **2.10** respectively (Review-48). **[R6]**
- *Exit:* gates fail the build (and the negative tests prove they fail) when violated.

## Phase 1 — Demolition + foundation (sequential; no-regret; green at each step)
Strangler work on existing code. **Each step starts with a characterization/regression test
pinning the retained live-path behavior before the change (`REQ-TEST-001`). [R7]**
- **1.1** Characterization tests: `analyze_starlark`/`analyze_bazel` → captured
  `AnalyzedAction.argv`/providers for cpp-tutorial 1–3 + a macro case. *(≈180 LOC tests)*
- **1.2** Delete the dead second loader — `lib.rs` `load_build`/`TargetDecl`/`query_targets`
  + `razel_analysis::analyze` + `Depset<T>` + their tests (verified: no non-test callers).
  *(≈ −400 LOC)* **[R8]**
- **1.3** Extract the **Lexicon** (`canon_label`, `glob_match`, `fold_depset`, `shquote`,
  path/quoting) + direct unit tests; `canon_label` etc. now take pkg/state as **params**
  (no thread-local read) — precursor to the Session. *(≈250 LOC + 150 tests)*
- **1.4** Introduce the **`Analysis` Session** via `eval.extra`; migrate `RESULTS`→
  `results` first (it is also the provider store). *(≈220 LOC)* **[R1]** the nested-eval
  borrow (interior-mutability fields).
- **1.5** Migrate the remaining 7 statics (`STATE`/`CONFIGS`/`WORKSPACE`/`CURRENT_PKG`/
  `LOADED`/`GLOBAL`) + `CTX` to Session fields; delete the divergent hand-resets; unify the
  drifted globals-builders; **turn on the ambient-state CI deny**. *(≈300 LOC)*
- **1.6** Build the **shared analysis-only test harness** (Review-48 HR-3): "feed a Word
  args + a tiny `Analysis`, assert on captured `AnalyzedAction.argv`/providers" — **no
  Evaluator, no toolchain**. Every later pack consumes it; this is the structural fix for the
  24 `exists(){return}` skips, not a policy. *(≈250 LOC)* **[R3b]**
- *Exit:* no thread-locals remain; `rules.rs` shrinking; the harness exists; all tests green.

## Phase 2 — Kernel + yidl-lite rule-pack API (THE enabler; build once, well)
Dissolve the rest of `rules.rs`; build the infra Phase 4 parallelizes against; the §5b seams
are Phase-2 contracts proven *together* by the cc gate. **Review-48 correction: four of these
are *greenfield*, not "reuse"/"refactor" — the typed engine value-model (engine is a
`String`→`Digest` toy), the loading graph (`razel-vfs` is unwired), the provider system +
return-capture (the rule impl's return is discarded, no `provider()`, `attrs` ignored), and
`select`/toolchain matchers (`select` picks the first branch). Steps are re-framed, re-sized,
split, and re-ordered (the BL-4 cycle) accordingly.**

**TDD + visible sub-gates (55-2 H1 — the repo's red-first rule).** Each step below lands its
own **failing contract test before implementation** (the §11 row→step map in
`RazelV2Contracts.md` names where each first test lands). 2.16 is the **cross-seam
integration** proof only — *not* the first test of keys/schemas/loading/depset/return-capture/
typed-nodes. Phase 2 has **four visible sub-gates** so failure is caught early, not at 2.16:
**2A** identity + repo-map + provider schema + fact key + determinism fixtures (2.1–2.5);
**2B** loading graph + typed engine value-model (2.6a–2.8); **2C** rule-pack facets + provider
system + adapter return-capture (2.9–2.13); **2D** cc dogfood through the engine + provenance
(2.14–2.16). **Three compatibility tests must pass before Phase 3:** provider `D1` consumer
accepts compatible `D2` *by stable type id*; two repo-mappings resolve the same apparent
`@foo//:defs.bzl` to different modules without provider-identity collision; a toolchain change
shifts `AnalysisInstanceId` without changing the target label.

*Contracts & identity*
- **2.1** Key types + the **DDS store** — `Label`/`RepoId`, `BzlModuleKey`(≠Label, B4),
  **two-level provider identity** (`ProviderTypeId` + `ProviderSchemaId`, 55-2 B1),
  **namespaced `FactKey`** (B2), **complete `ActionKey`** ((path,digest) inputs + tool/exec/
  workdir/output-shape/param-files, B5) + encodings/equality/taut fixtures **incl. the
  path-sensitive + env + param-file action fixtures** (Contracts §0, §1, §10). *(≈380 LOC)*
- **2.2** `RequestedInstanceKey` + **`ToolchainResolution` node** → `AnalysisInstanceId`
  (the **two-stage bootstrap**, 55-2 B3 — no cycle); instance-scoping of `TargetKey`; tests:
  same-label-two-configs, and **toolchain-change → different `AnalysisInstanceId`, no
  collision** (`REQ-CONFIG`). *(≈250 LOC)* **[R5]**
- **2.3** Provider **schema** — type/schema split, fields+tags, **type-first lookup then
  schema-compat** (B1), closed **`FieldType` universe** (adapter rejects struct/dict/runfiles
  with provenance, H4), unknown-field policy, taut defs (Contracts §2); the
  *unregistered-provider* CI negative + the **D1↔D2 compat test** land here. *(≈340 LOC)* **[R3]**
- **2.4** Fact model + merge + provenance + **definition-time confluence** + conflict tests —
  **`Scalar` + `Set` only; `OrderedList`/`OverrideableScalar`/`Derived` declared but
  *reserved/unimplemented*** (CAL-1: zero consumers yet; saves ~300–400 LOC for the
  greenfield seams). *(≈250 LOC)* **[R3]**
- **2.5** Declarative repo identity + repo-mapping + canonical labels + local-path resolver
  shaped like a future lockfile resolver (`REQ-REPO`). *(≈300 LOC)*

*Loading graph + engine value-model — GREENFIELD (BL-3, BL-2)*
- **2.6a** New loading nodes `SourceSnapshot`/`DirListing`(glob) over the `razel-vfs`
  `ContentProvider` (vfs supplies the abstraction *only*; the nodes are new). *(≈300 LOC)* **[R2]**
- **2.6b** `BzlLoad` keyed by **`BzlModuleKey`** with **contextual apparent-repo resolution
  through the importer's repo-mapping** (55-2 B4) + transitive-load tracking + `RepoMap` + the
  **4 edit-invalidation tests** (asserting *recompute/cutoff behavior*, not internal dirty
  marks) + the **two-repo-mapping → same apparent `@foo//:defs.bzl` → different module** test
  (`REQ-LOAD-003`). *(≈380 LOC)* **[R2]**
- **2.7** **Rebuild `razel-engine`'s value/key model** to carry **typed node values**
  (`ProviderSet`/`Vec<ActionKey>`/`Outputs`), not just `Digest` (BL-2 — today `String`→
  `Digest`, no value store; keep its early-cutoff algorithm). Wire the **loading + `ActionExec`**
  nodes. *(≈450 LOC)* **[R9]**
- **2.8** Route **CLI + daemon** through the engine for the loading+exec ends; delete the
  straight `execute` loop as the product path; parity vs 1.1. *(≈250 LOC)* **[R9]** *(The
  `Analyze`/`ActionPlan` nodes wait for 2.13 — they need the rule pack; BL-4.)*

*The yidl-lite layer — GREENFIELD (BL-1, HR-1)*
- **2.9** Rule-pack API as **capability facets** + pure `lower(facts,&Analysis)->Declared` +
  reserved extension points (`REQ-RULEPACK`). *(≈350 LOC)* **[R10]**
- **2.10** Kernel primitives: `fold_deps`, action templates, and **`depset` with real
  traversal order + deterministic dedup + byte-stable encoding** (HR-2 — order is dropped
  today, `rules.rs:714`). *(≈400 LOC)*
- **2.11** Matchers — **`select`/`config_setting` + `match_toolchain`, Eq-decidable, with
  definition-time confluence** (greenfield: `select` picks the first branch today, HR-1);
  scoped to what cc/toolchain need. *(≈350 LOC)* **[R3]**
- **2.12** **Provider system (greenfield, BL-1):** the `provider()` builtin + a typed
  `Provider` value + **capture the rule impl's currently-discarded return** (`rules.rs:564`) +
  the `attr`-schema/`TargetFacts` layer (`:587`). The prerequisite of the adapter. *(≈450 LOC)* **[R4]**
- **2.13** Bazel adapter = **effect-capturing `ctx`** that executes evaluated `rule()` impls and
  records `ctx.actions` + **the returned typed provider** as facts; macro + native-shim paths
  (`REQ-ADAPTER`). *(≈400 LOC)* **[R4]** the make-or-break evaluated-`.bzl` seam (Q6).

*Analyze nodes + dogfood + gate*
- **2.14** Wire the **`Analyze(TargetKey)→ProviderSet` + `ActionPlan` engine nodes** — now the
  rule-pack/adapter that computes them exists (**resolves the BL-4 cycle**); complete
  CLI+daemon **analysis-on-graph** + early-cutoff (a `.bzl` edit invalidates `Analyze` via its
  `BzlLoad` dep). *(≈300 LOC)* **[R9]**
- **2.15** Registry/manifest + assembler; `cc` reference pack through the **public API +
  engine only**. `rules.rs` is gone (→ thin `assembler` + `analysis/orchestrate` + adapter +
  cc pack). *(≈450 LOC)* **[R1]**
- **2.16** Strengthened gate test-suite. *(≈450 LOC tests)*
- ⛔ **Exit gate (all hold; else fix the contract/API *now*, before fan-out):** keys+fixtures
  incl. a **depset-order fixture** (HR-2) · loading via VFS with the **4 invalidation
  scenarios** · **CLI & daemon on the engine with typed `Analyze`/`BzlLoad` nodes present — a
  build whose graph has zero `Analyze` nodes FAILS the gate** (BL-2) · a **nontrivial
  evaluated Starlark `rule()` impl that `return`s a typed provider**, with the gate asserting
  *that returned provider landed as a fact* (not the `DefaultInfo` side-effect path) (BL-1) ·
  **same label under two `AnalysisInstanceKey`s, no collision** · `cc` builds **cpp-tutorial
  1–3** via API+engine · negatives pass (schema mismatch, confluence conflict, config
  collision, ambient state, kernel-edit-from-pack) · minimal **explain/provenance** for cc.

## Phase 3 — First `.bzl` surfaces from declarations + validate the flow
- **3.1** Declare `@rules_cc`/`@rules_python`/`@rules_rust`/`@rules_shell` load surfaces as
  manifests against the API. *(≈200 LOC)*
- **3.2** `rust` rule pack as declarations — **the "second pack proves the model" gate: zero
  kernel changes**. *(≈300 LOC)* **[R10]**
- **3.3** `py` rule pack (PYTHONPATH/runfiles/launcher). *(≈300 LOC)*
- **3.4** `sh` rule pack (script-as-exe + data/runfiles). *(≈150 LOC)*
- **3.5** Stock-`.bzl` tests, **each classified by what it proves** (Review-55): macro-expand
  · evaluated `rule()` · provider declare+consume · action declare · depset order ·
  `select`/toolchain · repo mapping. *(≈300 LOC tests)*
- *Exit:* rust/py/sh land as declarations with **no kernel edits** → "new ruleset =
  declaration file + one row" is proven; fan-out is unblocked. **[R11]**

## Phase 4 — Parallelize `.bzl` support (the payoff; N agents; the anti-bottleneck)
Kernel frozen and proven. **Fan out** — one worktree-isolated agent per rule-pack /
stock-`.bzl` cluster, each ≲500 LOC: a declaration file set + one manifest row + a
stock-`.bzl` test, disjoint files (the proven rust/py/sh pattern, at scale).
- **4.x** work items (parallel): `@bazel_skylib` rules + `lib` (`selects`/`paths`/`sets`) ·
  config-repo shims (`@local_config_*`, cuda/rocm `if_*_is_configured`) · `genrule` +
  make-vars/`$(location)` · cc depth (`cc_test`, transitive include dirs) · rust/py/sh depth
  (transitive, runfiles).
- Cadence: each pack passes its stock-`.bzl` test + the forcing gates; integrate by
  collecting disjoint files. **No kernel edits permitted [R12]** — if an agent needs one,
  *pause the fan-out, add the capability to the public API with a regression test, resume.*
- **Don't present fan-out as imminent (Review-48):** because providers/attrs/toolchains are
  all *new* in Phase 2, the odds that the *first* Phase-3 packs surface a kernel gap are high.
  That's acceptable (R12 names the protocol), but expect Phase-3 churn before the API is
  genuinely frozen — fan-out begins only once rust/py/sh truly land with zero kernel edits.
- *Exit:* progressively larger self-contained repos build; coverage grows without
  serialization.

## Phase 5 — The mission surfaces (F17 + F24 + MCP) — *the product*
- **5.1** F17 derivation layer over the fact substrate (IDE/LSP index, affected, lint) —
  pure matchers/derivations, bounded composition. *(≈400 LOC + per-derivation packs)*
- **5.2** MCP surface: `schema`/`graph`/`query`/`explain`/`provenance` (transactional edit
  later). *(≈350 LOC)* (explain/provenance already exists from 2.16.)
- **5.3** F24 distribution over iroh: multi-instance graphs + fact/provider **merge across
  the mesh** (taut/CBOR); cross-platform = per-platform instances + cross-instance
  derivation. *(≈450 LOC + protocol)* **[R13]** merge soundness across instances.
- **5.4** Parallel **execution** — the budgeted `!Sync`→`Send+Sync` engine rewrite — when a
  workload demands it. **[R14]** a real rewrite, not commodity.
- *Exit:* an MCP agent + the UI consume live derived views from a multi-node graph.

## Phase 6 — External deps + scale
- **6.1** `@repo`→local-checkout path-mapping via the resolver interface from 2.5. *(≈250 LOC)*
- **6.2** Broaden config-repo shims toward bigger real repos. (Full fetch/lockfile and
  `cc_common`-class TF remain explicitly out of V2's bar.)

---

## Mapping to the rough skeleton you gave
1. razelv2 branch → **0.1**. 2. clean up / remove `rules.rs` → **Phase 1** + **2.15**.
3. build fundamental scaffolds (yidl-like) → **2.1–2.14**. 4. first `.bzl` surfaces from
declarations → **2.15 + 3.1**. 5. test against stock `.bzl` → **3.5**. 6. add more support →
**Phase 4**. 7. parallelize → **Phase 4** (gated on the 2.16/3 proof). Plus forcing gates
from line 1 (0.2) and the mission/scale phases (5–6).

---

## Risks & gates (consolidated; surfaced into the steps above)
- **R1 — Session / nested-eval borrow.** `eval.extra` can't carry a `&mut` across nested
  rule eval → interior-mutable `Analysis` fields (the honest minimum, not a relapse). *Steps
  1.4, 2.15.*
- **R2 — loading not quarantined (greenfield, BL-3).** `razel-vfs` is unwired today; the
  loading nodes are new. If reads stay ambient, incrementality/distribution break. *Steps
  2.6a/2.6b + invalidation tests.*
- **R3 — composition over-build (F25/S8/CAL-1).** Phase 2 ships **`Scalar`+`Set`+confluence
  only**; reserve the other 3 merge-classes (no consumers yet); no general logic engine.
  *Steps 2.3, 2.4, 2.11.*
- **R3b — toolchain-gated coverage floor (HR-3).** A *policy* erodes; build the shared
  analysis-only harness so it doesn't. *Step 1.6.*
- **R4 — the provider system + effect-capturing adapter (Q6/B1, greenfield).** The rule-impl
  return is discarded + no `provider()` today; the new provider-capture + the evaluated-`.bzl`
  seam are the make-or-break. *Steps 2.12 (provider system) + 2.13 + gate.*
- **R5 — multi-config creep (S6/B4).** Cross-platform = multi-instance (AD8); add a bounded
  exec/host transition only if a concrete case forces it. *Step 2.2.*
- **R6 — forcing gate gaps.** Negative tests prove the bans actually fail CI. *Step 0.2.*
- **R7 — refactor regressions.** Characterization tests before each Phase-1 change. *Phase 1.*
- **R8 — third source of truth.** Delete the dead loader *before* new contract work, or a
  third target/depset rep appears (S7). *Step 1.2.*
- **R9 — engine is a Digest toy (B5/BL-2).** Rebuild the typed value/key model in Phase 2;
  route CLI+daemon through it; the gate demands typed `Analyze`/`BzlLoad` nodes (or it passes
  on action-digests alone). *Steps 2.7 (rebuild), 2.8, 2.14 (Analyze nodes, post-adapter).*
- **R10 — rule-pack god-constructor (S5/H1).** Capability facets + reserved extension points,
  not flags on one object. *Steps 2.9, 3.2.*
- **R11 — API proven too narrowly.** The Phase-2 gate must include an evaluated `rule()` and
  two configs, and Phase 3 must land rust/py/sh with zero kernel edits, before fan-out.
- **R12 — Phase-4 agent needs a kernel change.** Signal the API is incomplete: pause, fix the
  kernel once *with a regression test*, resume — never fork the kernel. *Phase 4.*
- **R13 — cross-instance merge soundness.** Provider/fact merge over the mesh must respect
  schema identity/versioning + merge-classes; negative tests. *Step 5.3.*
- **R14 — parallel execution rewrite.** The Engine is `!Sync` today; budget a real
  `Send+Sync` step, not a commodity add-on. *Step 5.4.*

## Review-55 + Review-48 amendments (folded in) — decisions + REQ→phase map
**Review-55 DL:** DL-001 canonical-contract + rule-pack direction; DL-002 adapter =
effect-capturing boundary; DL-003 loading/VFS = quarantined graph effect; DL-004 explicit
`AnalysisInstanceKey`; DL-005 engine-on-live-path + repo identity moved earlier.

**Review-48 DL (code-grounded):** BL-1 adapter/provider system is **greenfield, not a
redirect** (return discarded, no `provider()`, attrs ignored) → new Step 2.12; BL-2
`razel-engine` is a `String`→`Digest` toy → **rebuild the typed value model** (2.7) + gate
demands typed `Analyze` nodes; BL-3 `razel-vfs` unwired → loading is **new** (2.6a/b); BL-4
**ordering cycle resolved** — `Analyze` nodes (2.14) land *after* the rule-pack/adapter;
HR-1 four from-zero subsystems → confidence re-stated; HR-2 ordered `depset` (2.10) + gate
fixture; HR-3 shared test harness (1.6); HR-4 `RepoId` encoding (2.1); **CAL-1** ship only
`Scalar`+`Set`+confluence, reserve the rest (2.4) to fund the greenfield seams.

| Requirement | Step(s) |
|---|---|
| `REQ-ADAPTER` | 2.12 (provider system) + 2.13 + gate |
| `REQ-LOAD` | 2.6a/2.6b + gate |
| `REQ-CONTRACT` | 2.1 (incl. `RepoId`), 2.3 |
| `REQ-CONFIG` | 2.2, 2.16 + gate |
| `REQ-ENGINE` | 2.7 (rebuild), 2.8, 2.14 + gate |
| `REQ-RULEPACK` | 2.9 |
| `REQ-REPO` | 2.5 |
| `REQ-COMP` | 2.4 (Scalar+Set only), 2.16 + gate |
| `REQ-TEST` | 0.2, 1.1, 1.6 (harness), 3.5, 2.16 |

**Rule-pack test bar (`REQ-TEST-002/3`, every pack):** assert on **captured contract data**
— providers, actions, argv/env, inputs/outputs, provenance — *without a real compiler*;
toolchain-gated integration tests must not be the *only* coverage (closes S11).

## The essential point
Phases 2–3 are the leverage: **the kernel + the yidl-lite rule-pack API + the proof**. Pay
for them properly — with the §5b contracts written first and the strengthened gate proving
them at once — and the road to broad Bazel coverage (and the grip-lab derivation server) is
*parallel throughput*, not a serial grind. Build the infra that enables `.bzl` support at
speed, then add the speed.
