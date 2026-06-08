# Architecture A3 — M2 Inversion: Facts / Matchers / Compositional Rules

*Candidate architecture 3 of 3. Read against `ArchFundamentals.md` (F1–F23),
`ArchBazelConstraints.md` (C1–C9), `ArchPatterns.md` (Part C dial), `ArchModel.md`
(R1–R15), and `YIDLDigest.md` (the inspiration source — **inspiration, not adoption**).*

> **The one-sentence bet.** Re-slice razel's analysis layer so that the build is
> **derived** by matching and composing **typed facts**, not **recorded** as a
> side-effect of imperative `rule()` impls. `rule()`/`ctx` stay at the Bazel
> *surface* (C-constraints forbid changing them), but they **lower into facts**
> instead of pushing into thread-locals; everything below the surface is a
> fact store + matcher dispatch + compositional rules feeding razel's existing
> demand-driven `Engine` and content-addressed kernel.
>
> **The honesty up front (the load-bearing caveat).** YIDL's *resolution engine* —
> ordered, stratified, once-through, **fixpoint-free** — is the *opposite* of a
> build tool's reason to exist (incremental, demand-driven, early-cutoff,
> explainable: F5/F6/F10/F21). A3 therefore steals **only the authoring/decoupling
> slicing** (facts-as-providers, matcher selection, phase-splice extension, closed
> plan over a blind executor, definition-time disambiguation) and **rejects the
> evaluation model wholesale**. Where this draft is weakest — transitive provider
> propagation and demand-driven derivation — is exactly where YIDL is weakest, and
> I say so in §6 and §8 rather than hiding it.

---

## 0. Where the M2 layer lives (the critical placement question)

razel **must** consume unmodified `BUILD`/`.bzl` (C1–C5). Bazel's `rule()` model is
irreducibly imperative: `rule(impl, attrs)` returns a callable, and `impl(ctx)`
runs arbitrary Starlark that calls `ctx.actions.run(...)`. **A3 does not and cannot
replace that.** The M2 layer lives strictly **below the Starlark surface**, at the
analysis/IR seam. Concretely, three planes:

```
  ┌─ PLANE 1 — Bazel surface (FIXED by C1–C9) ─────────────────────────────┐
  │  Starlark: rule(impl, attrs), ctx.actions.run/write, provider(),        │
  │  depset(), select(), native cc/rust/py/sh. Evaluated faithfully.        │
  │  *** This plane is imperative and razel does not get to change it. ***   │
  └────────────────────────────┬───────────────────────────────────────────┘
                               │  LOWERING (the seam): builtins ASSERT facts
                               │  instead of mutating thread-locals.
  ┌────────────────────────────▼───────────────────────────────────────────┐
  │  PLANE 2 — Fact store (the M2 substrate, NEW)                            │
  │  Typed facts in identity-bearing collections, held in the per-analysis  │
  │  Session (NOT thread-locals). Targets, Attrs, DepEdges, Providers,       │
  │  ActionReqs, Configs, Toolchains. This IS the analysis state (F19).     │
  └────────────────────────────┬───────────────────────────────────────────┘
                               │  DERIVATION: matchers + compositional rules
                               │  derive new facts (providers, action reqs).
  ┌────────────────────────────▼───────────────────────────────────────────┐
  │  PLANE 3 — razel's existing engine (KEPT)                               │
  │  wire_to_ir → razel-ir::Graph (rdeps/impact) → razel-engine::Engine      │
  │  (demand-driven, early-cutoff) → razel-exec (content-addressed kernel).  │
  │  *** YIDL's once-through resolution is REJECTED here; this stays. ***    │
  └──────────────────────────────────────────────────────────────────────────┘
```

**Two front-ends, one fact store.** Because Plane 2 is surface-agnostic, the *same*
fact store can be populated two ways:
1. **Lowered from Bazel** (the mandatory path): a `cc_library(...)` call, whether
   native or evaluated `rules_cc`, asserts `Target`/`Attr`/`DepEdge` facts.
