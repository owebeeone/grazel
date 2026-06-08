# Adversarial critique — Architecture A2 (Typed Provider Graph, ● typed-rich, single-language)

*Reviewer stance: try hard to break A2. Read against ArchFundamentals (F1–F23), ArchBazelConstraints
(C1–C9), ArchPatterns Part B scars (S1–S12) and the Part C dial, and grounded in the crates
(`razel-engine/src/lib.rs`, `razel-build/src/{lib,incremental}.rs`, `razel-loading/src/rules.rs`,
`razel-daemon/src/rpc.rs`, `razel-ir/src/lib.rs`). Citations are to draft-arch-2.md unless noted.*

---

## Steelman (two sentences)

A2 spends razel's *next* relief valve above the already-owned Starlark interpreter on one unifying
move: the per-analysis `Session.providers` store **is** the typed inter-target contract **is** the
value of analysis nodes in a single demand-driven Engine — so decoupling (F11), state (F19), and the
graph (F5/F7/F10) collapse into three views of one typed store, gaining Pants's two best ideas
(composition-by-typed-value, open-set union dispatch) while paying neither of Pants's taxes (no second
language; no startup rigidity — Starlark stays dynamic). The bet is the retrofit-cost asymmetry: a
typed provider contract (S3) and a config-as-graph-axis (S6) are the most expensive things to bolt on
late, and with ~0 consumers today the cost of committing them is near-minimal — so commit now.

That steelman is coherent and the code citations are, with one exception (see §1.b), accurate. Now the
attack.

---

## 1. Which Fundamental does A2 satisfy poorly or expensively (esp. ★)

A2's headline is that it satisfies F4/F5/F6/F7/F10/F11/F12/F16/F19 "maximally by construction" (closing
line). The honest reading is that it satisfies several of them **expensively and on credit**, and one
★ **worse than A1 in the near term**.

### (a) F7 (parallel execution) ★ — satisfied only after the single largest rewrite in any of the three drafts, and the draft admits it isn't cheap

A2's own §2 (F5/F7/F10) concedes the engine today is "`String`-keyed, single-threaded (`RefCell`/`Cell`,
not `Send`/`Sync`), and only models opaque `Digest` values" — verified at `razel-engine/src/lib.rs:15-17`
(`type Key = String; type ComputeFn = Box<dyn Fn(&[Digest]) -> Digest>`) and `:33-38` (`nodes:
RefCell<…>`, `revision: Cell<…>`). So the path to F7 is: (1) a typed `Key` enum, (2) `Send`/`Sync`
nodes, (3) a tokio/rayon driver, **and** (4) lifting *analysis* into that same graph as
`ConfiguredTargetNode`s. The draft itself calls this "the single largest build in A2 and the heart of
its over-build risk" (§2, F5/F7/F10).

The unsparing point: **F7 is the only fundamental A2 cannot get cheaply, and A2's whole thesis is "do
the expensive structural things now because there are ~0 consumers."** But the parallel typed Engine is
expensive in *engineering*, not in *migration* — its cost does not shrink because there are few
consumers; it is a fixed cost paid against a benefit (parallel analysis) that nobody can exercise until
there is a graph big enough to be slow. That is precisely the `ArchitectSkillRules` "too-abstract" dead
end: generality (a parallel analysis graph) spent ahead of any workload that needs it. A1 reaches the
*same* F7 outcome by a "worklist over the ready-frontier" scheduler over the existing Engine's edges
(draft-arch-1.md §1.F5/7/10) — commodity, and it does **not** require typing the Key or lifting analysis
in first. A2 has tied F7 to the config axis and the analysis-graph lift, making the cheapest win
(parallel *execution*) hostage to the most speculative build (parallel *analysis* across configs).

### (b) F5/F10 (incrementality, demand-driven) ★ — A2 rests its migration on a factual overstatement of where the Engine already runs

