# Razel Architecture — Decision Document (A1 / A2 / A3 + the recommended path)

Synthesis of the three candidate architectures (`draft-arch-{1,2,3}.md`) and their
adversarial critiques (`critique-arch-{1,2,3}.md`), scored against `ArchFundamentals.md`
(F1–F23), `ArchBazelConstraints.md` (C1–C9), `ArchPatterns.md` (the dial + scars
S1–S12), `ArchModel.md` (R1–R15), `ArchitectSkillRules.md` (the design lens), and
`YIDLDigest.md` (the facts/matchers/compositional-rules M2 model). Code claims are
grounded against the crates at 62 commits (citations re-verified, not pattern-matched).

This document keeps **all three options visible** (the brief wants options), names where
each clearly wins and clearly loses, and then gives a **decisive recommended path** —
including a surgical hybrid — sequenced against the migration reality.

---

## 1. Framing — the dial, plus the M2 inversion as a third axis

Two mature build tools and razel-as-built collapse to one small model (`ArchModel.md §1`),
so the architectures **do not differ in *what* they do — they differ in *how much typed
structure and static machinery they layer above the already-owned Starlark
interpreter***. That is the **thin ◐ ↔ rich ●** dial of `ArchPatterns.md` Part C. **A1**
sits at the leftmost detent (thin-dynamic, Bazel-grain: keep the imperative `rule()`
body, give the provider channel a *thin* typed identity, take exactly one forced notch
right at F5/7/10). **A2** sits at the right column (typed-rich, Pants-informed,
single-language: first-class typed providers as the only inter-target contract, the
demand-driven Engine lifted onto the live path as *the* execution+analysis model). A third
axis — orthogonal to the dial — is the **M2 inversion (A3)**: instead of *recording* the
build as a side-effect of imperative rule impls, *derive* it by matching and composing
typed **facts** (YIDL's slicing). A3 is not "more rich on the same dial"; it re-slices the
*computation* below the Starlark surface into facts → matchers → compositional rules,
buying the finest-grained modularity and testability of the three at the cost of a second
declarative layer whose payoff is keyed to orthogonal derivation (F17), a *non-★*
fundamental. Critically, **all three share an identical load-bearing prefix** — kill the 8
thread-locals into a passed plural Session (verified: 7 statics in `rules.rs` + `CTX` in
`lib.rs`), delete the dead second loader, manifest the registry — so the real decision is
only *where to stop on the dial* and *whether to invert one sub-domain into facts*.

---

## 2. Scoring matrix

Rows are the ★ load-bearing fundamentals (grouped as the brief specifies) + the
architecture-quality lens (a/b/c) + Bazel-constraint fidelity + the characteristic
dead-end risk. Cells are concise judgements grounded in the drafts and critiques — **not
invented**; where a critique overturned a draft's self-claim, the cell reflects the
critique.

| Axis | **A1 — Thin Dialect ◐** | **A2 — Typed Provider Graph ●** | **A3 — M2 Facts/Matchers/Rules** |
|---|---|---|---|
| **F11 producer↔consumer decoupling ★** | Cleared in *invariant* (lookup by label, never producer) but **thinly typed**: privileged hard-coded `hdrs`/`cflags` struct fields + escape-`BTreeMap` for user `provider()` → **two-tier provider model** (S3 seed). Cheapest, weakest. | **Strongest & the one unambiguous win over A1**: typed `ProviderType`/`ProviderKey` from day one, `dep[CcInfo]`; the expensive-to-retrofit contract (S3/R4) committed while there are ~0 consumers. | Strong (typed providers, derived) but **not distinctively better than A2** — A3 adds derivation *machinery around* the same provider contract. |
| **F5/F7/F10 graph (incrementality, parallel, demand-driven) ★** | One *forced* notch right: route the live build through the existing `Engine`. Buys F5/F6/F10 cheaply (Engine has early-cutoff). **F7 under-budgeted**: the Engine is `!Sync` (`RefCell`/`Cell`, verified `lib.rs:33-38`); "commodity worklist" hides a real rewrite. | Same Engine notch + commits to *lifting analysis into the graph* and a typed/`Send+Sync` parallel engine — **the single largest, fixed-cost build in any draft**, spent ahead of any workload. Step 5 also **mis-stated** the daemon as Engine-backed (it calls straight `execute`, verified `rpc.rs:225`). Analysis-level F6 asserted "free" but needs a `SkyValue.equals`-class digest discipline A2 never specifies. | **Worst satisfaction of a ★ here**: A3 inserts a new derivation pass *above* the Engine that is re-run wholesale per `analyze_*`, buying **no analysis-incrementality A1/A2 lack** while adding recompute cost. F6 at the analysis layer is absent (a changed `define` re-derives the whole fold). |
| **F12 configuration / variation (dial-load-bearing)** | ◑ `select`+`config_setting` matcher — real & correct for the common case, but **bakes single-config identity into a label-keyed `BTreeMap`**; the day transitions / `(Target×Config)` arrive, the key-type migration (`Label`→`(Label,Config)`) is among the most expensive undos (R4). Its plural Session buys concurrent *packages*, not N configs of one target. The deepest A1 risk. | Dial label says ● "config-as-graph-axis + transitions"; **body delivers A1's position** — decides the matcher model, *defers* the cross-product behind a single-config default. The ● is aspirational; transition *semantics* are unspecified. **On F12, delivered A2 = A1.** | ◑→● claimed, but **internally inconsistent**: §2a fact identities are `(label)`/`(label,attr)` with **no `Config` in the tuple**, while §4 hand-waves `(label,config)` keying. Has not escaped the same S6 trap it criticizes A1 for; relabels the select-stub replacement a "matcher." |
| **F16 open-set composition / extension ★** | ◑ registry + open-set dispatch via const-array manifest. New language = one file + one row; dispatch by target-kind/provider-contract, never `if rule=="cc_library"`. Right altitude for ~12 builtins + 4 langs. | ● union-membership dispatch (`ProviderType`/goal membership) over the ◑ manifest — a *widening* of the proven ruleset seam, not a new subsystem. `test` iterates "targets exposing `TestInfo`" without being edited. | ● open-set, manifest — the **headline win**, finest grain (`lifecycle_local_store.yidl` template: new language = facts+matchers+rules in one file). But tension: "small closed `Fact` enum" (S10 guard) pulls against "new lang wants new `RuleKind`/`Resource`/`Phase` variants." |
| **F18/F15 constrained eval + engine-closed extensibility ★** | ◐ full-left, the core bet — **satisfied for free** (Starlark already owned, zero build-tool imports, R1). Maximal dynamism; weakest static checks; lowest comprehension cost. | ● "typed value graph above Starlark" — keeps Starlark's dynamism *and* adds the typed contract; additive over existing Nouns. Best-of-both is A2's reason to exist. | ◑ Starlark above + ◑ facts/matchers below — **two declarative layers = the M2 comprehension wall** (genuinely two levels up; `lifecycle_managed.yidl` 1,545 lines is the warning). A3's biggest soft cost. |
| **F19 analysis state (no dial — invariant)** | ✅ Passed plural `Session` via `eval.extra`; `Session.results` *is* the provider store (one move, two payoffs). Discipline-enforced ("remember to thread it"). | ✅ Same Session; `Session.providers` *is* the typed store *is* the analysis-node value — the unification is A2's identity (and where over-build concentrates). | ✅ **Structurally strongest form**: you cannot assert a fact without a store handle, so state-as-value is *forced*, not disciplined. |
| **(a) Modularity / smallest-edit** | Strong — kills the `rules.rs` 1658-LOC gravity well into words/nouns/rulesets + manifest; "new feature = new file + one row." Honest cost: a wide-shallow tree (~20 files), near the upper bound for the member count; forbids a `dialect/` super-layer. | Strong — same dissolution + open-set goals untouched by new languages. Slightly more ceremony (a new provider type must be named by consumers — the F11/R4 tax, paid on purpose). | **Strongest at the rule-authoring grain** — the 64-line `native_cc_library` god-body splits into ~5 testable units; **no central recording funnel to reform** (beats A1 here). But a tie with A1 on dissolving `record_target`. |
| **(b) Testability** | Strong — Words become `fn(args,&Session)`, pure-testable; Lexicon helpers finally unit-tested; assert on captured `AnalyzedAction.argv`. Limit: doesn't by itself remove cc/rust/py end-to-end toolchain-gating. | **Best of the three** — typed providers make the inter-unit contract assertable as data; existing incremental==scratch equivalence test extends to analysis nodes. New burden: concurrency tests for the parallel engine. | **Finest grain** — matcher = pure `fact_tuple→Resource`; derivation = pure `fact_view→fact_delta`; the confluence check is itself a unit test. Kills the ~28 `exists(){return}` env-gated skips (S11). |
| **(c) Calibrated extensibility** | **Deliberately minimal — the gamble.** Spends relief valves at exactly two seams (Session, manifest) + one forced notch. Right *if* the future is more languages + bigger graphs; under-built *if* config-transitions or overlays arrive. The default AI failure (too-rigid), consciously near the edge. | **Most exposed (over-spend).** Each valve sits at a *provably-moving, expensive-to-retrofit* seam (new lang/config/query) — but staples the **fixed-engineering-cost** parallel analysis graph to the cheap retrofit-now argument that only justifies the typed contract + the config *decision*. | **Over-calibrated** — most indirection of the three; a derivation engine over ~12 builtins. Pays off **only if F17 (orthogonal derivation) is a named goal**; otherwise A1 gets ~80% of (a)/(b) for ~40% of the conceptual cost (A3's own concession). |
| **Bazel-constraint fidelity (C1–C9)** | No outright violation; **strains C7 hardest** — structurally defers the config axis into a contract (label-keyed map) that must be migrated to admit transitions. C4 `aspect()`/transitions "NOT built." | No violation; **strains C7 by overpromise** (● transition label, deferred construction, semantics unspecified). Takes the most net-new C4 work (`provider()` identity, `attr.*` schema-driven `ctx`; today `rule()` does `let _ = attrs;`, verified `:587`). | No semantic violation; **strain C4/C5 unproven**: lowering a live `ctx.actions.run(...)` (heap `Args`/`depset`/`File` mid-impl) into an immutable identity-bearing fact is asserted, only the *native shim* path is shown; the *evaluated-`.bzl`* path (the one that matters for `@rules_cc`) is waved through. |
| **Characteristic dead-end risk** | **TOO RIGID** (S6 primary, conditionally fatal: fatal iff multi-config/transitions arrive — then a key-type contract migration; S3 secondary). S9 avoided, S5/S7 defeated cleanly. | **TOO ABSTRACT / over-build** (S8 the most-courted; guard is *discipline, not architecture* — "no five-phase builder" is a promise). S10 well-handled (`ProviderKey` identity). S9 deliberately avoided. | **TOO ABSTRACT, sharpest** (S8 = its own headline; over-architects an already-finite shape). + a deliberate slice of S9. Distinct value concentrates in `fold_over_deps`, which the critique shows is **the YIDL evaluated-field escape hatch in a Rust costume**. |

---

## 3. Honest trade-offs — where each clearly wins and clearly loses

**A1 (Thin Dialect ◐)**
- **Clearly wins:** lowest cost and fastest path to green; F18/F15 for free; F19/(a)/(b)
  achieved *cheapest*; S5 (god-module) and S7 (split-brain loader) defeated, S9
  (startup-rigidity) avoided. It is the **correct default and the honest baseline** —
  its entire shared prefix is no-regret work every architecture needs.
- **Clearly loses:** F12/C7 — it does not just defer transitions, it *bakes single-config
  identity into the fact-store key type*, so the eventual pivot is a contract migration,
  not an additive change; F17 — structurally absent, no seam for overlays; F11 typing —
  two-tier (privileged cc fields vs escape-map user providers). It **under-budgets F7**:
  the `!Sync` Engine rewrite is called "commodity" but is the same large build A2 names.

**A2 (Typed Provider Graph ●)**
- **Clearly wins:** F11 — typed providers with explicit `ProviderKey` identity, from day
  one; this is the one expensive-to-retrofit contract (S3/R4) that A2 commits and A1 only
  half-commits. S9-avoidance and S10-handling are A2 at its best.
- **Clearly loses:** F7 / the parallel typed analysis engine — the largest *fixed-cost*
  build in any draft, whose cost does **not** shrink with "~0 consumers" (the trap A2's
  thesis claims to dodge), spent ahead of any workload. It **under-prices its biggest
  migration step on a false code claim** (daemon calls straight `execute`, verified, not
  the Engine). F21/provenance unaddressed despite adding the most indirection.
- **The critique's verdict on collapse:** A2 is genuinely distinct from A3 (it types the
  *contract*; A3 re-slices the *computation*) and is the **safer of the two right-column
  bets**. But A2 is only **weakly distinct from A1**: strip the shared prefix and A2's
  *guaranteed near-term* delta is **typed providers + a promise** — F12-cross-product and
  parallel analysis are deferred to the *same horizon A1 defers them to*. A2 ⊋ A1 reduces
  to "commit the typed contract now and intend to lift analysis later." (A2 concedes this
  in its own §6.)

**A3 (M2 Facts / Matchers / Compositional Rules)**
- **Clearly wins:** (a) and (b) at the **finest grain** of the three; and **F17 is the
  one fundamental A3 alone expresses natively** (derive lint/IDE/coverage/codegen as new
  rules over existing facts, zero edits to the rules that built the graph).
- **Clearly loses:** F5/F6/F10 (★) — the derivation layer buys *no* analysis-incrementality
  A1/A2 lack while adding a new recompute pass; F21 is *fought* (saved only by a mandatory
  derivation log), not satisfied cheaply.
- **The fatal-flaw findings (fold in, don't soften):**
  1. **Distinctness collapses.** By A3's own §1/§8, **A3 = A1 + a derivation layer**;
     steps 1–2 are "identical to A1's keystone move." Facts and matchers are *already
     spent* by A1 and A2 at the one place razel needs them (`select`/`config_setting`).
     The sole true differentiator is `fold_over_deps`-style transitive derivation.
  2. **That differentiator is the escape hatch relabeled.** The loop it replaces is six
     readable lines (verified `rules.rs:914-920`); `YIDLDigest §6` states plainly that
     transitive closure "cannot be expressed as facts→rules." A3 renames the YIDL
     evaluated-field a "derivation primitive" and asserts it stays declarative — and §8's
     own go/no-go kill-gate exists *because this is expected to fail*.
  3. **Its value is keyed to a non-★ fundamental (F17).** "If razel stays a minimal core,
     A3 is over-built (S8)" — A3's own words.
  - Net: A3 is **mechanically viable but a calibration dead-end** for the tool as a whole
    — *unless* F17 is committed roadmap. Today it is not.

**The structural truth the three critiques converge on:** these are **not three
architectures — they are one architecture (the Session-based Dialect) with a dial at the
end**. A1 is the leftmost detent. A2 = A1 + the typed provider contract (+ a deferred
ambition). A3 = A1 + a derivation layer (whose one novel piece is unproven). The shared
prefix is mandatory and identical; the divergence is small, late, and reversible.

---

## 4. Recommendation — A1 spine + the typed-provider commitment, with a *gated* surgical hybrid

The options stay on the table. The recommended path is decisive:

### 4.1 Build the A1 spine, and within it take A2's one unambiguous win.

Adopt **A1 as the spine** — it is the no-regret prefix every option needs and the right
calibration for a 62-commit, single-config, multi-language tool with `select` stubbed and
no consumer asking for transitions. But **fold in the single A2 commitment that is
cheap-now / catastrophic-later: typed providers with explicit `ProviderKey` identity,
from day one.** Every critique agrees on this independently — A1's two-tier provider model
is the S3 seed, and the typed contract "should be adopted regardless of which architecture
wins" (critique-2). This is the one place A1-as-drafted is too thin, and the fix costs one
`nouns/provider.rs` + typing `Session.providers`, paid against ~0 consumers.

This collapses the A1/A2 distinction honestly: **the *recommended* architecture is A1's
thinness everywhere except F11, where it takes A2's typing.** A2's *remaining* distinct
spend — the parallel, config-keyed, typed analysis Engine — is correctly **demand-paced
and held behind a real workload** (it is fixed engineering cost, not retrofit cost, so
"do it early while consumers are few" does not apply to it).

### 4.2 Consider the hybrid surgically — and gate it hard.

The brief asks whether the M2/facts-matchers approach should be applied *surgically* to a
genuinely-hard-to-modularize sub-domain. The honest answer, from the analyses:

- **Where the M2 prize (isolation + testability + confluence-by-construction) actually
  pays without the too-abstract risk: bounded, Eq-decidable dispatch.** That is exactly
  **`select`/`config_setting` matching** and **toolchain/platform resolution** — both are
  static, finite, Eq-AND selection, which is the *one* place YIDL's matcher is a genuinely
  good fit, and **A1 and A2 already adopt it there** (A1 §1.F12, A2 §2.F12). So the
  surgical matcher is *already in the spine* — adopt the YIDL definition-time
  equal-score-overlap rejection (a cheap static F1 guarantee) for these tables. This is
  the part of A3 worth stealing, and it costs almost nothing because it is decidable.
- **Where the M2 approach does *not* pay: transitive provider propagation.** This is the
  sub-domain the hybrid would most want (it is the combinatorial hard case), but it is
  precisely where `fold_over_deps` is the escape hatch relabeled and the six-line loop is
  already clear. **Do not invert this into a derivation engine.** Keep it as a typed
  helper over the typed provider store (the Lexicon `fold_depset` move A1 already plans).
- **Toolchain resolution as a matcher: yes, surgically.** Replacing the `CXX`/`AR`
  hard-coded consts (`rules.rs:844`) with a typed-matcher selection of a `Toolchain`
  resource (F13) is a real isolation+testability win on an Eq-decidable sub-domain — this
  is the A3 idea most worth lifting, and it lifts *without* the second declarative layer
  (it is one matcher table, not a fact store + derivation pass).

**The gate:** the only place to even consider the *full* A3 derivation layer is if **F17
(orthogonal derivation — lint/IDE/coverage/codegen as first-class derivations over the
build graph) becomes committed roadmap.** Until then, the matcher-where-decidable +
typed-helper-for-transitive-closure hybrid captures the M2 prize on the sub-domains where
it pays, and the full inversion stays a documented option, not a build.

### 4.3 Sequence against the migration reality (the order matters)

The first three steps are **identical across all three drafts** — do them regardless:

0. **Delete the dead second loader first (R14/S7).** `lib.rs` `TargetDecl`/`load_build`/
   `CTX` + `razel_analysis::analyze` + `Depset<T>` are reachable only from their own tests
   (verified: `load_build` has no non-test caller outside the dead pipeline). Deleting
   *after* refactoring would spawn a *third* depset/target representation. One PR, green.
1. **Lexicon extraction + unit tests** (`canon_label`, `glob_match`, `fold_depset`,
   `shquote`) — lowest risk, highest test ROI; they have zero direct tests today.
2. **The Session keystone (kill the 8 thread-locals).** Introduce `Analysis` via
   `eval.extra` with interior-mutability fields (the composable choice — `extra_mut`'s
   `&mut` does not survive nested eval); migrate `RESULTS`→`results` first (it is also the
   provider store). Delete the divergent hand-resets; unify the drifted globals-builders.
3. **Words/Nouns lift + const-array manifest;** push the still-inline cc/skylib/autoconfig
   bodies out to `rulesets/` to finish the half-built seam. `rules.rs` → thin `assembler.rs`
   + `analysis/orchestrate.rs`. The god-module is gone.

Then the recommended-path-specific steps:

4. **Typed providers (the folded-in A2 win).** `nouns/provider.rs` + `ProviderKey`
   identity; migrate the implicit `DefaultInfo`+`hdrs`+`cflags` bundle and the `dep[Info]`
   reconstruction to typed `ProviderInstance`s; make `rule()` consume its `attrs` schema
   (today `let _ = attrs;`). This is the one place we take the ● column.
5. **Engine on the live path (the forced F5/7/10 notch).** Route *both* the CLI and the
   daemon (`rpc.rs:225`, which today bypasses the Engine — budget for two callers, not
   one) through the `Engine`/`IncrementalBuilder`; delete the straight `execute` loop as
   the product path. **Parallel execution (F7) is a separate, correctly-budgeted step**:
   the Engine is `!Sync` today, so this is a real `Send+Sync` + worklist rewrite, not a
   "commodity" add-on — sequence it after the unification lands and only when a graph is
   big enough to need it.
6. **Surgical matchers (the hybrid, where decidable).** `select`/`config_setting` and
   toolchain resolution as typed matcher tables with definition-time confluence checking;
   decide the config *model* now (R9) even while deferring the `(Target×Config)`
   cross-product machinery — **but front-load the decision of whether the provider/results
   key is `Label` or `(Label,Config)`** (see open question Q1), because that is the one
   contract migration A1 most risks.
7. **Held behind a workload:** parallel *analysis* + `(Target×Config)` keying (A2's
   remaining spend) and — only if F17 ships — the A3 derivation layer (gated on a cc
   transitive-propagation pilot that must beat the six-line loop, or it reverts).

---

## 5. Open questions to resolve before committing

1. **Provider/results key shape — `Label` vs `(Label, Config)` — decide now (Q-critical).**
   This is the single contract that A1 most risks baking wrong (critique-1's sharpest
   finding) and that A2 commits and A3 fumbles. Even if the config cross-product is
   deferred, the *key type* must be chosen up front because migrating it later is among
   the most expensive undos (R4). Recommendation leans toward provisioning the key for
   `(Label, Config)` while running single-config — but this needs an explicit decision.
2. **Is multi-config / configuration transitions on the roadmap, or genuinely "someday"?**
   This single fact decides A1-vs-A2 on F12: if multi-config is coming, A2's
   F12-decided/F11-typed core is vindicated and A1's deferral is the expensive retrofit;
   if not, A1's thinness is pure leverage.
3. **Is F17 (orthogonal derivation: lint/IDE/coverage/codegen as derivations) a named,
   near-term goal?** This is the *entire* condition under which the full A3 layer earns its
   keep. Today nothing in `ArchModel.md` or the north star elevates it. Resolve before
   spending any derivation-engine machinery.
4. **Parallel analysis vs parallel execution — which does razel actually need, and when?**
   A2 ties F7 to lifting analysis into the graph; A1 needs only execution-parallelism. The
   `!Sync` Engine rewrite cost is real and fixed; budget it against the actual workload,
   not the architecture's ambition.
5. **Analysis-level early-cutoff (F6) digest discipline.** If analysis is ever lifted into
   the Engine, a recomputed `ProviderSet` must hash stably (canonical serialization,
   stable `ProviderKey` ordering) — the `SkyValue.equals` tax A2 asserts as "free." Decide
   whether to pay it before promising analysis-incrementality.
6. **`ctx.actions.run` → IR lowering for *evaluated* `.bzl` (not just native shims).**
   Independent of A3, the evaluated-`@rules_cc` path (live heap `Args`/`depset`/`File` mid-impl
   → IR) is the unproven seam; confirm it works before relying on real rulesets.

---

## Executive summary (~15 lines)

1. The three candidates are **one architecture (the Session-based Dialect) with a dial at the end**; all share an identical, mandatory, no-regret prefix.
2. **A1 (thin ◐)** is the correct default and honest baseline — cheapest, fastest to green, F18/F15 free, S5/S7 defeated, S9 avoided.
3. A1's real weaknesses: it **bakes single-config identity into a label-keyed contract** (F12/C7, conditionally fatal), keeps a **two-tier provider model** (S3 seed), and **under-budgets F7** (the `!Sync` Engine is a real rewrite, verified, not "commodity").
4. **A2 (typed-rich ●)** has exactly one unambiguous win — **typed providers with `ProviderKey` identity from day one** (the expensive-to-retrofit S3/R4 contract).
5. But A2 is only **weakly distinct from A1**: its near-term delta is "typed providers + a promise"; the parallel config-keyed analysis Engine is **fixed-cost over-build (S8)**, and it **mis-states the daemon path** (it calls straight `execute`, verified).
6. **A3 (M2 facts/matchers)** wins (a)/(b) at the finest grain and **uniquely expresses F17** — but **collapses into "A1 + a derivation layer"**, its one differentiator (`fold_over_deps`) is the **YIDL escape hatch relabeled**, and its value is keyed to F17, a **non-★** fundamental razel has not committed.
7. **Recommendation:** build the **A1 spine**, fold in **A2's typed-provider commitment** (the one cheap-now/catastrophic-later move), and apply the **M2/matcher idea surgically** only where it is Eq-decidable — `select`/`config_setting` and **toolchain resolution** — with definition-time confluence checking, which A1/A2 already half-adopt.
8. **Do not** invert transitive provider propagation into a derivation engine (the six-line loop is already clear; `fold_over_deps` re-imperativizes); keep it a typed helper.
9. **Sequence:** (0) delete the dead loader → (1) Lexicon → (2) Session keystone (kill 8 thread-locals) → (3) Words/Nouns + manifest → (4) typed providers → (5) Engine on the live path (budget two callers + a separate `Send+Sync` step for F7) → (6) surgical matchers + decide the config *model* → (7) hold parallel-analysis and the full A3 layer behind a real workload / committed F17.
10. **Gate the full A3 inversion** behind committed F17 roadmap **and** a cc transitive-propagation pilot that must beat the existing loop, or revert.
11. **Decide before committing:** the provider key shape (`Label` vs `(Label,Config)`) — front-load this even while single-config; it is the one contract A1 most risks baking wrong.
12. Resolve the three roadmap questions — multi-config, F17, parallel-analysis-vs-execution — because each flips a specific A1↔A2 trade.
13. Net: **A1 spine + A2's typed providers + surgical matchers**, sequenced after the thread-local→Session and dead-loader fixes — decisive, reversible, and honest about the two deferred frontiers (config-as-axis, orthogonal derivation).

---

## 6. Addendum — F17/F24 are committed: razel is a derivation server (the recommendation shifts)

Open-question **Q3 is resolved: razel exists *for* grip-lab.** Its differentiator over
Bazel is not compatible C++ builds (that's **table-stakes input fidelity** — the C1–C9
constraints); it is **the build graph as a live, distributed, agent-facing derivation
substrate.** So **F17 (orthogonal derivation) is now ★ and committed**, and a new
fundamental **F24 (distribution + multi-instantiation)** is added (`ArchFundamentals.md`).
This changes the §4 recommendation in four concrete ways — and *relieves* one of A1's
biggest risks.

1. **The M2/facts derivation layer moves from "gated, step 7" to a first-class component.**
   The §4.2 gate ("only if F17 ships") is now open. But the **honest scope is unchanged**:
   facts/matchers are for the **derivation + distribution + Eq-decidable-dispatch** layers;
   the **core transitive build accumulation stays a typed helper** (the `fold_over_deps`
   escape-hatch finding stands). The inversion earns its keep on razel's *product surface*
   (the views MCPs/UI consume), not on the build core.

2. **Typed providers → typed *serializable* providers/facts.** A2's one win is
   re-justified for a second reason: not just static safety, but **wire + merge across the
   mesh** (F24). The substrate already exists — the **taut IR → CBOR wire** (`razel-wire`)
   expands from "daemon RPC" to "the distributed fact substrate." Provider/fact types
   become taut-defined and CBOR-serialized; derived facts flow over iroh and compose on
   peer/aggregator nodes. *This is a strong, low-new-infra alignment.*

3. **Cross-platform reslices the config problem — and relieves A1's deepest risk.** The
   mission does cross-platform as **N graph *instances* (one per platform node/VM)** +
   **cross-instance *derivation*** (F24 + F17), **not** as Bazel-style `(Target×Config)`
   transitions *within one graph* (F12). Consequence: each instance is effectively
   single-config, so the **provider/results key can stay `Label`** (the §5 Q1 fork leans
   *toward* `Label`, not `(Label,Config)`), and cross-platform compile-command/index views
   are *composed by deriving across instances over the mesh*. This **demotes A2's
   config-as-axis spend** (its weakest, most speculative investment) and **largely defuses
   A1's "conditionally fatal" S6/C7 risk.** (Caveat: in-graph exec/host transitions — a
   tool built for the exec platform inside one build — still need a *bounded* mechanism;
   they don't reintroduce the full cross-product.)

4. **Strategic anchor for the architecture.** Optimize for **F17 + F24 as the value**, with
   C1–C9 as table-stakes input fidelity. This aligns with the clean-slate thought
   experiment: **the IR / typed-fact store is the canonical product**; BUILD/`.bzl` is one
   *input* front-end and the UI/MCP query API is the *output* front-end — both project
   to/from the serializable fact substrate.

### Revised recommendation (supersedes §4 where they differ)
**A1 spine + typed *serializable* providers (taut/CBOR) + a facts-based F17 derivation
layer distributable over iroh + surgical matchers (select/toolchain).** The core
transitive build stays a typed helper. Cross-platform via multi-instance + cross-instance
derivation; provider key leans `Label`. A2's parallel `(Target×Config)` analysis engine is
*demoted further* (the mission routes around it). The M2 inversion is adopted **for the
derivation/distribution surface**, not the build engine.

### Revised sequence (tail re-prioritized; prefix unchanged and *more* justified)
`0` delete dead loader → `1` Lexicon → `2` **Session keystone** (now mission-critical:
F24 needs the graph as a value, instantiable N× and serializable) → `3` Words/Nouns +
manifest → `4` typed **serializable** providers (taut-define them; the fact data) → `5`
Engine on the live path → `6` **the F17 derivation layer over the fact substrate +
distribution over iroh (now committed, not gated)** + surgical matchers (select/toolchain)
→ `7` parallel *execution* (F7, the real `!Sync` rewrite) as workload demands. The
`(Target×Config)` in-graph cross-product is **deferred indefinitely** unless an in-graph
exec/host case forces a bounded version.

### Open questions, updated
- **Q3 → resolved (F17/F24 committed).**
- **Q1 (provider key) → leans `Label`** per-instance (cross-platform is multi-instance,
  not in-graph config) — but the key/fact types must now be **serializable** (taut).
- **Q2 (multi-config) → largely answered**: cross-platform = multi-instance (F24), not
  in-graph `(Target×Config)`; only exec/host transitions might need a bounded mechanism.
- New: **what is the canonical serialized fact/provider schema** (taut IR), and **what is
  the MCP/UI query + subscription surface** over derived facts? These are now design
  drivers, not afterthoughts.