2. **Authored natively** (the optional, clean-slate path of
   `ThoughtExp-CleanSlateBuildSurface.md`): razel's *own* rules (cc, rust, py, sh)
   are written **as fact-deriving rule-sets** rather than as 1,658 lines of
   imperative `cc_rules`. This is where A3 pays off: the native cc rule-set is the
   place A3 deletes the `rules.rs` god-module, not the Bazel surface.

This placement is the whole point: **A3 is an analysis-model re-slice, not a surface
language.** It does not need M3 (a self-defining language — `YIDLDigest §2` flags M3
as deliberately unbuilt, and razel needs it even less than YIDL did).

---

## 1. The metaphor — and how it maps onto the existing Dialect (A1) vocabulary

`RazelDialect.md` already names Words / Nouns / Phrasebooks / Session / Lexicon.
A3 **keeps that vocabulary at Plane 1** and adds a Plane-2 vocabulary underneath it:

| A3 piece | what it is | razel crate terms | YIDL analogue |
|---|---|---|---|
| **Fact** | an immutable typed record in an identity-bearing collection | `enum Fact { Target, Attr, DepEdge, Provider, ActionReq, Config, Toolchain }` in a new `razel-facts` crate | YIDL `record`/`family` in a `collection` |
| **Collection** | the stored, queryable fact-set (identity + write policy) | `FactStore` field on the `Session` (`RefCell<…>` interior mutability) | YIDL `collection … identity …` |
| **Matcher** | declarative, decidable selection of a resource from a fact tuple | `trait Matcher { fn select(&self, facts: &FactView) -> Option<Resource> }` + a table of guarded rules | YIDL `matcher … rule when … == …` |
| **Compositional rule** | derives new facts / action-requests from existing facts | `trait DerivationRule { fn derive(&self, view: &FactView, out: &mut FactDelta) }` | YIDL `production` / `composable production` |
| **Resource** | the *named output contract* a matcher selects — never the producer | `enum Resource { Toolchain(ToolchainId), ProviderType(ProviderTypeId), ActionTemplate(…) }` | YIDL `resource` / `contribution` |
| **Rule-set** | one file's bundle of facts + matchers + derivation rules for a language | `trait RuleSet { fn install(&self, reg: &mut Registry) }` (cc, rust, py, sh) | YIDL `.yidl` concept file (e.g. `lifecycle_local_store.yidl`) |
| **Session** | the per-analysis fact store + derivation log | the `Analysis` struct from `RazelDialect.md §3`, now *holding* the `FactStore` | YIDL closed `AssemblyPlan` (callback-free after expansion) |

So A3 is **not** a competitor to A1 (the Dialect). It is A1's **Session generalized
into a fact store**, with the imperative `rule()` recording replaced by *fact
assertion + ordered derivation*. `Analysis.providers` in `RazelDialect.md §3`
becomes `FactStore.collection::<Provider>()`; the same move that A1 makes to kill
thread-locals, A3 makes — and then adds the derivation layer on top.

> **Word/Noun/Fact, precisely.** A **Word** (Plane 1) is a Starlark builtin
> (`glob`, `rule`, `depset`). A **Noun** (Plane 1) is a `StarlarkValue`
> (`File`, `Depset`, `Ctx`). A **Fact** (Plane 2) is the *typed residue a Word
> leaves in the Session* — `cc_library(name="x", srcs=[...])` (a Word call)
> asserts a `Target{label, kind:cc_library}` Fact + N `Attr` Facts + M `DepEdge`
> Facts. Words write facts; matchers/rules read-and-derive facts; the engine
> consumes the derived `ActionReq` facts.

---

## 2. Worked example — `cc_library` as facts + matchers + compositional rules

