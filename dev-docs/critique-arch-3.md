# Adversarial Critique — Architecture A3 (M2 Inversion: Facts / Matchers / Compositional Rules)

*Adversarial review of `draft-arch-3.md` against `ArchFundamentals.md` (F1–F23),
`ArchBazelConstraints.md` (C1–C9), `ArchPatterns.md` (S1–S12 + Part C dial),
`ArchModel.md`, `YIDLDigest.md`, and the code at 62 commits. The brief: steelman
in two sentences, then break it.*

---

## Steelman (two sentences)

A3 re-slices razel's analysis layer so the build is *derived* — `cc_library(...)`
asserts typed `Target`/`Attr`/`DepEdge` facts into a per-`Session` `FactStore`, and
isolated **matchers** (toolchain, config) and **compositional rules** (provider
derivation, action synthesis) read-and-derive new facts that lower into razel's
existing demand-driven `Engine` — dissolving the 64-line `native_cc_library`
god-body (`rules.rs:900-963`) and the `CXX`/`AR` consts (`:844`) into ~5
independently-testable units, with state forced to be a passed value *by
construction* (you cannot assert a fact without a store handle). It explicitly
steals only YIDL's authoring/decoupling slicing and rejects YIDL's once-through,
fixpoint-free, provenance-poor *evaluation* engine — keeping razel's
`razel-engine` and content-addressed kernel underneath.

The draft is unusually honest about its own weaknesses (§6, §7c, §8). That honesty
is the steelman's ceiling: A3 is the best-argued of the three, and *because* it
pre-concedes its risks, the real adversarial work is testing whether those
concessions are survivable or fatal. They are mostly survivable — and that is
exactly why A3 is dangerous: it talks itself into being the wrong choice while
sounding like the most rigorous one.

---

## 1. Which Fundamental does A3 satisfy poorly or expensively (esp. ★)

### F5/F6/F10 (★ incrementality, early cutoff, demand-driven) — the load-bearing crack

