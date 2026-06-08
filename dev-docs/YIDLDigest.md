# YIDL + Astichi Digest — INSPIRATION for a build-tool architecture

Status: research digest, not a proposal. Read against `ArchFundamentals.md`
(F1–F23) and `ArchModel.md` (the distilled model + R1–R15). The verdict at the
end states what is transferable vs idiosyncratic. **Do not treat YIDL's vocabulary
as a solution to adopt — it is a worked example of one slicing of the same
problem, with its own scars.**

Sources read (cited inline by path):
- `grip-pyrolyze-dev/yidl/src/yidl/concept_grammar.lark` (the `.yidl` surface)
- `grip-pyrolyze-dev/yidl/src/yidl/concept_parser.py` (lowering)
- `grip-pyrolyze-dev/yidl/src/yidl/generation/matcher.py` (matcher runtime + resolution)
- `grip-pyrolyze-dev/yidl/src/yidl/generation/assembly_runtime.py` (assembly execution)
- `grip-pyrolyze-dev/yidl/dev-docs/YidlDesignSummary.md` §26.1 (DDS substrate)
- `grip-pyrolyze-dev/yidl/dev-docs/YidlDesignSummary-gaps2.md` (current reality)
- `grip-pyrolyze-dev/yidl/MetaSquaredLog.md` (the meta/M2/M3 framing)
- `grip-pyrolyze-dev/astichi/README.md`, `astichi/dev-docs/AstichiAssemblerConcept.md`
- `grip-pyrolyze-dev/yidl-lifecycle/src/yidl_lifecycle/yidl/*.yidl` (real consumer)

One readability caveat: `YIDLDesign.md` is marked historical (the v2 transducer
compiler); the live system is the **recorded-concept / DDS / assembly path** per
`YidlDesignSummary-gaps2.md §3`. Everything below describes the live path.

---

## 1. FACT, MATCHER, COMPOSITIONAL RULE — precise definitions

YIDL is a *compiler-description* system: you declare a domain's architecture as
data, and a generator lowers that data to source. The substrate is the
**DataDefinitionSystem (DDS)** (`YidlDesignSummary.md §26.1`). The three primitives:

### FACT — a record in a collection (`concept_grammar.lark:59-91`, `matcher.py:40-67`)
A fact is a typed **record** instance held in a named **collection**.
- A `property` is a typed, named slot with a default and a *storage name*
  (`property CNAME : type_expr default_clause? storage_clause?`,
  grammar:59-62). Property names are semantic; storage names are the generated keyword.
- A `record` is a fixed set of property-refs (grammar:71-72); a `family`/`variant`
  is a tagged union of record shapes sharing `common` properties (grammar:77-80).
  Records are plain slotted Python objects with `__dds_record_spec__`, *not*
  dataclasses; constructors validate declared types (`§26.1.3`).
- A `collection` is the stored fact-set: `collection CNAME : type_ref identity?
  cardinality?` (grammar:81-88). Identity is one property or a property tuple;
  inserts strictly reject duplicates unless an explicit write policy
  (`AddIfAbsent` / `ReplaceExisting`) is given (`§26.1.5`).
- A `computed collection` is a *derived, non-stored* filtered view:
  `computed collection X : Shape from Y where Prop == value` (grammar:89-90).
  It returns existing source records matching the predicate (`§26.1.6`) — this
  is the cheapest derivation: a fact-subset by Eq-predicate.

So: **facts are immutable typed records; the collection is the namespace; the
record shape (or family variant) is the type.** This is YIDL's analogue of
Bazel/Pants *providers* — typed data with declared identity — but stored as a
queryable set, not just attached to a target.