A2 §2 (F5/F7/F10) and migration step 5 both assert the Engine "already drives `IncrementalBuilder`/daemon"
and that step 5 is "Route `build_target`→`execute` through `razel_engine::Engine` (it already drives
`IncrementalBuilder`/daemon)." **This is wrong on the daemon.** Grounded: `razel-daemon/src/rpc.rs:225`
calls the *straight* `execute(&targets, …)` — the `collect_order` deps-first loop in
`razel-build/src/lib.rs:191` — **not** `IncrementalBuilder`. The Engine-backed `IncrementalBuilder`
(`razel-build/src/incremental.rs:41,83`) is exercised only by `razel-daemon/src/lib.rs`'s own `build()`
and its tests. So **both** product build paths (CLI via `build_bazel_with`/`build_workspace_with` →
`execute`, and the daemon RPC via `rpc.rs:225` → `execute`) currently bypass the Engine; only a warm
side-path uses it.

Why this matters for the critique: A2 sells step 5 as "delete the second loop, the Engine is already the
daemon's path." It is not. Unifying onto the Engine is a larger cutover than A2 implies (two callers to
move, plus the daemon's `warm_analyze` path), and A2 has under-priced its single biggest step on a claim
the code contradicts. A1 states the same landscape *correctly* ("the daemon … still calls the straight
`execute`, not even the Builder", draft-arch-1.md §1.F5/7/10). When the two drafts disagree on a load-
bearing code fact, A2 is the one that's wrong, and it's wrong in the direction that flatters A2's
migration cost.

### (c) F6 (early cutoff) for *analysis* — claimed, but the existing cutoff is value-level over opaque Digests, and typed-provider cutoff is unproven

A2 §2 claims lifting analysis into the Engine "gives F6 for analysis, not just execution," riding the
existing `changed_at`-not-bumped firewall (`razel-engine/src/lib.rs:82-90`, verified). But that firewall
fires on `Digest` equality of *opaque values*. Analysis-node early cutoff requires that a recomputed
`ProviderSet` hash *stably and cheaply* to a digest such that a semantically-unchanged provider compares
equal. That is a real design obligation (canonical provider serialization, stable `ProviderKey`
ordering, no incidental ordering noise) that A2 asserts as falling out "for free" but never specifies.
It is not free; it is the difference between Bazel's `SkyValue.equals` discipline working and not. A2
gets credit for *aiming* at analysis-level F6, but it has not shown the typed store can be digested
without the same equals-discipline tax Bazel paid.

### (d) F12 (configuration) ★ — A2 commits the model and *defers the machinery*, which is A1's move wearing a ● label

A2 §2 (F12) and §6 both say: adopt `select`+`config_setting` matching as the committed model, but
"**defer the full `(Target × Config)` cross-product machinery behind a single-config default** until a
real multi-config consumer exists." Compare A1 §1.F12: "decide the matcher model now … does **not** build
`(Target × Configuration)` keying as a graph axis … transitions deferred." **These are the same decision.**
Both decide the matcher model now and defer the cross-product. A2 dresses it as "● config-as-graph-axis"
in the dial table (§0) but the body (§6, "decide the model, defer the cross-product machinery") collapses
to ◑. So on F12, A2's *delivered* position is A1's, and the ● label is aspirational. This is the single
clearest place A2's self-described dial column overstates what the draft actually commits to build.

### (e) F21 (provenance) — under-served relative to the indirection A2 adds

A2 adds typed providers, a config axis, and union dispatch, but says almost nothing about F21. Each layer
of indirection (a provider chosen by membership, an action synthesized from a typed store) widens the gap
between "why did this action run" and the answer. A3 makes provenance a *non-optional* mitigation (its §6
mandates a derivation log). A2 does not even name F21 in its per-fundamental section. For the indirection
A2 spends, the absence of an explainability story is a gap — the more typed machinery between the BUILD
file and the spawn, the more F21 has to be designed, not assumed.

---

## 2. Which Bazel Constraint (C1–C9) does A2 strain or violate

A2 honors the C-surface where it keeps the existing Starlark front-end, and its claim "A2 changes the
*layer above*, not the surface" (§4) is structurally true. The strain is concentrated and real:

### C7 (configuration, platforms, toolchains, transitions) — strained by promise, not by delivery

A2 §0 and §2 promise transitions as "configurable edges" and `cfg = "exec"` as "the built-in instance,"
and the dial table claims ● "transitions." But §6 and step 6 defer the cross-product machinery, and
nothing in the draft specifies transition *semantics* (how a child's `Config` is rewritten, how the
`(Label, Config)` node identity is minted under a transition, how exec-vs-target config splits propagate
through `depset`). Today `select`/`config_setting`/transitions are *all* stubs (verified:
`razel-loading/src/rules.rs:744-753` picks `//conditions:default` or the first branch;
`config_setting` records an empty target). A2 commits to the hardest C7 surface (transitions are the part
Bazel needed `*AttributeMapper` for, S6) on the dial label while deferring its construction — so A2 *strains*
C7 by claiming to have decided more than the body specifies. This is not a violation (the surface still
parses), but it is the place A2's ● claim writes a check the draft doesn't cash.

### C4 (rule-authoring API) — A2 makes `provider()` and `attr.*` load-bearing, which is a *fidelity gain* but also the largest net-new surface

A2 §4 correctly notes `rule()` ignores `attrs` today (`rules.rs:587 let _ = attrs;`, verified) and that
A2 makes the schema load-bearing. Good — but this is net-new C4 surface (schema-driven `ctx` attr
typing, `provider()` with real `ProviderKey` identity and `target[Provider]` indexing) that A2 *must*
build for its F11 story to exist, whereas A1 gives `provider()` only a "thin typed identity" and keeps
the bundle. So A2 strains no C4 constraint, but it takes on the most C4 *work* of the three — and that
work is the substrate of A2's whole identity, so if it slips, A2 has nothing A1 doesn't.

No outright C-violation. The strain is C7-by-overpromise and C4-by-scope.

---

## 3. Which Part-B scar (S1–S12) does A2 most court — fatal or manageable?

A2 names its own scars in §6 (S8/S9/S10 + secondary S6) and that section is honest. The sharpest
adversarial reading:

### Most-courted: S8 (over-engineering / bespoke dataflow) — and the guard is softer than it reads

A2 dodges the *two-language* half of S8 cleanly (all Rust + Starlark, verified plausible — no PyO3
needed). But the **"bespoke dataflow compiler"** half is exactly what "lift analysis into a typed,
parallel, config-keyed demand graph" *is*. A2's guard (§6) is "the engine stays a demand/restart memo
graph (Skyframe-lite), never a static plan-solver. No five-phase builder." That guard is a *promise of
restraint*, not a structural barrier. The same temptation that grew Pants's 5-phase monomorphizer is
latent the moment you have a typed `Key` enum, config-keyed nodes, and union dispatch: the next natural
step is to pre-validate the plan, which is the static-plan-solver A2 swears off. **S8 is the scar A2 most
courts, and its only defense is discipline, not architecture.** Is it fatal? *Not fatal, but manageable
only by sequencing* — A2 is safe iff steps 4–6 (§7) are actually demand-paced and stopped early when the
need doesn't materialize. The risk is that once the typed Key + config axis exist, "we already have the
machinery, let's use it" pressure erodes the guard. Manageable, but the burden is on process, and process
guards are the weakest kind.

### S7 (duplicated source of truth) — A2 *resolves* it, but its plan under-prices the work (see §1.b)

A2 correctly identifies the split-brain (CLI/daemon `execute` loop vs off-path Engine) and makes
unifying it step 5. Good — this is a genuine S7 cure. But because A2 mis-states the daemon as already
Engine-backed (§1.b above), it under-scopes the cure. Not fatal; it means step 5 is bigger than billed.

### S10 (wrapper-type proliferation) — the A2-specific risk, manageable by the stated guard

First-class typed providers invite a `FooInfo` per micro-fact and composition-by-type invites accidental
collisions. A2's guards (explicit `ProviderKey` identity; keep composition-by-type *soft* — consumer
names the provider, not Pants's auto-return-type wiring; review providers like API) are the *right*
guards and are architectural, not just disciplinary (the `ProviderKey` is a type-system fact). Manageable.
This is A2's best-handled scar.

### S9 (static-graph startup rigidity) — A2 explicitly avoids it; the avoidance is credible

A2's "softer than Pants" stance (Starlark stays dynamic; missing dep re-demands via restart, does not
`fail()`) is the correct read of the brief and is structurally supported by keeping the Starlark surface
(F18 already owned). A2 is at the *opposite* scar-pole from S9, deliberately. No charge here — this is
A2 at its strongest, and it is the one place A2 clearly out-positions a hypothetical Pants-clone.