A3's §4 dial table scores F5/F7/F10 as **◑ "Adequate, by *not* touching it"** and
concedes the honest weakness in the same cell: *"A3 must prove derivation itself is
incremental (re-deriving only affected targets' facts), which YIDL gives no help
with."* This is not a footnote — it is the structural center of gravity for a build
tool, and A3 has it backwards.

The concrete failure: A3 inserts a **whole new derivation layer (Plane 2) between
loading and the Engine (Plane 3)**, and the Engine's incrementality lives *below*
that layer. The `razel-engine` is `String`-keyed over opaque `Digest` values
(`razel-engine/src/lib.rs:15-17`: `type Key = String; type ComputeFn = Box<dyn
Fn(&[Digest]) -> Digest>`) and its early-cutoff (`changed_at` advances only on value
change, `lib.rs:80`, confirmed) operates on **execution** nodes. A3's facts,
matchers, and derivation rules run *above* the Engine and are re-run wholesale per
`analyze_*` — exactly the limit A1 names for itself (draft-arch-1 §1.F5: "Re-analysis
of an edited BUILD is still whole-package"). So A3 buys **no analysis-incrementality
the others lack**, while adding a fact-store + matcher-dispatch + derivation pass
that must itself be recomputed on every analysis. The §3 "demand-driven *within*
target derivation" is asserted as the divergence from YIDL but is *not* node-granular
invalidation: re-deriving "only affected targets' facts" requires invalidation keyed
on fact identity, which A3 sketches nowhere. F6 (early cutoff) at the analysis layer
is simply absent — a changed `define` re-derives the whole `CcInfo` fold.

Verdict on F5/F6/F10: A3 satisfies them **no better than A1 or A2** and pays a *new
recompute cost* (the derivation pass) to get there. The §6 perf note ("Astichi needed
a Rust hot path... a build runs analysis far more often than a once-per-class codegen")
is the draft conceding this — but it files it under "constant factor to watch," when it
is actually the ★ axis the whole tool exists for. **This is the most expensive
satisfaction of a ★ fundamental in the draft.**

### F11 (★ decoupling) — genuinely strong, but not distinctively so

A3's strongest claim (§4: "●→ typed providers, derived... Strong & cheap... YIDL's
best idea"). It is real. But the typed-provider-as-contract win is *identical* to what
A2 commits to (draft-arch-2 F11: "typed from day one") and a notch above A1's flat
bundle. A3 does not satisfy F11 *better* than A2; it adds derivation *machinery around*
the same provider contract. Credit where due — but this is not where A3 wins over the
field.

### F21 (provenance) — satisfied only by a mitigation the draft admits is non-optional

§6 is candid: YIDL's matchers fall through silently (`matcher.py:386-388`), and "for a
build tool whose daily question is *why didn't this action run?*, silent non-firing is
disqualifying." A3's answer is a **mandatory derivation log**. Fine — but note what
this means: A3's F21 satisfaction is **entirely in the mitigation, not the model**. The
model (facts→matchers→derived facts) is *natively worse* at F21 than today's
straight-line `rules.rs`, where the cause of an action is the lexically-adjacent code
that emitted it. A3 must *out-engineer* its own substrate to reach parity. The draft
says so ("If the log is skipped, A3 degrades to... worse debuggability than today").
That is a fundamental the architecture **fights**, not one it satisfies cheaply.

### F17 (orthogonal derivation) — the one A3 uniquely satisfies, and the one razel may not need

§7c is the honest crux: A3's extra layer "pays off **only if** razel grows orthogonal
derivations — IDE data, lint, coverage, codegen (F17)." F17 is explicitly **not** a ★
fundamental (`ArchFundamentals.md:92`, tagged "extended-core, not minimal-core"). A3's
distinctive value is keyed to a *non-load-bearing* fundamental. That is the whole
ballgame for the recommendation (see §5).

---

## 2. Which Bazel Constraint (C1–C9) does it strain or violate

A3 satisfies C1–C9 honestly by **keeping Plane 1 (Starlark) unchanged** (§5), and that
argument holds: it changes what builtins' *effects* are (assert facts vs mutate
thread-locals), not their semantics. The seam claim (§5: the engine "never learns
cc_library," better than today's `wire_to_ir` name-suffix inference at
`analysis.rs:112-120`) is verified accurate — `infer_kind` really does sniff `_test`/
`_binary` suffixes (`analysis.rs:113-118`), so A3's typed `RuleKind` is a genuine
improvement.

But two constraints **strain** under A3:

### C4 / C7 — `ctx.actions.run(...)` lowering to an `ActionReq` fact is asserted, not shown

§5 claims: "An evaluated `rules_cc` `cc_library` impl that calls `ctx.actions.run(...)`
has that call lower to an `ActionReq` fact — the impl never knows it's feeding a fact
store." This is the hardest unproven claim in the draft. Bazel's `ctx.actions.run`
takes a `ctx`-bound, heap-allocated `Args` object, `depset` inputs with traversal
order, and `File` outputs that are *Starlark values produced earlier in the same impl*.
A fact is "an immutable typed record in an identity-bearing collection" (§1). Lowering a
live `ctx.actions.run` call — mid-impl, with Starlark heap references — into an
identity-bearing immutable fact is a real serialization/identity problem the draft waves
through. The worked example (§2) only shows the **native shim** path (`cc_library`
asserting facts directly); the **evaluated-`.bzl`** path (C4/C5, the path that actually
matters for `@rules_cc`) is asserted to "just work." This is the same gap A3 accuses
YIDL of (`YIDLDigest §5`, evaluated-field escape hatch) — relocated to the
Starlark/fact boundary.

### C7 (config/transitions) — A3's matchers are Eq-only by construction

§2b: "The matcher table is **Eq-and-AND only by default** — which is exactly YIDL's
decidable core." A3 proudly keeps the closed predicate enum and refuses YIDL's
evaluated-field escape hatch. But C7 requires `constraint_setting`/`constraint_value`
subsumption and configuration *transitions* — a target's identity varying per config.
The draft's §6 admits the predicate must grow to `Predicate::flag_in(set)` for ranges,
but says nothing about transitions, which are not a predicate problem at all — they
change the *key* a target is derived under. A3's fact identity is `(label)` for
`Target`, `(label, attr)` for `Attr` (§2a); there is **no `Config` in the identity
tuple**. §4's F12 cell hand-waves "the fact store is keyed by `(label, config)`" but
§2a's stated identities are not. This is an internal inconsistency, and it is the same
S6 (config-bolted-on-late) trap A1 is criticized for — A3 has not escaped it, it has
relabeled the select-stub replacement as a "matcher."

---

## 3. Which scar (S1–S12) does it most court — fatal or manageable?

### S8 (over-engineering / out-building the need) — the headline scar, and A3 *names it as its own*

§6: "A fact/matcher/derivation engine for 4 languages is plausibly more machinery than
the problem demands; A3 is the candidate most exposed to 'you built a dataflow compiler
for a job a typed Session + open-set dispatch (A1) already does.'" This is precise and
correct. `ArchitectSkillRules.md` warns the too-abstract end "ossifies, obscures, also
a dead end," and `R7`/`ArchModel.md` explicitly flags that even `inventory`-style
auto-registration is "the too-abstract dead end for ~12 builtins." A3 proposes a
*derivation engine* over the same ~12 builtins + 4 languages. The razel exemplar in
`ArchitectSkillRules.md:125-136` distills the shape to "Words/Nouns/Phrasebooks/Session"
— and concludes the leverage was "the Session via `eval.extra`... one move, two
payoffs." A3 adds a second M2-level abstraction *on top of* that already-sufficient
distillation.

**Is S8 fatal? Manageable in mechanism, fatal in calibration.** The machinery would
work. But `ArchModel.md §1` already collapsed the feature-ocean to a finite shape that
A1 satisfies; A3 re-expands it into a generator-of-generators (the M2 framing it
inherits from YIDL, `YIDLDigest §2`). Per the cardinal rule ("architect against the
finite shape, not the ocean"), A3 over-architects a shape that is already finite. That
is a calibration failure, not a bug — and calibration failure is exactly what S8 *is*.

### S9 (static-graph startup rigidity) — courted deliberately, manageable

A3's closed `Phase::{Config,Toolchain,Provider,Action}` enum (§6) and definition-time
confluence check trade toward Pants's rigidity. The draft argues this is the *right*
trade for determinism and keeps Plane 1 dynamic. Agreed — this slice of S9 is
manageable and is arguably correct. Not the fatal one.

### S10 (wrapper-type proliferation) — real, mitigated on paper

`Resource`/`ProviderType`/`Fact`-variant sprawl (§6). Mitigation: "keep `Fact` a small
closed enum." Plausible but in tension with F16: every new language wants new `RuleKind`
variants, new `Resource` variants, new `Phase`-ordered rules. The "small closed enum"
and "open-set extension" goals pull against each other. Manageable, but the draft
under-weights the tension.

### S5 (god-module) — A3's best anti-scar claim, and it holds

§7a: "the `rules.rs` 1,658-LOC gravity well has *no place to reform* because there is no
central recording function to funnel into — facts are asserted locally." This is the
genuinely strong structural argument and it is correct: `record_target` (`rules.rs:847`)
*is* the funnel today, and A3 dissolves the funnel. **But A1 also dissolves it** (Session
fields + words/ split), so this is a tie with A1, not a win.

---

## 4. Is A3 genuinely DISTINCT, or does it collapse into A1/A2?

**This is A3's biggest vulnerability, and the draft's §1 quietly admits it.**

A3 §1: "A3 is **not** a competitor to A1 (the Dialect). It is A1's **Session generalized
into a fact store**." And: "`Analysis.providers` in `RazelDialect.md §3` becomes
`FactStore.collection::<Provider>()`; the same move that A1 makes to kill thread-locals,
A3 makes — and then adds the derivation layer on top." A3's own migration §8 confirms
steps 1–2 (delete dead loader; introduce Session/FactStore) are "**identical to A1's
keystone move**... A3 and A1 share steps 1–2 exactly."

So A3 = A1 + a derivation layer. The distinctness reduces to **one thing**: matchers +
compositional derivation rules replacing imperative rule bodies (§8 step 3). Everything
else — passed Session, typed providers, manifest registry, kept Engine, kept kernel — is
shared with A1 (and the typed-provider half with A2).

Worse for distinctness: **A1 and A2 already claim the facts-and-matchers borrow.**
- A1 (draft-arch-1:32) calls `AnalyzedTarget` "razel's fact... the one YIDL idea A1
  steals — facts-as-substrate (F11)," and A1's F12 (draft-arch-1:128) uses "YIDL's
  Eq-only, AND-combined, highest-score-first matcher" for `select`.
- A2 (draft-arch-2:51) says "the provider store is literally 'facts as providers'" and
  A2's F12 borrows "YIDL's definition-time-disambiguated, Eq-style matcher for this
  bounded decidable slice only."

So "facts" and "matchers" are **already spent** by A1 and A2 at the one place razel
actually needs them (`config_setting`/`select` dispatch). A3's *additional* distinct
claim is narrower than the framing suggests: it is **compositional derivation rules for
provider/action synthesis** (`fold_over_deps`, the `rule!` macro at §2c) replacing
imperative rule bodies. That is the real, distinct A3 idea — and it is exactly the part
the draft's own §8 step 3 names as the **go/no-go kill gate** and `YIDLDigest §6` names
as YIDL's weakest spot ("transitive depset accumulation falls outside the declarative
layer").

**Verdict on distinctness:** A3 is distinct *in principle* (derivation-as-data vs
imperative rule bodies) but the distinction is thin and concentrated entirely in the one
mechanism (`fold_over_deps`-style transitive derivation) that the draft itself flags as
unproven. Strip that one mechanism and A3 **collapses into A1** — by its own §1 and §8
admission. The matcher/fact vocabulary is shared marketing across all three; the
derivation engine is the only true differentiator, and it is the riskiest piece.

---

## 5. The single biggest risk + what must be true for A3 to be the wrong choice

### Biggest risk: the transitive-derivation primitive (`fold_over_deps`) is not cleaner than the loop it replaces

A3's entire differentiated value rests on §2c's claim that transitive `hdrs`/`cflags`
propagation expressed as `fold_over_deps(label, |dep| provider::<CcInfo>(dep).hdrs)` is
"meaningfully cleaner and more testable than the inline loop at `rules.rs:914-920`"
(§8 step 3, the go/no-go gate). I read that loop (`rules.rs:914-920`):

```rust
let (mut dep_names, mut dep_hdrs, mut dep_cflags) = (Vec::new(), Vec::new(), Vec::new());
for d in &unpack(deps) {
    let dep = resolve_dep(d)?;
    dep_hdrs.extend(dep.hdrs);
    dep_cflags.extend(dep.cflags);
    dep_names.push(dep.canon);
}
```

This is **six readable lines** over an already-resolved `DepInfo` (`rules.rs:856-894`).
`fold_over_deps` is a *higher-order derivation primitive* that must (a) resolve the dep
closure, (b) order it, (c) handle the demand-driven cross-package load that
`resolve_dep` does today (`rules.rs:874-882`), and (d) be expressible as a closed,
inspectable derivation rule rather than "a YIDL evaluated field" (which §2c explicitly
forbids). The honest reading: **`fold_over_deps` is the YIDL evaluated-field escape
hatch wearing a Rust costume.** It is the one place transitive closure is unavoidable,
and `YIDLDigest §5/§6` says plainly that recursive/transitive derivation "cannot be
expressed as facts→rules" and "must be an evaluated field / operation." A3 renames the
escape hatch a "derivation primitive" and asserts it stays declarative. If that
assertion fails at the go/no-go gate — and the digest's own analysis says it will —
A3's distinct value evaporates and §8 step 3 reverts to A1.

### What must be true for A3 to be the wrong choice (it mostly is true)

A3 is the **wrong** choice if **all** of the following hold — and the draft concedes
each:

1. **F17 (orthogonal derivation) is not a named, near-term goal.** F17 is not a ★
   fundamental. The draft (§7c) stakes A3's entire calibrated-extensibility case on
   F17 being adopted: "*If razel stays a minimal core, A3 is over-built (S8).*" Nothing
   in `ArchModel.md` or the north star elevates F17 to near-term.

2. **Analysis-incrementality (F5/F6 at the analysis layer) is not delivered by the
   derivation layer** — and it isn't (§1 above; §4 concedes it). So A3 pays the
   derivation-recompute cost without buying the ★ property a build tool exists for.

3. **`fold_over_deps` is not cleaner than the six-line loop** — likely, per the digest's
   own verdict and the go/no-go gate's existence.

If (1)∧(2)∧(3), then A3 = A1 + a derivation engine that satisfies no extra ★
fundamental, costs extra recompute, and concentrates its value in a primitive that
fails its own kill-gate. That is the precise definition of the too-abstract dead end
`ArchitectSkillRules.md` warns is "generality nobody needs."

A3 is the **right** choice only in the narrow world where F17 is a committed roadmap
item (lint/IDE/coverage/codegen as first-class derivations over the build graph) — in
which case it is the *only* candidate that expresses orthogonal derivation natively, and
the extra layer earns its keep. The draft says exactly this and to its credit does not
oversell it.

---

## Verdict (6 lines)

1. **Viable? Mechanically yes, calibration-wise no — A3 is the too-abstract dead end this project explicitly warns against (S8), and its own §6/§7c say so.**
2. **Clear win:** modularity (a) and testability (b) at the finest grain of the three — matcher = pure `fact_tuple→Resource`, derivation = pure `fact_view→fact_delta`; and F17 (orthogonal derivation) is the one fundamental A3 alone expresses natively.
3. **Clear loss:** F5/F6/F10 (★) — the derivation layer buys *no* analysis-incrementality A1/A2 lack while adding a new recompute pass; F21 is fought (saved only by a mandatory log), not satisfied cheaply.
4. **Distinctness is thin:** by its own §1/§8 admission A3 = A1 + a derivation layer; facts+matchers are already spent by A1 and A2 at the one place razel needs them (`select`/`config_setting`), so the sole true differentiator is `fold_over_deps`-style transitive derivation.
5. **Biggest risk:** that differentiator is the YIDL evaluated-field escape hatch relabeled — the six-line loop at `rules.rs:914-920` is already clear, and the digest's own verdict (`YIDLDigest §6`) is that transitive closure cannot stay in the declarative layer; the §8 go/no-go gate exists *because* this is expected to fail.
6. **Wrong choice iff F17 is not a near-term goal — which today it is not — making A1 the ~80%/40% pick; recommend A3 only if lint/IDE/coverage/codegen-as-derivations is committed roadmap, and gate it hard on the cc transitive-propagation pilot.**