### MATCHER — declarative Eq-rule dispatch over fact tuples (`matcher.py:107-416`, grammar:121-131)
A matcher selects an output *resource/contribution/operation* from a tuple of
input facts. Declared as:
```
matcher Name(input: SomeCollection, ...) -> contribution {     # or -> resource / -> operation
    default -> SomeContribution
    rule r1 when input.Prop == "x" and input.Other == True -> ContribA weight 2
    rule r2 when input.Prop == "y"                          -> ContribB
}
```
(grammar:121-131; lowered in `concept_parser.py:1015-1191`).
- Inputs bind named collections; conditions are **Eq-only** (`condition_term:
  value_expr "==" value_expr`, grammar:209), AND-combined (grammar:207-208).
  No `<`, no `or`, no negation, no joins beyond the input cross-product.
- The matched tuple is fixed positional (`tuple_schema`, `matcher.py:180-186`);
  values come from record property reads or from explicit **evaluated fields**
  (a registered callable over inputs — `matcher.py:219-246`, `§26.1.17`), which
  is the controlled escape hatch from pure Eq.
- A matcher is a **definition-time object** with a **lazy cached runtime
  evaluator** (`MatcherRuntime`, `matcher.py:318-416`): `resolve(*records)`
  extracts the tuple, memoizes, and selects.

### COMPOSITIONAL RULE — three layers that derive new facts / new structure
1. **A matcher rule** (above) is the atomic compositional rule: *"facts shaped
   like this → contribute this resource."* It is the producer→consumer seam:
   the rule names a *resource*, never the code that built it (`§26.1.19`,
   `from_literal` / `from_astichi_code` / `from_import`).
2. **A production** derives a *new collection of facts* from a source
   (collection, computed collection, or `matcher.results()`):
   `production Name from Src to Target { set Prop = match.record("in").prop(P);
   identity ...; policy ... }` (grammar:162-169; `concept_parser.py:1266-1348`).
   Value expressions can read matched records (`match.record(...).prop`), the
   selected resource (`match.resource()`), tuple values (`match.value(i)`), or
   do keyed `lookup(coll, key=…, value=…)` joins against another collection's
   identity (`§26.1.20`, grammar:223). Productions are grouped into ordered
   **execution groups** (`ProductionGroupSpec`, `data_schema.py:527-538`).
3. **A composable production / assembly** is the top compositional rule: it
   builds a *code artifact* (a tree of Astichi scopes). It declares a `root`
   resource and a sequence of `phase`s, each with `apply ... using <matcher>`
   that expands facts → contributions placed at named targets/holes (grammar:170-192).
   Crucially, `extend production X { phase P after Q order N {…} }`
   (grammar:180-182) lets a *different module* splice new phases into an existing
   pipeline **without editing it** (see the leverage example below).

### Resolution model — **ordered + statically-disambiguated, NOT fixpoint, NOT Rete**
Pinned from source, this is the load-bearing finding:
- **Matcher selection is single-pass, highest-score-first.** Rules are sorted by
  `score = len(conditions) * weight` descending; the **first** rule whose Eq-tuple
  matches wins; else `default`; else `None`
  (`matcher.py:330,376-388,572-579`; assembly path identical at
  `assembly_runtime.py:387-401`). No iteration to convergence.
- **Ambiguity is rejected at definition time, not resolved at runtime.**
  `_validate_no_equal_score_overlaps` (`matcher.py:715-745`) raises if two
  equal-score rules can match the same tuple (i.e. they agree on every shared
  tuple position). Determinism is therefore a *checked static property*, not an
  emergent runtime tie-break. This is YIDL's confluence guarantee, bought
  cheaply because the only operator is Eq.
- **Derivation is a fixed pipeline, not a saturating engine.** Productions run in
  declared **ordered groups** (`data_schema.py:527`); assemblies run as a
  **post-order scope tree** with phases ordered by `after`/`order`
  (`AstichiAssemblerConcept.md §4,§13.4`). Grep for `fixpoint|saturat|iterate
  until` across `yidl/src` returns nothing. New facts produced by group N are
  visible to group N+1, but there is **no re-triggering** of earlier groups —
  it is stratified/layered, closer to a topologically-ordered dataflow than to
  Datalog-with-recursion or Rete.