Today (`crates/razel-loading/src/rules.rs:900-963`) `native_cc_library` is one
imperative function: it unpacks attrs, **walks `deps` and accumulates
`dep_hdrs`/`dep_cflags` inline** (the loop at `:914-920`), builds compile/archive
actions, and calls `record_target(...)` which mutates the `RESULTS` and `STATE`
thread-locals (`:847-852`). Every concern — attr typing, transitive header
propagation, toolchain choice (`CXX`/`AR` consts at `:844-845`), action synthesis,
config flags (`GLOBAL` thread-local at `:927`) — is fused into one body. **That
fusion is what A3 dissolves.**

### 2a. Facts (what `cc_library(...)` asserts, no behavior)

```rust
// A cc_library call (native shim OR evaluated rules_cc) asserts these and stops.
// No deps walked, no actions built, no toolchain chosen — those are DERIVED.
facts.assert(Target  { label: "//pkg:lib", kind: RuleKind::CcLibrary });
facts.assert(Attr    { label: "//pkg:lib", name: "srcs",    value: List(["a.cc","b.cc"]) });
facts.assert(Attr    { label: "//pkg:lib", name: "hdrs",    value: List(["lib.h"]) });
facts.assert(Attr    { label: "//pkg:lib", name: "defines", value: List(["FOO=1"]) });
facts.assert(DepEdge { from: "//pkg:lib", to: "//pkg:base", attr: "deps" }); // edge = a fact (F9)
```

