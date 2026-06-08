# `.bzl` → DDS — empirical validation across 5 rulesets

Stress-test of the DDS methodology (`RazelV2Contracts.md`) against **real** Bazel rulesets:
cc, rust, go, java, sh. Goal: not the happy path — find where the cc-baseline model
**strains**, and whether the strains are *localized refinements* or a *redesign*. Per-language
mappings (tables/properties/facts/matchers/productions + ranked strains, file:line-grounded)
are in `scratch/bzl-map-{cc,rust,go,java,sh}.md`. Grounding: **cc** = real Bazel Java
provider/`cc_common` source (rule `.bzl` moved to unmounted `@rules_cc`); **rust** = real
`@rules_rust` on disk (fully grounded); **go/java/sh** = canonical + buck2-prelude cross-ref
(rulesets not mounted) — flagged per claim in the maps.

---

## Verdict

**The DDS *spine + data model* is SOUND and vindicated — no kernel/spine redesign.** Five
real languages, including the richest (java) and the edge-heaviest (rust), map onto **atomic
provider facts + a typed key model + the engine/transactions** without a new kernel primitive.
**But the cc-baseline "uniform `Set`-fold over a pre-merged bag of deps" does *not* survive**,
and the empirical pass surfaced **three named, recurring refinements — all localized to L3
(the rule-pack `lower`/`FactView` contract) and §2 (`FieldType`), none in the DDS kernel.**

So the honest answer to *"is it a done deal?"*: **No** — but the gaps are three specific
amendments, not a rethink, and the spine is *confirmed* by the exercise.

---

## Per-language one-liners (the strain each exposed)

| lang | grounding | shape | the load-bearing strain |
|---|---|---|---|
| **cc** | real (Java API) | node-provider baseline | **ordered depsets** (link order significant; ≠ `Set` merge); **toolchain feature config = constraint solver**, not a matcher |
| **rust** | real (`@rules_rust`) | **edge-property** | **`--extern name=path` lives on the *edge*** (`DepInfo.direct_crates` = Set of edge records); proc-macro = typed edge **role** + **cross-instance** edge; `aliases`/`rustc_env` = `Map` |
| **go** | canonical+buck2 | config-axis | single-mode = AD8 ✅; **mixed-mode/per-edge transitions = deferred `(Target×Config)`**; `cgo→CcInfo` cross-lang seam (reduced `CcInfo`); `importmap` = `Map` |
| **java** | canonical+buck2 | **multi-closure** | **3+ transitive closures, 3 propagation rules** (deps/exports/runtime_deps); **`exports` re-propagation** = edge-kind splice; atomic-provider **vindicated** |
| **sh** | partial+buck2 | trivial floor | **runfiles deferred → expressible only as declarations+data, not *runnable***; facet API keeps authoring cheap but exercises ~2/8 capabilities |

---

## What HOLDS (the spine, vindicated)

- **Atomic provider fact per `(target, ProviderTypeId)`** — **java vindicates the 55-4 B2
  decision**: `JavaInfo` is the most multi-faceted provider of the five and is still *one*
  atomic `Scalar` fact; storing it field-by-field would have been a disaster. cc/rust/go fit.
- **Keys/identity, `AnalysisInstanceId`, the engine, transactions, the read/write split** —
  no strain; sound as written.
- **Multi-instance (AD8)** covers single-mode go + the `GOOS/GOARCH` cross-product (linear
  N-instance overhead, the intended trade).
- **The facet API encapsulates the substrate** — sh authors a pure `lower` in ~150 LOC and
  never touches read-set/merge/confluence machinery.

## The three refinements (recurring, localized — fold these in)

### R-α — `FieldType` is too narrow: add `Map`, and make ordered `Depset` first-class (≠ `Set`)
- **`Map`** recurs in **rust** (`aliases`, `rustc_env`), **go** (`importmap`), **java**
  (`RunEnvironmentInfo`) — the closed §2 universe excludes it; multiple languages need it.