Net: **ordered, stratified, confluent-by-construction dispatch.** It deliberately
trades Rete's incremental re-firing and Datalog's recursive closure for
*decidability and legibility*. That choice is the whole personality.

---

## 2. Astichi's assembler role + the meta / M2 level

**Astichi is the executor below the rule layer, with a hard semantic firewall.**
- Astichi (`astichi/README.md`) is an **AST stitcher**: marker-bearing Python
  snippets (`astichi_hole`, `astichi_pass`, `astichi_bind_external`, `astichi_ref`,
  `astichi_for`, `astichi_pyimport`, keep/import/export) composed at named sites,
  with **isolated scopes** (no implicit capture across a boundary — README "the
  one rule that matters most is scope") and compile-time bind/unroll, emitting
  inspectable straight-line Python.
- The **assembler** (`AstichiAssemblerConcept.md`) is a two-phase plan compiler:
  **Expansion** (the client — YIDL — answers demands from collections/matchers and
  emits a closed `AssemblyPlan` of scopes + contributions + edge overlays +
  identifier bindings) then **Execution** (Astichi validates descriptors,
  resolves target holes, checks source/target compatibility, builds child scopes
  before parents in **post-order**, and materializes only the requested artifact;
  §4,§7). The plan is **callback-free after expansion** (§3) — exactly the
  "closed, deterministic execution" discipline F18/F19 want.
- **The firewall is the point**: "Astichi should not learn YIDL concepts"
  (§2); it never knows what `managed`, a store, or a tx-key means
  (`YidlDesignSummary.md §26.2`). YIDL owns *semantics and selection*; Astichi owns
  *composable shape, hole resolution, hygiene, deterministic build*. This is the
  same separation Bazel draws between rule analysis and action execution
  (the assembler doc cites it, §1) — and the same one `ArchModel.md` calls the
  pure-above / world-effects-quarantined-kernel invariant (R6).

**The meta / M2 level** (`MetaSquaredLog.md`):
- The stack is three layers (M2): `ordinary program <- generator <- description
  model` (lines 11-15, 393-399). The developer does not write lifecycle behavior;
  they describe *how lifecycle-behavior descriptions compose into a generator*
  (lines 401-404). DDS is "a way to define the description of the problem itself,
  make it mergeable, extend it, and use it to generate code that generates code"
  (lines 387-389).
- **Is the definition-language itself defined this way? Not yet, and deliberately
  so.** "Bootstrapping YIDL with YIDL" (lines 418-454) and "Meta3" (lines 925-968)
  are explicit *aspirations held back*: self-hosting is treated as a coherence
  *test* ("if YIDL can model its own properties/records/matchers/productions… the
  language is probably coherent", 960-963) and a "rabbit hole if attempted before
  the model is proven" (948-950). The `.yidl` grammar is a hand-written Lark
  grammar (`concept_grammar.lark`), not a YIDL-defined one. So M2 is real and
  load-bearing; **M3 (the language defining itself) is named but not built** —
  and the log argues it should stay that way until pressure demands it (a
  discipline `ArchModel.md R15` would endorse).

---

## 3. THE LEVERAGE — why this slicing modularized the otherwise-unmodularizable

The forcing pain (`MetaSquaredLog.md §"Evidence From Lifecycle And DDS"`,
`CLAUDE.md` north star): lifecycle codegen is *combinatorial* — N field kinds ×
M surfaces (init / property / commit / rollback / store-slot / facade) ×
transaction/ownership variants — and the naive realization is one imperative
emitter with a god-branch per field kind. That does not modularize: adding a
field kind edits every emitter.

The leverage is a **specific interplay of (a) and (b)**, with (c) as a *contained*
amplifier — not any one alone:

- **(a) Knowledge decomposed into independent facts is necessary but not
  sufficient.** A field kind becomes a *family variant* + a *computed collection*
  (`FieldKind == "local_store"`) — pure data, addable in isolation. But facts
  alone don't place code.
- **(b) Behavior emerging from matching/composition vs imperative wiring is the
  load-bearing half.** The placement of generated code is decided by **matcher
  rules selecting contributions into named holes of an open pipeline**, and new
  behavior is added by **`extend production … phase … after …`** — splicing a
  phase into an existing assembly *without editing the producer*. The consumer
  (the core pipeline) never names the new field kind; the new kind names the
  pipeline's phase anchor. That is **producer→consumer decoupling via a published
  contract** — `ArchModel.md` R4/R7/R11 and `ArchFundamentals.md` **F11, F15, F16,
  F17** — realized as data, so "support a new field kind under `commit`" requires
  zero edits to `commit`.
- **(c) The meta-level is the amplifier, not the lever.** Because the description
  is *mergeable* (`extends`, `family … extend`, `extend production`) the same
  decomposition that buys modularity buys *layering*: each lifecycle feature is
  one `.yidl` file that extends the previous concept.

**The pinned proof** is `lifecycle_local_store.yidl` (114 lines, read in full):
an entire new field kind — `LocalStoreField` variant, `LocalStoreFields` computed
collection, three `resource` templates, three `contribution`s, three `matcher`s,
and an `extend production CoreClassProduction { phase … after state_slots_plain
… }` — added with **zero edits to `lifecycle_core.yidl`**. That is the thing
imperative codegen cannot do without touching the central emitter. The slicing
works because *selection* (matchers over facts) and *placement* (contributions
into open holes) are both data, and the pipeline is *open for extension by phase
splice*. **If you remove the matcher/phase layer and keep only facts, you get a
schema, not a modular generator** — so (b) is the lever, (a) is its substrate,
(c) compounds it.

---

## 4. Testing FACTs / MATCHERs / RULEs in isolation (finer units)

The decomposition directly buys finer test units (the `ArchModel.md` (b) lens,
F-list testability R12):
- **Facts** test as plain record/collection construction + query: identity,
  cardinality, write-policy, computed-collection filtering — no generator, no
  Astichi (`tests/generation/test_data_productions.py`,
  `tests/data/gold_src/dds_collection_productions.py`,
  `dds_write_policy.py`, `dds_composite_identity_lookup.py`).
- **Matchers** test as pure dispatch: build a `MatcherRuntime`, call
  `resolve(record_a, record_b)`, assert the selected resource + rule + score —
  no code emitted (`tests/generation/test_matcher.py`;
  `data/gold_src/matcher_runtime.py`, `matcher_runtime_ranges.py`). The
  equal-score-overlap rejection is itself a unit test of the confluence
  guarantee (`matcher.py:715`).
- **Compositional rules** test at two altitudes: production derivation as
  data-in/data-out (`test_data_matcher_productions.py`,
  `dds_matcher_productions.py`), and assembly as **materialized-source golden**
  (`test_assembly_runtime.py`; `actual_test_results/**/materialized/*.py`).
- **Astichi** tests independently of YIDL entirely (its own `tests/`), and the
  assembler doc carves "leaf-shaped" slices (L1 scope-build-set lowering, L2
  descriptor query/compatibility) explicitly so they are *testable with
  handcrafted `ScopeBuildSet` values, no matcher, no expansion driver*
  (`AstichiAssemblerConcept.md §13`).

This maps cleanly onto `ArchModel.md` R12: "rule bodies assert on captured pure
data, integration tests sit at the top." YIDL's golden-source materialization is
exactly the "assert on `AnalyzedAction` (pure data)" pattern — assert on emitted
*text*, not on running it.

---

## 5. Limits / failure modes (skeptical)

- **Eq-only matching is a low ceiling.** Conditions are `==` AND-chains
  (grammar:207-209). Ranges, ordering, negation, and joins are simulated through
  **evaluated fields** (arbitrary callables, `matcher.py:219`) — which reintroduces
  exactly the opaque imperative logic the model claims to banish, just relocated
  into a Python function the matcher can't reason about. The "matcher" is then
  only as declarative as authors keep their evaluators.
- **Debugging a non-firing rule.** Selection is silent-fall-through to `default`
  or `None` (`matcher.py:386-388`). A rule that never fires (typo'd Eq value,
  wrong tuple position, lost a higher-score rule) produces *no error* — just
  absent code, surfacing far downstream as a missing hole or a materialization
  failure. The assembler doc flags this as its "third risk" (§12) and pushes
  *descriptor preflight* as mitigation, but the matcher layer has no
  "why did rule R not match this fact" explain facility. This is the inverse of
  `ArchFundamentals.md` **F21 (explainability/provenance)** — and it is the model's
  weakest spot for a build tool, where "why didn't this action run" is daily bread.
- **Ordering/confluence is bought by stratification, which is itself fragile.**
  Phases ordered by `after X order N` (grammar:178-179) are a hand-maintained
  total order; cross-module `extend production` makes that order a *distributed*
  decision. Equal-score overlap is checked **within one matcher**, but ordering
  *across phases/productions/extensions* is not statically confluence-checked the
  same way — two modules each splicing `order 8` after the same anchor is a
  latent nondeterminism the Eq-overlap checker does not cover.
- **No fixpoint = no recursive derivation.** Anything needing transitive closure
  (e.g. transitive dep flattening, reachability) cannot be expressed as
  facts→rules; it must be an evaluated field / operation in Python
  (`§26.1.13-15` push initvar-closure and callable-fact extraction into
  *generated operations*, not the rule layer). For a build tool this is a real
  gap: transitive `deps` / depset accumulation is the core operation and would
  live *outside* the declarative layer here.
- **Comprehension wall (M2 is genuinely two levels up).** The log is candid: the
  developer reasons about *how descriptions compose into a generator* (lines
  401-412), and "the generated compiler must remain understandable" is listed as
  a hard constraint, not a given. `local_store.yidl` is legible; a 1,545-line
  `lifecycle_managed.yidl` is not obviously so. The DDS is also **flat, not
  hierarchical** (lines 436-440), explicitly flagged as a possible hard limit for
  complex domains.
- **Performance.** Astichi needed a native Rust engine for the assembler hot path
  (`astichi/README.md` "Native rust fast path"; `dev-docs/AstichiPerfAnal.md`,
  `HotPathNoPythonPlan.md`) — i.e. the elegant data model has a real per-build
  constant-factor cost that had to be engineered away. A build tool runs this
  loop far more than a once-per-class codegen does.

---

## 6. Concrete sketch — `cc_library` as facts + matchers + compositional rules

Mapping a Bazel-style `cc_library` (today: `rule(impl, attrs, provides)` +
imperative `ctx`) onto the YIDL slicing. This is a *thought experiment* for the
razel target, not a recommendation.

**Facts (collections of typed records):**
```
collection Targets:    Target   identity Label      # one fact per declared target
collection Attrs:      AttrValue identity (Label, AttrName)   # srcs, hdrs, copts, deps...
collection DepEdges:   DepEdge  identity (From, To)  # the declared graph, as facts
collection Providers:  Provider identity (Label, ProviderType)  # CcInfo, DefaultInfo...
collection Actions:    Action   identity ActionId    # emitted spawns (derived, see below)
```
`Target`, `Provider` are family variants by `RuleKind` / `ProviderType`. The
declared BUILD graph is *just facts*; `DepEdge` makes edges first-class
(`ArchFundamentals.md F9`).

**Matchers (selection over the fact context):**
```
matcher ToolchainForTarget(t: Targets, p: Platforms) -> resource {
    rule host  when t.Lang == "cpp" and p.Os == "linux"  -> ClangLinuxToolchain
    rule cross when t.Lang == "cpp" and p.Os == "darwin" -> ClangDarwinToolchain
    default -> CcToolchainError
}                                                  # = toolchain resolution, F13
matcher CompileActionFor(t: Targets) -> operation {
    rule with_pic when t.Linkstatic == False -> CompilePicOp
    default                                  -> CompileOp
}                                                  # config-driven action choice, F12
```
`select()` becomes a matcher over a `Config` fact input — `ArchModel.md R9`
("configurability is part of the attribute model") realized as Eq-rules over a
config fact rather than a stubbed `select`.

**Compositional rules (productions derive new facts; assembly emits actions):**
```
# derive the providers a cc_library publishes, from its attrs + deps' providers
production CcLibProviders from CcTargets to Providers {
    set ProviderType = "CcInfo"
    set Headers      = lookup(Attrs, key=Label, value=Hdrs)
    # transitive include dirs across deps: NOT expressible as Eq-rules ->
    # an evaluated field / generated operation (the model's escape hatch, see §5)
}
# derive compile/link actions; one Action fact per source
production CcCompileActions from CcSources.results() to Actions {
    set Tool   = match.resource()             # toolchain chosen by matcher
    set Inputs = (match.record("src").prop(Path), ...)
    policy RejectDuplicate
}
# the build is then a composable production: phases over the action facts,
# each phase placing a spawn; deps' provider facts are visible because the
# dep-targets' provider production ran in an earlier ordered group.
production BuildCcLibrary -> composable {
    root cc_link = CcLinkAction { ... }
    phase compile order 0 from a: CompileActions { apply emit using CompileActionFor }
    phase link    order 1 ...
}
```
A *new* language (`rust_library`) is added the `local_store.yidl` way: a new
concept file with new variants + computed collections + matchers + an `extend
production BuildAll { phase rust after cc … }` — **no edit to the cc rules or to
`BuildAll`** (`ArchFundamentals.md F15/F16`).

**What this buys vs `rule()+ctx`:** the god-constructor (`ArchModel.md R10`,
Bazel's 22-param `rule()`) dissolves — "is this a test", "has a transition",
"provides CcInfo" are separate facts/matchers/productions, separately testable.
Ambient analysis state (`R3`, razel's 8 thread-locals) becomes the fact store +
the closed `AssemblyPlan` — a *plural, scoped value* by construction
(`F19 concurrency-safe partitionable analysis state`).

**What it does NOT buy / where it fights the build domain:**
- **Transitive depset accumulation** — the daily core of a build tool — is not a
  fact/Eq-rule operation; it falls into evaluated fields / generated operations,
  i.e. back into imperative Python. The model is strongest at *per-node selection
  and placement*, weakest at *graph-recursive aggregation*.
- **Demand-driven, incremental, early-cutoff** (`F5/F6/F10`): YIDL's pipeline is a
  *whole-plan*, ordered, once-through evaluation with no node-granular
  invalidation and no fixpoint. A build tool's reason-to-exist (incrementality)
  is exactly what this resolution model omits. You would be borrowing the
  *authoring/decoupling* shape, not the *evaluation engine*.
- **Provenance** (`F21`): silent non-firing rules are antithetical to "explain why
  this action ran."

---

## 7. Verdict — transferable vs idiosyncratic

**Transferable (the genuinely good ideas, and they line up with R1–R15 / F-list):**
1. **Producer→consumer decoupling as data**: rules name *resources/providers*,
   never the producing code (`§26.1.19`) ⇒ `R4/R8/F11`. Typed-fact-from-day-one.
2. **Open-set extension by phase-splice** (`extend production … phase … after …`)
   with the consumer enumerating, never importing, contributors ⇒ `R7/F15/F16/F17`.
   The `local_store.yidl` "one new file, zero core edits" result is the template
   to aim at.
3. **The semantic firewall**: a selection/composition layer that owns *meaning*
   over a dumb, deterministic, closed-plan executor that owns *mechanics* and
   "never learns the client's concepts" (`AstichiAssemblerConcept.md §2`) ⇒
   `R1/R6` (decoupled engine, quarantined kernel) and `F18` (closed, side-effect-free
   evaluation).
4. **Closed plan, callback-free after expansion** ⇒ `F18/F19` and partitionable
   analysis state (the antidote to razel's ambient thread-locals, `R3`).
5. **Definition-time confluence checking** (equal-score-overlap rejection) instead
   of runtime tie-breaks — a cheap static guarantee worth stealing wherever the
   match operator stays decidable.
6. **Golden-source / pure-data testing** of derived structure ⇒ `R12/F-(b)`.

**Idiosyncratic (over-fit to AOT codegen; do not transplant):**
1. **Eq-only matching + evaluated-field escape hatch.** Fine for a finite field-kind
   taxonomy; far too weak for a build tool's config/constraint/transitive logic,
   and the escape hatch quietly re-imperativizes it.
2. **Ordered/stratified, no-fixpoint, whole-plan evaluation.** This is the deepest
   over-fit: it omits incrementality, early-cutoff, and demand-driven resolution
   (`F5/F6/F10`) — the *defining* properties of a build tool. YIDL runs once per
   class; a build runs constantly and must reuse. Borrow the authoring model,
   **not** the evaluation engine — keep razel's graph/Engine (`R5`) underneath.
3. **No provenance for non-firing rules** ⇒ violates `F21`; unacceptable as-is for
   a build tool.
4. **Flat (non-hierarchical) DDS** (`MetaSquaredLog.md:436`) and the M2 comprehension
   wall — accept the authoring decomposition, but the build graph is inherently
   hierarchical/recursive, which the DDS does not model.
5. **Astichi specifically** is a Python-AST stitcher; irrelevant to razel except as
   *proof that a strict client/executor firewall with a closed plan is buildable
   and testable* — and as a warning (it needed a Rust hot path).

**Bottom line for razel:** YIDL/Astichi is strong, transferable inspiration for
the **analysis/authoring seam** — facts-as-providers, matcher-style open-set
selection, phase-splice extension, a closed plan over a semantics-blind executor,
and definition-time disambiguation. It is *not* a model for the **evaluation
engine** — its once-through, fixpoint-free, provenance-poor resolution is the
opposite of the incremental, demand-driven, explainable graph a build tool is
*for*. Take the slicing; keep razel's demand-driven graph (`R5`) and provenance
(`F21`) underneath it.

---

## 10-line abstract (model + leverage)

1. YIDL describes a domain's architecture as **typed facts** (records in
   identity-bearing collections; families = tagged unions).
2. **Matchers** are Eq-only, AND-combined, highest-score-first rule dispatchers
   that select a *resource/provider* from a tuple of facts — never naming the producer.
3. **Compositional rules** derive new facts (**productions**) and emit code
   (**composable productions / assemblies**) by expanding facts into
   **contributions** placed at named holes of an **open pipeline**.
4. Resolution is **ordered, stratified, confluent-by-construction (no
   fixpoint / no Rete)**; ambiguity is rejected at *definition time*.
5. **Astichi** is the semantics-blind executor: it stitches AST scopes from a
   **closed, callback-free plan** and never learns the client's concepts.
6. This is **M2** (program ← generator ← description model); **M3** (the language
   defining itself) is named as a coherence test but deliberately unbuilt.
7. **The leverage = (b) behavior-from-matching + (a) facts as substrate:** a new
   feature is one file that *splices a phase* into an existing pipeline with
   **zero edits to the consumer** (proven by `lifecycle_local_store.yidl`).
8. It buys finer test units (facts/matchers/productions test in isolation;
   assemblies test as golden source) and partitionable, closed analysis state.
9. Failure modes: Eq-only ceiling + evaluated-field re-imperativization, **silent
   non-firing rules (no provenance)**, cross-module phase-ordering nondeterminism,
   **no recursive/transitive derivation**, comprehension wall, perf (needed Rust).
10. For razel: **steal the authoring/decoupling seam** (facts-as-providers,
    open-set phase-splice, closed plan over a blind executor, static
    disambiguation); **reject the evaluation engine** — it lacks incrementality,
    early-cutoff, demand-driven resolution, and explainability, which are the
    very reasons a build tool exists.