`Target` and `Provider` are **family variants** (Rust enums) keyed by `RuleKind` /
`ProviderType`. `DepEdge` makes the declared graph **first-class data** — the same
edges `razel-ir::Graph` already stores, but available *during* analysis as queryable
facts (F9). Identity is the tuple `(label)` for `Target`, `(label, attr)` for `Attr`,
`(from, to, attr)` for `DepEdge`; the `FactStore` rejects duplicate identities
(YIDL's write-policy discipline → cheap F1 determinism).

### 2b. Matchers (decidable selection — the hard concepts, isolated)

```rust
// TOOLCHAIN RESOLUTION (F13) — one isolated, table-driven matcher, separately testable.
matcher!(ToolchainForTarget(t: Target, p: Platform) -> Resource::Toolchain {
    rule when t.lang == Cpp && p.os == Linux  => ClangLinux,
    rule when t.lang == Cpp && p.os == Darwin  => ClangDarwin,
    default => Err(NoCcToolchain),  // explicit failure, NOT a silent CXX="/usr/bin/c++" const
});

// CONFIG-DRIVEN ACTION CHOICE (F12) — select() lowered to a matcher over a Config fact.
matcher!(CompileMode(t: Target, c: Config) -> Resource::ActionTemplate {
    rule when c.opt == true  => CompileOpt,
    rule when t.linkstatic == false => CompilePic,
    default => CompilePlain,
});
```

`select({...})` (C7) lowers to a `Config` fact + a matcher, replacing today's stub
that just `return`s the default branch (`rules.rs:744-753`). The matcher table is
**Eq-and-AND only by default** — which is exactly YIDL's decidable core, and gives
the cheap definition-time confluence check (§4). Where Eq is too weak (ranges,
constraint subsumption), A3 uses a *typed predicate* (`Predicate::flag_in(set)`),
**not** YIDL's untyped "evaluated field" callable — the YIDL escape hatch
re-imperativizes the matcher (`YIDLDigest §5`), and A3 refuses it by keeping
predicates a closed, inspectable enum.

### 2c. Compositional rules (derive providers + action-requests)

```rust
// PROVIDER DERIVATION — isolate transitive cc-header/flag propagation into ONE rule-set.
rule!(CcInfoProvider from Target[kind==CcLibrary] derives Provider {
    // own contribution:
    hdrs   = attr(label,"hdrs"),
    cflags = define_flags(attr(label,"defines")) ++ include_flags(attr(label,"includes")),
    // TRANSITIVE accumulation across DepEdge — see §6 honesty box: this is a
    // controlled fold over the dep providers, expressed as a derivation primitive
    // (`fold_over_deps`), NOT inline imperative loops and NOT a YIDL evaluated field.
    transitive_hdrs   = fold_over_deps(label, |dep| provider::<CcInfo>(dep).hdrs),
    transitive_cflags = fold_over_deps(label, |dep| provider::<CcInfo>(dep).cflags),
});

// ACTION SYNTHESIS — one ActionReq fact per source; toolchain from the matcher.
rule!(CcCompileActions from Target[kind==CcLibrary] × srcs derives ActionReq {
    tool    = ToolchainForTarget.select().compile,   // resource, not a hard-coded const
    mnemonic= "CppCompile",
    inputs  = [src] ++ CcInfoProvider(label).transitive_hdrs,
    outputs = [src ++ ".o"],
    flags   = CompileMode.select() ++ CcInfoProvider(label).transitive_cflags,
});
rule!(CcArchiveAction from Target[kind==CcLibrary] derives ActionReq { /* ar rcs lib.a *.o */ });
```

`ActionReq` is a **fact**, not a side effect. The engine reads `ActionReq` facts and
lowers them — `wire_to_ir` (`crates/razel-analysis/src/analysis.rs:126`) becomes
*"for each `ActionReq` fact, emit an `ActionNode` + edges,"* which is nearly what it
already does for `AnalyzedAction` today; A3 just makes the source a derived fact set
instead of an imperatively-filled `Vec<AnalyzedAction>`.

### 2d. What this buys vs `native_cc_library`

- The 64-line god-body splits into ~5 independently-testable units: toolchain
  matcher, config matcher, provider-derivation rule, two action-synthesis rules.
- The `CXX`/`AR` hard-coded consts (`rules.rs:844`) become a `Toolchain` resource
  selected by a matcher → F13 satisfied without rewriting the rule.
- The `GLOBAL` thread-local for CLI copts (`rules.rs:927`) becomes a `Config` fact
  fed into `CompileMode` → F12 + F19 (state is a passed plural value).
- Adding `rust_library` is **a new rule-set file** (new `RuleKind` variant + its
  matchers + derivation rules) registered with **one manifest line** — the
  consumer (`BuildAll`, the engine's action loop) never names `rust_library`
  (F15/F16, the `lifecycle_local_store.yidl` "one file, zero core edits" template).

---

## 3. Analysis-as-derivation (the control flow)

```
load BUILD/.bzl (Plane 1, faithful Starlark) ──► Words assert FACTS into Session.FactStore
                                                            │
   ┌────────────────────────────────────────────────────────┘
   ▼  DERIVATION (Plane 2) — STRATIFIED but DEMAND-DRIVEN (the key divergence from YIDL):
   for each requested target (demand-driven, F10 — NOT whole-store once-through):
     1. resolve config + toolchain via matchers           (F12/F13, isolated)
     2. run provider-derivation rules over its dep closure (F11 — provider is the contract)
     3. run action-synthesis rules → ActionReq facts        (action = derived fact)
                                                            │
   ▼  LOWERING
   ActionReq facts ──► wire_to_ir ──► razel-ir::Graph (rdeps/impact)
                                                            │
   ▼  EXECUTION (Plane 3, KEPT AS-IS)
   razel-engine::Engine (demand-driven, early-cutoff) ──► razel-exec (content-addressed)
```

**The single most important design decision in A3:** derivation is driven *by demand
from the requested targets over the dep graph*, **not** by YIDL's once-through
ordered sweep of the whole store. A3 borrows YIDL's *stratification within a target*
(toolchain → providers → actions are ordered phases) but keeps razel's *demand-driven
graph between targets* (F10) and the `Engine`'s node-granular incrementality (F5/F6).
This is the line in `YIDLDigest §7` "borrow the authoring model, NOT the evaluation
engine" made concrete.

---

## 4. Part C dial placement — per ★ fundamental, with mechanism & cost

| ★ Fundamental | A3 column | mechanism | how well / how cheaply |
|---|---|---|---|
| **F11 decoupling** | ●→ (typed providers, derived) | matchers select a `Resource`/`ProviderType`, never a producer; `Provider` facts are the only inter-unit channel | **Strong & cheap.** This is YIDL's best idea and it lines up exactly with R4/R8. Provider facts are typed from day one — fixes razel's "implicit DefaultInfo/hdrs/cflags bundle" (`ArchModel §4`). |
| **F5/F7/F10 graph** | ◑ (demand-driven Engine on live path) | **KEPT from razel** — derivation feeds `razel-ir::Graph` + `razel-engine::Engine`; A3 adds nothing here and explicitly rejects YIDL's once-through model | **Adequate, by *not* touching it.** The honest weakness: A3 must prove derivation itself is incremental (re-deriving only affected targets' facts), which YIDL gives no help with — see §6. |
| **F12 config** | ◑→● (matching, typed) | `select`/`config_setting` lower to `Config` facts; `CompileMode`-style matchers select per config | **Good.** Decides the config model early (R9) instead of the current stub. Cost: a real config-axis means the fact store is keyed by `(label, config)` — modest blowup, far less than Bazel's `(Target×Configuration)` thicket because matchers are flat tables. |
| **F16 extension** | ● (open-set, manifest) | a language = one rule-set file (facts + matchers + rules) + one manifest line; generic ops (build/test) enumerate `ActionReq` facts, never the rule-set | **Strong & cheap — the headline win.** Matches the `lifecycle_local_store.yidl` proof. Same const-array manifest discipline `RazelDialect.md §2` already recommends (not `inventory` auto-reg). |
| **F18/F15 eval** | ◑ (dialect + Session) **above**, ◑ (facts+matchers) **below** | Plane 1 keeps the sandboxed Starlark interpreter (R1, already won); Plane 2 adds the fact/matcher layer as the *structured residue* | **Good, but this is the comprehension-cost spot.** Two declarative layers (Starlark + facts/matchers) is genuinely "M2-ish" — the learning wall is real (§6). |
| **F19 state** | (invariant — no dial) | `FactStore` lives on the per-analysis `Session`; **zero thread-locals**; plural, passed, scoped | **The keystone, satisfied by construction.** Facts-as-substrate *forces* state to be a value (you cannot assert a fact without a store handle), which is structurally stronger than A1's "remember to thread the Session" discipline. |

Invariant columns (same as all candidates): **P-D** content-addressed kernel
(`razel-exec`, kept), **P-E** passed Session, **P-J** pure-data unit tests, clean
front-end→IR seam (`ActionReq` fact → `wire_to_ir`).

---

## 5. Bazel constraints (front-end fidelity) + the four invariants

**Constraints C1–C9** (A3 satisfies them by *keeping Plane 1 unchanged*):
- **C1 Starlark / C4 rule-authoring API:** untouched. `rule()`, `ctx`,
  `provider()`, `depset()` evaluate exactly as today; A3 changes only what their
  *effects* are (assert facts, not mutate thread-locals). An evaluated `rules_cc`
  `cc_library` impl that calls `ctx.actions.run(...)` has that call lower to an
  `ActionReq` fact — the impl never knows it's feeding a fact store.
- **C3 native builtins / C5 ruleset `load()`:** `glob`/`select`/`filegroup`/… stay
  Words; `@rules_cc//…` resolves to a native fact-asserting rule-set or to evaluated
  `.bzl`. Either way the residue is facts. C2 labels, C6 per-language conventions,
  C7 config/platforms, C8 external deps, C9 CLI: all Plane-1/CLI concerns, unchanged.
- **The seam holds:** the engine (Plane 3) sees only `ActionReq`/`Provider`/`Target`
  facts → `ActionNode`/edges. **It never learns "cc_library."** `RuleKind::CcLibrary`
  exists only in the cc rule-set (Plane 2); the IR has only `TargetKind::Library`
  (`razel-ir/src/lib.rs:26`). This is *better* seam hygiene than today, where
  `wire_to_ir` infers kind from name suffix (`analysis.rs:112-120`).

**The four non-negotiable invariants:**
1. **Analysis state is a passed, scoped, plural value (F19).** ✅ `FactStore` is a
   `Session` field; many sessions coexist. This is the *strongest* form of the
   invariant — facts cannot be asserted ambiently.
2. **World-effects quarantined to a content-addressed kernel, pure above (F2/F3).**
   ✅ Unchanged — `razel-exec` is Plane 3. Derivation is pure (facts in, facts out),
   which makes "pure above the kernel" true *by the type of `DerivationRule`*.
3. **Units unit-testable on pure data, no toolchain.** ✅ A matcher tests as
   `select(fact_tuple) == Resource` (no Evaluator, no compiler); a derivation rule
   tests as `derive(fact_view) == fact_delta`; action synthesis asserts on the
   `ActionReq` fact's `argv` — exactly the captured-`AnalyzedAction` pattern razel
   already uses (`rules.rs:1597-1650` tests), now at finer grain.
4. **Clean front-end→IR seam.** ✅ Strengthened — the seam is now *typed facts*, not
   a `Vec<AnalyzedTarget>` with stringly-typed `hdrs`/`cflags` fields.

---

## 6. The honest dead-end risk — A3 courts the **too-abstract** end

A3's characteristic risk is unambiguous: **too-abstract** (the right-hand dead end of
`ArchitectSkillRules.md`). Named failure modes, each grounded:

- **Non-firing matchers / silent absence (the worst, ~F21).** YIDL's matchers
  fall through to `default` or `None` with *no provenance* (`YIDLDigest §5`,
  `matcher.py:386-388`). For a build tool whose daily question is *"why didn't this
  action run?"*, silent non-firing is disqualifying. **A3's mandatory mitigation
  (non-optional):** every matcher carries a `default => Err(...)` (no silent
  `None`), and the `FactStore` records a **derivation log** — which rule fired, which
  facts it read, which it produced — so F21 is answerable. This is the one place A3
  *must* out-engineer YIDL or it fails its own fundamentals. If the log is skipped,
  A3 degrades to "schema, not a modular generator" (`YIDLDigest §3`) with worse
  debuggability than today's straight-line `rules.rs`.

- **Ordering / ambiguity / confluence (S9-adjacent rigidity *and* nondeterminism).**
  Within one matcher, A3 keeps YIDL's definition-time equal-score-overlap rejection
  (`matcher.py:715`) — a cheap static guarantee (F1). **But** cross-rule-set phase
  ordering (cc derives before rust, etc.) is a *distributed* decision
  (`YIDLDigest §5` flags this), and two rule-sets each claiming the same derivation
  phase is a latent nondeterminism the per-matcher check does not cover. Mitigation:
  derivation phases are a *typed, total* enum (`Phase::{Config, Toolchain, Provider,
  Action}`) not a free-form `after X order N` string — trading YIDL's open phase
  ordering for closed, checkable ordering. Cost: less open than YIDL (closer to S9
  rigidity), which is the *right* trade for a build tool.

