# Razel Architecture — Distilled Model & Fundamental Requirements

Synthesis of `ArchAnal{Bazel,Pants,Razel}.md` through `ArchitectSkillRules.md`. This is
**step 1** (distill the finite shape under the feature-ocean) and the **requirements the
candidate architectures must satisfy**. The architectures differ in *how* they realize
these (the indirection spectrum), not in *what*.

Evidence tags: **[Bz]** Bazel, **[Pa]** Pants, **[Rz]** razel-as-built. Property tags:
**(a)** modularity/isolatability, **(b)** testability, **(c)** calibrated extensibility.

---

## 1. The distilled model — the finite shape

Two mature tools + razel collapse to the *same* small model. Thousands of
rules/attrs/providers/flags reduce to:

### Capabilities (the canonical verbs)
1. **Evaluate** a sandboxed config language deterministically. [Bz,Pa,Rz]
2. **Define & register** vocabulary — rule *types*, builtins, providers — from declarations.
3. **Resolve** the labelled dependency graph (`load()` + deps), **demand-driven**.
4. **Configure** — parameterize a target (config/`select`/transitions): the variation axis.
5. **Analyze** — run a rule impl over a `ctx` → emit **providers** (typed data) + **actions**.
6. **Resolve toolchains/platforms** for actions.
7. **Execute** actions via a **pluggable, content-addressed** backend (local/sandbox/remote).
8. **Incrementalize** — node-granular invalidate/recompute; **concurrent**.
9. **Extend orthogonally** — overlay computation (aspect / union dispatch) without editing rules.
10. **Query** the graph without executing (rdeps/affected).
11. **Resolve external deps** — declaratively (module graph + lockfile + registry).

### Concepts (the canonical nouns)
- **Label / Package / Target.**
- **Rule-type vs Rule-instance** — the *type* (name + attribute schema + impl fn) vs the BUILD
  declaration. (razel: **Word** = the type/builtin; **Noun** = the value it makes.)
- **Attribute** — typed; potentially configurable (`select`) and a dep-edge (with transition).
- **Provider** — `type-id` (constructor) + `instance` (fields). **The only legal inter-unit
  data channel; consumer names the provider, never the producer.** [Bz,Pa]
- **Configured/param-bound target** — `(Target × Config)` / `(Rule × Params)`.
- **Action / Spawn / Artifact** — unit of execution + the file in the graph.
- **Toolchain / Platform** — late-bound resolved deps.
- **Extension point** — `aspect` (Bz) / `@union`+`UnionMembership` (Pa) / ruleset-registry (Rz):
  open-set, self-registering, consumer enumerates members.
- **Module / Repo** — the external-dep universe (razel: **Phrasebook** = `@repo//` shim).
- **Graph node/key** — the substrate (`SkyKey/SkyFunction` Bz; `Node`/`rule_graph` Pa).
- **Analysis Session** — the per-run state (targets-so-far, results-by-label, pkg, config).
  **In Bazel/Pants this is a passed value; in razel today it is 8 thread-locals.** [Rz]

### Interactions (the canonical flow)
```
define:  rule(impl, attrs, provides)   → a registered Word (rule-type)
declare: my_rule(name, deps=…)          → a Target (rule-instance)
              │ resolve (load + deps, demand-driven, over ONE graph)
              ▼
        [configure: select/transitions] → a configured node
              │ analyze:  ctx = {deps' providers, attrs, toolchains}
              ▼   impl(ctx) ──▶ [providers]  +  actions registered
              │ execute (pluggable, content-addressed, concurrent)
              ▼
        artifacts ; (overlay: aspect/union adds providers/actions without editing rules)
```
**Invariant in all three:** providers are the producer↔consumer decoupling seam; world-effects
are quarantined to a small primitive kernel; everything above the kernel is pure → caching,
parallelism, reproducibility fall out *as consequences*, not features.

---

## 2. The two great bets — the axis the architectures vary on

The single most useful contrast across the analyses:

- **Bazel** spends its top-tier relief valve on an **embedded interpreter** (Starlark) over a
  *thin, hard-coded native core* — and its decade-long trajectory drove rules *out* of the
  engine entirely (`exported_rules = {}`; native `cc_library` now `fail()`s → `load()` from
  `rules_cc`). Maximal dynamism; weaker static guarantees.