**Verdict on scars:** the most-courted is S8 and it is *manageable but only by sequencing discipline* —
which is the softest possible mitigation. Not fatal, because every step in §7 is reversible up to step 4
and stoppable; fatal *only if* the team treats the typed Key + config axis as license to build the
parallel analysis graph before a workload demands it.

---

## 4. Is A2 genuinely DISTINCT, or does it collapse into A1 or A3?

This is the most damaging line of attack, and A2 is **partially vulnerable on both flanks.**

### A2 vs A1 — the distinctive content is thinner than the framing suggests

A2 §6 makes a remarkable admission: "the keystone move (Session/provider-store/no-thread-locals) is
required by A1 too — so A2's *extra* spend is really just 'the unified graph + the config axis'." And §7
states the first three migration steps "are **shared with A1**." So strip the shared substrate and A2's
*distinct* content is exactly three things: (i) typed providers replacing the bundle, (ii) the unified
Engine on the live path (which A1 *also* does as its one forced ◑ notch — draft-arch-1.md step 6), and
(iii) the config axis (which A2 defers to a single-config default, i.e. A1's position — see §1.d).

Netting it out:
- **F12: A2 = A1** in what's delivered (matcher model decided, cross-product deferred). The ● label is
  aspirational.
- **F5/F7/F10: A2 ⊃ A1** only by the *parallel typed analysis graph*, which A1 explicitly defers as
  "incremental analysis … deferred" while still routing the live path through the Engine. So the
  *delivered* difference is "A2 commits to eventually lifting analysis into the Engine and typing the
  Key; A1 commits to execution-only incrementality first." Real, but narrow.
- **F11: A2 ⊃ A1** genuinely — typed providers from day one vs A1's "thin `ProviderId` + escape-hatch
  map." This is A2's *one unambiguous distinct win*.

So A2 is distinct from A1 essentially on **F11 (typed providers, real) + a stronger F5/F7/F10 ambition
(largely deferred)**. That is a narrower distinctness than the ●-vs-◐ framing implies. An unsparing
reviewer would say: **A2 is A1 + "we commit to the typed provider contract now and promise to lift
analysis into the Engine later."** The "later" parts (config cross-product, parallel analysis) are
deferred to the same horizon A1 defers them to. A2's *guaranteed near-term* delta over A1 is the typed
provider channel and nothing else.

### A2 vs A3 — distinct, and A2 is the more defensible of the two

A3 (facts/matchers/derivation engine) is a real second declarative layer below Starlark; A2 keeps the
imperative `rule()` body and only types its *output channel* (providers). A2 explicitly does **not** turn
`cc_library`'s transitive `hdrs`/`cflags` accumulation into a `fold_over_deps` derivation primitive —
that stays in the rule body. So A2 is genuinely distinct from A3: A2 types the *contract*, A3 re-slices
the *computation*. A2 is the less-abstract of the two right-column options, and it dodges A3's worst
spots (the comprehension wall of two declarative layers; the non-firing-matcher provenance hole). On the
A2-vs-A3 axis, A2 is clearly distinct and clearly the safer ● bet.

**Conclusion on distinctness:** A2 is distinct from A3 (different layer typed). A2 is *weakly* distinct
from A1 — its guaranteed near-term delta is the typed provider channel; everything else it claims over A1
is deferred to A1's own horizon. A2 does **not** collapse into A1, but it is closer to "A1 + typed
providers + a stated intent to grow" than the ●/◐ dial framing admits.

---

## 5. The single biggest risk + what would have to be true for A2 to be the wrong choice

**Biggest risk:** A2 pays the full *engineering* cost of the typed-rich column (typed Key enum,
`Send`/`Sync` analysis graph, config-keyed nodes, union dispatch) ahead of any workload that exercises
parallel multi-config analysis — and the cost does **not** shrink with "~0 consumers" the way A2's thesis
claims. The retrofit-asymmetry argument (S3/S6 are expensive to undo) is genuinely true for the *typed
provider contract* (F11) and for *deciding* the config model (F12) — both are cheap-now/expensive-later,
so committing them is correct. But A2 staples to those two well-justified commitments a *third* — the
parallel, config-keyed, typed analysis Engine — whose cost is a fixed engineering tax, not a retrofit
tax, and which therefore does **not** benefit from being done early with few consumers. That third thing
is where over-build (S8) lives, and A2's defense for it is "demand-paced, stoppable" — i.e. it might not
actually build it. If it doesn't, A2 *is* A1-plus-typed-providers (see §4). If it does, ahead of need, it
is the too-abstract dead end.

**What would have to be true for A2 to be the wrong choice:**
1. **Multi-config and parallel *analysis* are not coming soon** (only parallel *execution* is). Then the
   config axis and the typed analysis-graph lift are generality nobody exercises, and A1 gets the same
   near-term outcome (typed-enough providers, Engine-on-live-path, parallel execution) for less. A2 itself
   concedes this exact conditional: "If you don't believe multi-config and parallel analysis are coming,
   A1 (thin) is the right call and A2 is over-built" (§6).
2. **The typed-provider digest/early-cutoff discipline (§1.c) proves as costly as Bazel's `equals`
   work.** Then A2's "F6 for analysis falls out free" is false, and the analysis-graph lift carries a
   hidden tax that erodes the asymmetry argument.
3. **The team cannot hold the §6 S8 guard** (no static plan-solver, no five-phase builder) once the typed
   Key + config nodes exist. Then A2 slides into the bespoke-dataflow-compiler scar it swears off, and
   the only barrier was discipline.

If all three hold, A2 is the wrong choice and A1 is right. If even multi-config alone is genuinely
coming, A2's F12-decided/F11-typed core is vindicated and A1's S6/S3 deferral becomes the expensive
retrofit.

---

## Verdict

**Viable: yes** — A2 is internally coherent, code-grounded, and its scars are named honestly; it is a
legitimate point on the dial and the safer of the two right-column options (clearly beats A3 on
comprehension and provenance).

**Where it clearly wins:** F11 — typed providers with explicit `ProviderKey` identity, from day one,
replacing the `rules.rs` `DefaultInfo`+`hdrs`+`cflags` bundle and the `dep[Info]` reconstruction
(`rules.rs:498-521`); this is the one expensive-to-retrofit contract (S3/R4) that A2 commits and A1 only
half-commits. Also S9-avoidance (dynamic Starlark surface kept) and S10-handling (`ProviderKey` identity
+ soft composition) are A2 at its best.

**Where it clearly loses:** (1) F7 and the parallel typed analysis Engine — the largest, fixed-cost build
in any draft, spent ahead of a workload that needs it (the S8 over-build edge), and tied to F12's
cross-product which A2 *defers to A1's position anyway*. (2) It under-prices its biggest migration step on
a false code claim — the daemon RPC path calls the straight `execute` (`rpc.rs:225`), not the Engine, so
"the Engine already drives the daemon" is wrong (A1 has this right). (3) Its near-term distinctness from
A1 reduces, on inspection, to "typed providers + a promise," since F12-cross-product and parallel
analysis are deferred to the same horizon A1 defers them to. (4) F21/provenance is unaddressed despite
A2 adding the most indirection between BUILD and spawn.

**Net:** A2 is the right bet *iff* multi-config and parallel analysis are genuinely on razel's roadmap;
its F11 core should be adopted regardless of which architecture wins. Its weakest move is binding the
expensive parallel-analysis-graph engineering to the cheap retrofit-now argument that only justifies the
typed contract and the config *decision* — those two should be taken now; the analysis-graph lift should
be held behind a real workload, exactly as A2's own step 5–6 sequencing allows but its framing oversells.