- **Comprehension / learning wall (the M2 tax).** Two declarative layers (Starlark
  surface + facts/matchers/rules below) is genuinely two levels up. A reader must
  understand "a `cc_library` call asserts facts that matchers select resources for
  that rules compose into action facts the engine lowers." `YIDLDigest §5` is candid
  that `local_store.yidl` is legible but `lifecycle_managed.yidl` (1,545 lines) is
  not. **This is A3's biggest soft cost** and the reason it is presented as a
  candidate, not a recommendation: it may be *more* indirection than razel's ~12
  builtins + 4 languages justify (the R7 "don't out-abstract ~12 builtins" warning).

- **Perf.** Astichi needed a Rust hot path for its assembler (`YIDLDigest §5`). A
  build runs analysis far more often than a once-per-class codegen. A3's fact
  store + matcher dispatch must be cheap per-target; the demand-driven scoping (§3)
  is what keeps it from YIDL's whole-plan cost, but it is a real constant factor to
  watch.

**Scars from `ArchPatterns.md` Part B that A3 most courts:**
- **S9 (static-graph startup rigidity)** — closed phase enum + definition-time
  confluence is deliberately *toward* Pants's rigidity; A3 accepts a slice of S9 to
  buy determinism, and must avoid the full "missing rule = startup failure" version
  by keeping the *Starlark* surface dynamic (Plane 1 stays ◐).