- **Pants** spends it on a **typed, statically-solved rule graph** — logic composes by type,
  validated by a compiler at startup, world-effects in a tiny Rust intrinsic kernel. Maximal
  static safety + automatic wiring/caching; rigid (missing/ambiguous rule = startup failure).

**razel already owns the interpreter (Bazel's bet).** So razel's design leverage is *the layer
above Starlark* — the Session / provider store / extension registry. The candidate architectures
are therefore points on **"how much typed structure & static validation we layer above the
interpreter"** — from thin-and-dynamic (Bazel-like) to typed-graph-rich (Pants-like). That is
exactly the criterion-(c) calibration dial.

---

## 3. Fundamental requirements (invariants for *every* candidate)

### I. The spine — hard-code these (calibrated stops; the shape doesn't move)
- **R1. Decoupled sandboxed interpreter as the primary extension axis (c,b).** The language must
  not import build-tool types; the build API is *registered into* it. [Bz: `net.starlark.java`
  has zero build-lib imports; Rz: already has Starlark — preserve the decoupling.]
- **R5. One uniform, demand-driven graph; phases are node *types*, not control flow (a,b).**
  Buys incrementality, parallelism, and the ability to bolt on overlays. [Bz: Skyframe restart
  protocol; Pa: runtime `graph`; **Rz gap:** the live CLI path is a straight deps-first loop —
  `Engine` exists but is off the live path.]
- **R6. Quarantine world-effects in a small, content-addressed primitive kernel; everything
  above is pure (c).** [Pa: intrinsics; Bz: spawn strategies; Rz: actions/exec — keep as-is.]

### II. State — the keystone
- **R3. No ambient state. Analysis state is a scoped, explicit, *plural* value (Session) (a,b,c).**
  *The single highest-leverage requirement.* Ambient/global state is invisible-in-signatures,
  leaks across calls, and **structurally forbids two analyses at once** → blocks concurrency
  (P6) and two-phase (P3), and is what *causes* the god-module (no value to hang a module on →
  everything funnels to where the statics live). [Pa: the v1→v2 rewrite *was substantially* this;
  Bz: state threaded via `SkyKey`/`RuleContext`; **Rz violates:** 8 thread-locals.] Razel's
  `Session`-via-`eval.extra` (RefCell fields, forced by Starlark's `&mut Evaluator`-only builtin
  seam) is the honest realization; `Session.providers` *is* the AE provider store.
- **R14. One loader, one Target representation (a).** Unify/delete the dead parallel pipeline
  (`lib.rs` `load_build`/`TargetDecl`/`razel_analysis::analyze`/`Depset<T>`, reachable only from
  its own tests) **before** refactoring — else the refactor spawns a *third* depset/target
  definition. [Rz: the one gap the existing proposal missed.]

### III. The data contract
- **R4. Providers are the only inter-unit channel, and typed from day one (a).** Consumer names a
  provider, never a producer. Under-typing it early is among the most expensive things to undo
  [Bz: struct→declared-provider migration; Rz: today only an implicit DefaultInfo/hdrs/cflags
  bundle — make provider identity typed from the start].
- **R8. Compose by typed value/provider, not by name (a).** *Warning* [Pa]: avoid wrapper-type
  proliferation / overloading common types as wiring channels — give value-types clear identity.

### IV. Extension — "new feature = new file + one manifest line"
- **R2. Ship primitives, not rules; the rule/builtin is the unit of pluggability (a,c).** Do not
  bake concrete rules (`cc_library`) into the engine — Bazel spent years undoing exactly that.
- **R7. Open-set self-registration against a published extension point; consumers enumerate,
  never import producers (a).** [Pa: `@union`/`UnionMembership`, 108 backends, one-line
  activation; Bz: providers+toolchain+`RuleSet`; **Rz: the ruleset registry already proves this
  for languages — extend the same discipline to builtins/values.**] Use an explicit
  **const-array / data manifest**, *not* `inventory`-style auto-registration (that's the
  too-abstract dead end for ~12 builtins).
- **R10. Resist the rule() god-constructor (a).** Rule *capabilities* (test/build-setting/
  transition/aspect-host) are composable, separately-testable bits — not flags accreting on one
  signature/file. [Bz: `rule()` = 22 params, `RuleClass` 2.4k LOC; Rz: the `rules.rs` smell.]
- **R9. Configurability is part of the attribute model — decided early, not bolted on (c).** If
  razel will support `select`/transitions, an attribute *is* a select-able, transition-aware edge
  from inception. [Bz: bolting it on late produced a thicket of `*AttributeMapper`s; **Rz:**
  `select` is stubbed — decide the model now, even if to *defer explicitly*.]
- **R11. External-dep resolution is declarative from day one (c).** [Bz: imperative WORKSPACE was
  a dead end replaced by bzlmod over years; Rz: repo mapping/fetch unbuilt — design declaratively.]

### V. Testability
- **R12. Common layered harness; units unit-testable against a tiny session with NO toolchain (b).**
  *If a unit needs a real compiler (or a 2k-line harness) to test, the seam is wrong.* Keep a
  mock→real seam from day one; rule bodies assert on captured **`AnalyzedAction` (pure data)**,
  not on real builds; integration tests sit at the *top*. [Rz: ~28 `exists(){return}` skips
  silently collapse coverage on a compiler-less CI — the env-gated smell, confirmed.]
- **R13. Validate seams eagerly, but choose rigidity deliberately (c).** Surface unknown
  builtins/providers/loads at analysis, not deep in execution — but, *having Starlark's dynamism*,
  do **not** adopt Pants's full startup-failure rigidity (the too-rigid end). [Pa: 5-phase
  monomorphizing graph builder = expert-only subsystem.]