- **Ordered `Depset`** is **cc's #1 strain**: `CcInfo`'s `headers`/include-quad/`defines`/
  `linker_inputs` are order-significant depsets; **link order is semantic**, so the merge is
  **ordered-union, not the §3 commutative `Set`**. (Already flagged as a must-fix at
  `rules.rs:714` `let _ = order` — cc proves it's *baseline-critical*, not edge-case.)
  `List`/`Set`/`Depset` must stay **three distinct** field types (cc uses all three).
- **Amendment:** §2 adds `Map<K,V>` and a first-class ordered `Depset` (with ordered-union
  semantics distinct from `Set` merge); the **2.0 `taut` sum-type extension must also encode
  `Map` + ordered depset** deterministically. *(This is also exactly the `cgo→CcInfo`
  FieldType-fidelity gap go flagged.)*

### R-β — the `lower`/`FactView` contract must expose deps as **typed edges** (kind/role/alias, cross-instance) — NOT a pre-merged bag  *(the big one)*
- **rust:** `--extern {name}={path}` — the bind name lives on the **(consumer, dep) edge**;
  `DepInfo.direct_crates` is a `Set` of **edge records**, not `Depset<Label>`. Proc-macro is
  the **same provider type consumed by edge *role*** (exec-platform dylib vs target rlib),
  and the macro edge **crosses into a second `AnalysisInstanceId`** (its path points into
  another DDS instance).
- **java:** propagation is **edge-kind-discriminated** — `deps` vs `exports` vs
  `runtime_deps` vs `plugins` feed *different* closures by *different* rules; `exports`
  **re-propagates** a child's provider transitively through the exporter.
- **cc:** the milder baseline version — compilation-vs-linking context + transitive/direct/
  local split (nested `FieldType::Provider` + per-field fold).
- **The fatal-if-missed point:** if `lower` only receives a **pre-merged "bag of deps'
  providers,"** rust and java are **unimplementable correctly**. `lower(target, &FactView)`
  must let the rule pack walk deps **as typed edges** (with edge kind/role/alias) and request
  a dep's provider **in a specific instance**. This is the single biggest refinement — and
  it is **localized to the L3 rule-pack API contract (§8), not the DDS kernel.**
- **Amendment:** §8 — `FactView`/`TargetFacts` expose **edge-typed dependency access**
  (per-edge records: kind, role, alias, target `AnalysisInstanceId`); `fold_deps` becomes one
  helper *among* per-edge projections, not the only access path.

### R-γ — toolchain *feature configuration* is a bounded constraint-solver / `Derived` step, not an Eq-decidable matcher  *(cc)*
- cc's `configure_features` runs a **fixpoint solver over `requires`/`implies`/`provides`**
  (80+ feature constants), and `CcToolchainInfo` exposes **feature-config-*dependent*
  methods** (`needs_pic_for_dynamic_libraries(feature_configuration)`) with no slot in the
  closed `FieldType` universe. The §9 "Eq-decidable matcher" framing covers `select`/simple
  toolchain selection but **not** cc's feature engine.
- **Amendment:** model feature configuration as a **bounded, deterministic `Derived`
  computation** (a small constraint solver producing feature-config facts), distinct from the
  Eq-decidable `select`/toolchain *matcher*. (Tightens 55-4 B4's "bounded toolchain inputs":
  the *binding* is a computation, not a lookup.)

## Deferred gaps + honest scope corrections (no fold needed, but state them)

- **go mixed-mode-in-one-graph / per-edge transitions** = the *deferred* `(Target×Config)`/
  `Transitions` capability. Single-mode is fine (AD8); a *faithful* `rules_go` port (its idiom
  is transition-first) hits the reserved capability early. Narrow but real — keep it reserved,
  flag it.
- **sh runfiles** — `sh_binary`/`sh_test` are expressible **only as declarations + data-as-
  inputs, not as runnable binaries** (runfiles is post-V2, §2/§8/Plan-3.4). **Phase-3 narrative
  correction:** "sh lands with zero kernel edits" must *not* be read as "razel can run sh";
  the 3.5 sh test asserts on captured contract facts and flags runfiles as post-V2/untested.
- **rust build scripts do NOT need intra-`lower` ordering** (corrects an earlier prediction):
  `BuildInfo` is a normal producer→consumer **action-graph edge** over `File::Generated` + a
  `Scalar` "one BuildInfo" field — already covered.
- **`ActionKey` completeness** — cc's ~40-param `compile()` is exactly why §10 mandates the
  path-sensitive/env/param-file fixtures; vindicated.
- **go `importpath` uniqueness** — a cross-target global invariant → a `Validation` facet over
  a range-query read-set (minor net-new fact shape).

---

## Bottom line

The exercise did its job: **the DDS spine is confirmed by five real languages (atomic
providers especially), and the gaps are three named, localized refinements** — `Map` +
ordered-`Depset` in `FieldType` (§2/2.0), edge-typed dependency access in the `lower`/
`FactView` contract (§8), and toolchain feature-config as a bounded `Derived` solver (§9) —
**plus the cleanly-deferred go-transitions and sh-runfiles items.** None touch `razel-dds` or
the keys; all sit in L3/§2. That is the difference between "needs a rethink" and "needs three
contract amendments before the cc gate generalizes." The cc dogfood gate should be extended to
exercise **R-α (ordered depset fixture, already mandated) and R-β (a 2-dep target where the
two deps are consumed by different edge-kind/role)** so the contract is falsified on the
edge-typed-access requirement *before* the rust/java packs are attempted.