- **S8 (over-engineering / out-building the need)** — the headline risk. A
  fact/matcher/derivation engine for 4 languages is plausibly more machinery than the
  problem demands; A3 is the candidate most exposed to "you built a dataflow compiler
  for a job a typed Session + open-set dispatch (A1) already does."
- **S10 (wrapper-type proliferation)** — `Resource`/`ProviderType`/`Fact`-variant
  sprawl if value-types lack clear identity. Mitigation: keep `Fact` a *small closed
  enum*, not an open trait-object zoo.

---

## 7. a/b/c assessment (skeptical, on balance)

- **(a) Modularity / smallest-edit — STRONG.** This is A3's genuine win and it is
  real, not aspirational: a new language is one rule-set file + one manifest line,
  with the consumer enumerating `ActionReq` facts and never naming the language. The
  64-line `native_cc_library` god-body dissolves into ~5 independently-addable units.
  The `rules.rs` 1,658-LOC gravity well (R10/S5) has *no place to reform* because
  there is no central recording function to funnel into — facts are asserted locally.
  **This beats A1** (the Dialect) on (a) at the rule-authoring grain: A1 still has
  imperative `cc_rules` bodies; A3 makes those declarative rule-sets.

- **(b) Testability — STRONG, the finest grain of the three.** Matcher = pure
  `fact_tuple → Resource`. Derivation rule = pure `fact_view → fact_delta`. Action
  synthesis asserts on the `ActionReq` fact. *All* without an Evaluator or toolchain
  — killing the ~28 `exists(){return}` env-gated skips (S11). The confluence check is
  itself a unit test. This is YIDL's cleanest transferable property (`YIDLDigest §4`)
  and it maps directly onto R12.