### VI. Process (meta)
- **R15. Spend the calibrated indirection (R1–R11) up front (b/meta).** Bazel's 1092
  `incompatible_*` commits are the fever chart of early hard-coding paid down one guarded flip at
  a time. Building new, razel's advantage is landing R3/R4/R9/R11 *from day one*.

---

## 4. Where razel stands against the requirements

| Req | razel today | evidence |
|---|---|---|
| R1 interpreter decoupled | ✅ | starlark-rust embedded; build API via builtins |
| R6 primitive kernel | ✅ | `razel-exec` actions, content-addressed, sandboxed |
| R7 open-set registration | 🟡 | ruleset registry works for rust/py/sh; cc/skylib/autoconfig still inline; builtins/values funnel into `rules.rs` |
| R3 no ambient state | ❌ | **8 thread-locals**; the keystone violation |
| R14 one loader/target | ❌ | dead second loader + 2nd `TargetDecl`/`Depset` |
| R4 typed providers | ❌ | only an implicit DefaultInfo/hdrs/cflags bundle |
| R5 uniform graph | 🟡 | `Engine` exists but live CLI path is a straight loop |
| R10 no god-constructor | ❌ | `rules.rs` 1658-LOC gravity well |
| R12 testable/no-toolchain | ❌ | pure helpers untested; ~28 toolchain-gated skips |
| R9 configurable attrs | ❌ | `select` stubbed (default branch only) |
| R11 declarative ext deps | ❌ | not built |
| R8 compose by typed value | 🟡 | deps flow by canonical-label lookup into `RESULTS` |
| R13 eager seam validation | 🟡 | unknown `@repo` errors; unknown builtins recognized-or-diagnosed |

The architecture is **clean below the loader and well-seamed at the ruleset registry**; it fails
R3/R14/R10/R12 *purely because state is ambient and the target model is duplicated*. Fix those and
most of the rest follows.

---

## 5. Settled vs. what the three architectures must decide

**Settled (invariant — every candidate must satisfy):** R1, R2, R3, R4, R6, R7, R10, R12, R14.
These are not up for debate; they're the lessons paid for in two codebases.

**The candidates differ on the criterion-(c) dial — how much typed structure / static machinery
to layer above the interpreter:**
- **R5** — how much of a real demand-driven graph (thin loop ↔ full Engine-on-the-live-path).
- **R8/R9** — how typed and how configurable the value/attribute model is (dynamic Starlark
  structs ↔ Pants-like typed value graph).
- **R13** — validation rigidity (lazy/dynamic ↔ eager/static).
- **The shape of the extension seam** — bare registry+manifest (thin) ↔ provider/union open-set
  dispatch ↔ typed rule-graph (rich).

That dial — **thin-and-dynamic (Bazel-like) → typed-graph-rich (Pants-like)** — is what the three
candidate architectures will stake out, each naming where it spends its relief valves and its
characteristic dead-end risk (too-rigid vs too-abstract). That is the next deliverable.

---

*Step 1 complete. Next: three candidate architectures as points on the §5 dial, scored on a/b/c
+ dead-end risk.*