- **(c) Calibrated extensibility — OVER-CALIBRATED (the honest mark-down).** A3
  spends the *most* indirection of the three candidates: a whole derivation engine
  below the surface. For the axes razel can name (new language, new toolchain, new
  config), A1's open-set dispatch + typed Session already delivers most of (a)/(b) at
  a fraction of the comprehension cost. A3's extra layer pays off **only if** razel
  grows orthogonal derivations — IDE data, lint, coverage, codegen (F17, "extended
  core") — where "derive new facts from the existing graph without editing the rules
  that built it" is exactly the aspect/union story and A3 expresses it natively.
  **If razel stays a minimal core, A3 is over-built (S8).** That conditional is the
  crux of the recommendation and I will not paper over it.

**Net:** A3 wins (a) and (b) decisively and loses (c) on calibration. It is the
*right* architecture **if and only if** orthogonal derivation (F17) is a named goal;
otherwise A1 (Dialect + typed providers) gets ~80% of the benefit for ~40% of the
conceptual cost, and A3 is the too-abstract dead end this very project warns about.

---

## 8. Migration path from today (62 commits in)

The non-negotiable prerequisite (shared with A1, from R14/S7): **delete the dead
second loader first.** `crates/razel-loading/src/lib.rs` carries `TargetDecl`
(`:33`), `load_build` (`:195`), and the `CTX` thread-local (`:58`) — a parallel
pipeline reachable only from `razel-analysis::analyze` (`analysis.rs:48`) and its own
tests. The *live* path is `analyze_starlark`/`analyze_bazel` → `AnalyzedTarget` →
`wire_to_ir`. **Migrating onto facts before deleting the dead loader would spawn a
third target/depset representation** — exactly the R14 warning. So:

1. **Delete the dead loader (R14/S7).** Remove `TargetDecl`/`load_build`/`CTX` and
   the `razel-analysis::analyze`+`DefaultInfo` path; keep `wire_to_ir`. Green
   throughout (the dead path has only self-tests). *This step is shared with A1 and
   should happen regardless of which architecture wins.*

2. **Introduce the `Session`/`FactStore` (kills the 5 live thread-locals).** Create
   `razel-facts` with the `Fact` enum + `FactStore`; add `FactStore` to the
   `Analysis` session (`RazelDialect §3`). Route `record_target` (`rules.rs:847`) to
   assert `Target`/`Provider` facts into the store instead of `RESULTS`/`STATE`.
   Migrate `WORKSPACE`/`CURRENT_PKG`/`LOADED`/`GLOBAL`/`CONFIGS` to session fields one
   at a time. **This is identical to A1's keystone move** — A3 and A1 share steps 1–2
   exactly, so this migration is *not* a bet on A3; it's a no-regret prefix.

3. **Lower one rule-set to facts behind the existing API (strangler).** Reimplement
   `native_cc_library` as a cc rule-set (facts + matchers + derivation rules) feeding
   `ActionReq` facts, while keeping `AnalyzedTarget`/`wire_to_ir` as the output shape
   (derive `AnalyzedTarget` from the facts at the seam). cc is the right pilot — it
   has the transitive-propagation hard case that proves or breaks the model. **This
   is the first A3-specific commit and the go/no-go gate**: if expressing transitive
   `hdrs`/`cflags` propagation as `fold_over_deps` is *not* meaningfully cleaner and
   more testable than the inline loop at `rules.rs:914-920`, **stop** — that is the
   `cc_library` sketch's known weak spot (`YIDLDigest §6`, "transitive depset
   accumulation falls outside the declarative layer"), and its failure here is the
   honest kill-signal for A3.

4. **Add the derivation log (F21) before any second rule-set.** Non-optional —
   without provenance, A3 is strictly worse than today on debuggability.

5. **Migrate rust/py/sh rule-sets**, each one file + one manifest line. At this point
   the `rules.rs` god-module is gone: Words live in `words/`, Nouns in `nouns/`,
   rule-sets in `rulesets/` as fact-derivers, the assembler is thin.

6. **(Only if F17 is adopted)** add orthogonal derivations (lint/IDE/coverage) as
   new derivation rules over existing facts — the payoff that justifies A3's extra
   layer. If F17 is *not* adopted, stop at step 5 and note that A1 would have reached
   the same place with less machinery.

**Reversibility:** steps 1–2 are pure wins shared with A1. Step 3 is the single
reversible bet — if the cc pilot fails the go/no-go, revert to A1's imperative
rule-set bodies over the same Session, losing nothing from steps 1–2.

---

*Draft A3. Inspiration from YIDL/M2, explicitly not adoption: facts-as-substrate,
matcher selection, phase-splice extension, closed plan over a blind executor, and
definition-time disambiguation are stolen; YIDL's once-through, fixpoint-free,
provenance-poor resolution is rejected and razel's demand-driven `Engine` + content-
addressed kernel are kept underneath. The honest verdict: best-in-class on
modularity (a) and testability (b); over-calibrated on (c) unless orthogonal
derivation (F17) is a named goal — in which case it is the only candidate that
expresses it natively. Courts the too-abstract dead end (S8) and a slice of S9.*
