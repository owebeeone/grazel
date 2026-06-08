# Adversarial Critique — Architecture A1 (Thin Dialect over Starlark, ◐ thin-dynamic, Bazel-grain)

*Reviewer stance: try to break it. Claims grounded against the code at 62 commits and the
framing docs (F1–F23, C1–C9, S1–S12, Part C dial). Citations verified, not pattern-matched.*

---

## Steelman (two sentences)

A1 is the cheapest credible architecture because razel already owns the only expensive
relief valve a build tool needs — an embedded, build-tool-agnostic Starlark interpreter
(`starlark = "0.14.2"`, verified) — so it spends *new* indirection at exactly two seams that
are irreversible-if-skipped (a passed, plural `Session` replacing the 8 thread-locals, which
*is* the F11 provider store; and a const-array manifest registry), takes the one forced graph
notch (route the live build through the already-built `Engine`), and honestly names every
remaining hard-code as a future rework boundary. Its discipline is admirable: every claim it
makes is grounded (I re-verified `rules.rs:744` select stub, the 8 thread-locals, the dead
loader reachable only from its own test at `analysis.rs:192`, `extra`/`extra_mut` at
`evaluator.rs:176–178`, and the `IncrementalBuilder::configure` AnalyzedTarget→Engine mapping
at `incremental.rs:106–`), and it refuses to over-build for ~12 builtins.

Now the breaking.

---

## 1. Fundamentals it satisfies poorly or expensively — with the concrete failure

### F12 (configuration / variation) — the load-bearing failure, and A1 half-admits it

F12 is not a ★ in the Fundamentals' own star-list (the ★s are F2, F3, F5, F7, F10, F11, F15,
F16, F18, F19 per `ArchFundamentals.md` line 129). But the Part C dial map (`ArchPatterns.md`
line 124–131) *treats it as load-bearing* — it gets its own dial row — and `ArchModel.md` R9
elevates it to a settled-early decision. **A1's treatment is the weakest of the three drafts
and the failure is concrete, not abstract.**

A1 stakes F12 at ◑ "select + config_setting matching" (§1.F12, draft line 119), borrowing
YIDL's "Eq-only, AND-combined, highest-score-first matcher." The draft is candid that it does
**not** build `(Target × Configuration)` keying, `Fragment`s, or transitions, and names this
"A1's characteristic dead-end" (line 142). Good honesty — but honesty about a defect is not
absence of the defect. The concrete failure:

- **The forward-declaration defect compounds the config gap.** A1 inherits today's
  single-pass dep resolution: a dep not yet analyzed hard-errors (verified `rules.rs:510–516`:
  *"dep not analyzed yet — declare it before its users (forward references not yet
  supported)"*). A1 flags this (§1.F11, line 76–79) and says the F5/7/10 notch *might* bring a
  load→analyze split — but it does **not** commit to building two-phase analysis. So A1 ships a
  config matcher that resolves `select()` against flags, on top of a dep-resolution model that
  can't even handle forward references, let alone the *same target appearing under two configs
  in one invocation*. The matcher being "correct for the common case" (line 132) is true and
  irrelevant to the hard case F12 exists for.

- **The matcher fact lives in `Session.configs` but identity is still label-keyed.** A1's fact
  collection is `results: RefCell<BTreeMap<String, AnalyzedTarget>>` (draft line 203) — keyed by
  *canonical label*, a `String`. Bazel's whole reason for `(Target × Configuration)` is that a
  target's *identity* varies by config; a `BTreeMap<String, _>` structurally cannot hold two
  configurations of `//pkg:lib` at once. Retrofitting the key from `Label` to `(Label, Config)`
  is the single most expensive data-contract migration there is (R4: "among the most expensive
  things to undo"). A1's plural Session makes *holding N sessions* possible (line 117) — but
  that is N *whole-run* snapshots, not N configs of one target inside one run. The plurality A1
  is proud of does not actually buy the F12 axis; it buys concurrent *package* analysis, a
  different thing. This is the gap between "incremental execution now" (which A1 delivers) and
  "incremental + multi-config analysis later" (which A1 explicitly defers, line 116). **F12 is
  satisfied cheaply for the demo path and expensively — via a key-type migration — for its
  actual reason to exist.**

### F11 (★) — satisfied, but the typing is thinner than the draft's own escape hatch admits

A1 picks ◐ "label→bundle lookup, thinly typed" — a `ProviderSet` newtype with fixed fields
(`default_info`, `hdrs`, `cflags`) plus an open `BTreeMap<ProviderId, ProviderInstance>` escape
map (draft line 61–64). The draft concedes this "does not clear S3 fully" (line 71). The
concrete failure: **the moment a third-party ruleset defines `provider()` with semantic fields,
those land in the escape `BTreeMap` while cc's own outputs stay in the privileged hard-coded
`hdrs`/`cflags` struct fields.** That is a two-tier provider model — first-class for the
builtins razel shipped, second-class for everything users add — which is *exactly* the asymmetry
F11 forbids ("any producer satisfying the contract is substitutable," F11 line 66). A1 names
this as the S3 risk (line 337) but the cost is understated: it is not "migrate later if it
proliferates," it is "the design encodes cc as privileged from day one," and un-privileging it
is the same struct→declared-provider migration Bazel paid years for (S3).

### F5/F7/F10 (★★★) — the "free" notch is not free, and the daemon evidence undercuts the claim

A1's central optimistic claim is that the Engine notch lands "essentially for free" because the
Engine and the AnalyzedTarget→Engine mapping both already exist (§1.F5/7/10, line 108–111). I
verified both: `IncrementalBuilder::configure` (`incremental.rs:106–`) does map file→input,
action→derived, target→derived; the Engine has real early-cutoff (`request_inner`,
`lib.rs:149–169`). **But two concrete things break the "free" framing:**

1. **F7 (parallel, ★) is *not* free and A1 hand-waves it.** The Engine is single-threaded by
   construction: `nodes: RefCell<HashMap<…>>`, `revision: Cell<Rev>`, `in_progress:
   RefCell<HashSet>` (verified `lib.rs:33–38`) — `RefCell`/`Cell` are `!Sync`, and
   `request_inner` recurses synchronously through `self.request(d)?` (`lib.rs:140`). A "parallel
   scheduler over the ready-frontier" (draft line 110) is not a scheduler *over* this Engine; it
   requires making the Engine `Send + Sync`, which means replacing `RefCell`/`Cell` with locks or
   an actor and rewriting the recursive `request` into a worklist. A1 calls this "commodity: a
   worklist over the ready frontier" (line 111) — but the *graph it would schedule over* is
   today a `!Sync` data structure. A2 is honest that this is "the single largest build" (A2 draft
   line 99); A1 buries the same work behind the word "commodity." That is the one place A1's
   calibration-honesty slips.

2. **The daemon evidence cuts against A1's "Engine is the good path, just route to it" story.**
   I verified `daemon/rpc.rs:225` calls `execute(&targets, …)` — the *straight* `collect_order`
   loop (`razel-build/src/lib.rs:191`), **not** `IncrementalBuilder`, **not** the Engine. So the
   Engine is used by *neither* the CLI *nor* the daemon on the actual build path; it is exercised
   essentially by its own 6 tests. A1 acknowledges this (line 95–96) but it weakens the "cheap"
   claim: routing *all* builds through a subsystem that nothing currently builds through is more
   integration risk than "make the CLI call the Builder." The mapping existing in code ≠ the
   mapping being load-bearing and battle-tested.

### F17 (orthogonal derivation) — A1 cannot express it, and says so

F17 (aspects, lint, IDE data, coverage overlays) is explicitly out of A1's reach: "dispatch is
over a *flat* contract (target-kind + provider bundle), not Pants-style `@union`/typed plan; if
cross-cutting overlays (aspects, F17) arrive, this notch needs the next column" (draft line
163–164). This is the right call *if* F17 never arrives. But F17 is in the Fundamentals (line
92) and `cc_library`/`aspect()` is in the C4 surface A1 must eventually accept. A1 marks
`aspect()` as "NOT built" (C4, line 239). So A1 is structurally a minimal-core architecture; the
extended-core fundamentals (F17) are not deferred-with-a-home, they are *absent* — there is no
seam for an overlay to attach to, because the provider bundle is flat. If razel's roadmap
includes `query`/`cquery`-style derived info or lint-under-`test`, A1 needs the A2/A3 column.

---

## 2. Bazel Constraints it strains or violates

A1's front-end fidelity is genuinely its strength — it keeps razel's existing Starlark
front-end and *sharpens* the seam. The strains are real but mostly **shared with all three
drafts** (they are razel's current gaps, not A1-specific):

- **C7 (config / platforms / toolchains / transitions) — the genuine strain, and A1-specific
  in degree.** A1 explicitly defers `platforms`/`constraint_*`/toolchain resolution/transitions
  (C7, draft line 247–248), calling it "A1's biggest fidelity gap, named." This is the same
  surface as the F12 failure above. The strain is *worse* for A1 than for A2/A3 because A1's
  label-keyed fact collection has no place for `(Label, Config)` identity to grow, whereas A2
  commits the key shape now (A2 draft line 107) and A3's fact store keys by `(label, config)`
  natively (A3 draft line 245). A1 is the draft that most strains C7 by deferral.

- **C4 (rule-authoring API) — partial: `aspect()` and transitions not built.** Per F17 above.
  This is a 🟡, honestly marked (draft line 239), and shared with the others, but A1 has the
  least structural runway to close it.

- **C8 (external deps, `MODULE.bazel`/`bazel_dep`) — minimal `@repo`→local-path, no fetch
  (draft line 250).** Shared with all three; not an A1-specific violation.

- **C3 (`existing_rules`, full `genrule` make-vars/`$(location)`) — partial (draft line 237).**
  Shared; honestly noted.

**Verdict on constraints:** A1 does not *violate* any C-constraint — it parses or stubs the
vocabulary everywhere — but it *strains* C7 hardest of the three by structurally deferring the
config axis into a data contract (label-keyed map) that will have to be migrated to admit it.

---

## 3. The scar it most courts (S1–S12) — and whether it is fatal

A1's self-named characteristic dead-end is **TOO RIGID** (draft line 319), the symmetric
opposite of A2/A3's too-abstract. The scars, ranked by how hard A1 courts them:

- **S6 (configurability bolted on late) — the primary scar, and it is the real one.** A1's
  `select`+`config_setting` matcher is flat; transitions and `(Target × Config)` identity are
  deferred. `ArchPatterns.md` S6 cites Bazel's `*AttributeMapper` thicket as the cost of bolting
  config on late. A1's mitigant is *partial by its own admission*: "decide the matcher model now
  and keep the attribute-schema seam present (R9), but A1 does not build transitions, so S6 is
  *deferred, not defeated*" (draft line 334–335). **Is it fatal? Manageable-but-conditional.**
  It is fatal *iff* multi-config / transitions arrive — and then the cost is a data-contract
  migration (label → `(label, config)` key), which R4 flags as among the most expensive undos.
  It is manageable iff razel's future is "more languages, bigger graphs, single config." A1's
  entire viability rides on that conditional. The honest mark: A1 does not *defeat* S6; it
  *bets* S6 won't fire.

- **S3 (under-typed early data contract) — courted, partially hedged.** The flat provider bundle
  + escape map (§1 above). A1's thin `ProviderId` typing is "the *minimum* hedge" (draft line
  338). **Not fatal but irreversible-class:** migrating a data contract is expensive (R4), and
  A1's two-tier provider model (privileged cc fields vs escape-map for user providers) is the
  seed S3 warns about. Manageable only if third-party typed providers stay rare.

- **S12 (migration debt via guarded flips) — courted lightly, and A1 has the right answer.** The
  CLI→Engine routing and loader unification are flag-able transitions; A1 correctly says do them
  as outright strangler cutovers, not long-lived `incompatible_*` flags (draft line 343–345).
  Not fatal; this is the one scar A1 handles cleanly.

- **S9 (too-rigid startup) — *avoided*, correctly.** A1 keeps Starlark's lazy/dynamic validation
  and explicitly does *not* adopt Pants's startup rigidity (draft line 340–342). A1 sits at the
  opposite scar-pole from A2/A3. This is a genuine win, not a dodge.

- **S5 (god-module) — *defeated*, the design's whole point.** The wide-shallow-tree trade
  (~20 files for a 1658-LOC body) is near the upper bound of useful decomposition for ~12
  builtins (`ArchAnalRazel.md` §4 agrees) but A1 forbids a `dialect/` super-layer (draft line
  288), so it does not over-correct S5 into a new too-abstract problem.

- **S7 (duplicated source of truth) — *defeated first*, correctly sequenced.** A1's migration
  step 0 deletes the dead loader (`lib.rs` `TargetDecl`/`load_build`/`CTX`,
  `razel_analysis::analyze`) before refactoring — I verified these are reachable only from
  `analysis.rs`'s own test (line 192). This is the R14 prerequisite and A1 gets it right.

**Bottom line on scars:** S6 is the scar A1 most courts and the only one that is
*conditionally fatal*. It is not a dead-end *today* (razel is single-config); it is a dead-end
*the day multi-config arrives*, and the cost is a key-type migration. Manageable as a bet,
fatal as a surprise.

---

## 4. Is A1 genuinely DISTINCT, or does it collapse into A2/A3?

**A1 is distinct from A2/A3 — but the distinctness is narrower than the three-way framing
implies, because all three share their entire load-bearing prefix.**

The shared prefix is large and identical across all three drafts (verified by reading all
three migration sections):
- **Step 0:** delete the dead second loader (A1 step 0; A2 step 0; A3 step 1) — *identical*.
- **Step 1:** Lexicon extraction (A1 step 1; A2 step 1; A3 implied) — *identical*.
- **Step 2:** the Session keystone killing the 8 thread-locals (A1 step 2; A2 step 2; A3 step
  2) — *identical*, and all three explicitly say so ("identical to A1's keystone move" —
  A3 line 404; "shared with A1" — A2 line 297).
- **Words/Nouns lift + manifest** — *identical* (A1 steps 3–5; A2 step 3; A3 step 5).

So A1, A2, and A3 are **the same architecture for the first ~5 migration steps.** They diverge
only at the *end*:
- **A1** stops thin: flat provider bundle, select-matcher config, Engine-on-live-path for
  *execution* incrementality only.
- **A2** continues: typed provider lattice, `(Label, Config)` engine key, analysis lifted into a
  parallel Engine, union dispatch.
- **A3** continues differently: facts/matchers/derivation-rules below the surface, analysis as
  derivation.

**Does A1 collapse into A2?** No — but A2 collapses *toward* A1. A2's own blunt self-critique
admits: "the keystone move (Session/provider-store/no-thread-locals) is required by A1 too — so
A2's *extra* spend is really just the unified graph + the config axis… If you don't believe
multi-config and parallel analysis are coming, A1 (thin) is the right call and A2 is
over-built" (A2 draft line 288–291). That is A2 conceding that A1 *is* A2-minus-two-deferrals.
A3 makes the identical concession: "A1 (Dialect + typed providers) gets ~80% of the benefit for
~40% of the conceptual cost" (A3 line 379–380).

**The real shape:** these are not three architectures; they are **one architecture (the
Session-based Dialect) with a dial at the end**, and A1 is the leftmost detent. A1 is distinct
in *where it stops*, not in *what it is*. The honest framing — which A1's own draft gets right
(line 17: "leftmost ◐ column… mixing one notch right at F5/7/10") — is that A1 is a *calibration
point*, not a separate design. Its distinctness is real but is a difference of *degree of
indirection*, not of *kind*. The one place A1 is arguably a different *kind* is F12: A1's
label-keyed map is a structural commitment that A2/A3 do not make, and that commitment is what
makes A1's later pivot to A2 *expensive* rather than additive. So A1 is distinct precisely in
the dimension where its distinctness is a liability (the F12 key shape).

---

## 5. The single biggest risk + what would have to be true for A1 to be the wrong choice

**Single biggest risk: A1's data contract bakes in single-config identity (label-keyed
`BTreeMap`), and the day razel needs multi-platform / multi-mode / transitions, A1 owes a
key-type migration (`Label` → `(Label, Config)`) that R4 calls among the most expensive things
to undo — and A1's "plural Session" does not pay it down, because plurality is per-run, not
per-config.**

This is sharper than A1's own framing of its risk. A1 frames its risk as "we deferred
transitions" (a feature gap). The deeper risk is that the *deferral is encoded in the fact
store's key type*, so it is not an additive feature-add later — it is a contract migration that
ripples through every consumer that does `RESULTS.get(&label)` (the dep-resolution path I
verified at `rules.rs:508`). A1 says the plural Session "makes it possible later… without
forcing it now" (line 117) — but plural sessions hold N whole runs, not N configs of one target;
they do not soften the key-type migration at all. **A1's calibration claim has one soft spot,
and it is exactly here.**

**What would have to be true for A1 to be the WRONG choice:**
1. razel's roadmap includes **multi-config builds in one invocation** (host vs target, opt vs
   dbg simultaneously) or **configuration transitions** (`cfg = "exec"`) — i.e. C7 becomes a
   product requirement, not a someday. Then A1's label-keyed contract must be migrated, and
   doing it under A1 costs more than committing the `(Label, Config)` key now (A2's bet).
2. **OR** razel's roadmap includes **orthogonal derivation** (F17: aspects, lint, IDE/coverage
   overlays). A1 has no seam for these; the flat provider bundle gives an overlay nowhere to
   attach. A3 expresses them natively; A1 cannot without the next column.
3. **OR** the parallel scheduler (F7) turns out to be near-term and load-bearing, in which case
   A1 has *understated* the cost — making the `!Sync` Engine parallel is the same large build A2
   names, and A1's "commodity worklist" framing would have under-budgeted it.

**What would have to be true for A1 to be RIGHT:** razel stays a *minimal-core, single-config,
multi-language* build tool — more languages and bigger graphs, but not the config axis and not
overlays. Under that future, A1's deferrals never fire, its thinness is pure leverage, and
A2/A3 are over-built. Given razel is 62 commits in with `select` stubbed and no consumer asking
for transitions, this future is *plausible* — which is why A1 is viable, not dismissible.

---

## Verdict

**Viable: yes** — and it is the *correct default*, because its shared prefix (kill
thread-locals, delete the dead loader, manifest the registry, route to the Engine) is
no-regret work that every architecture needs, and A1 commits to *only* that plus the minimum
typing. A1 is the honest baseline against which A2/A3 must justify their *extra* spend.

**Where A1 clearly wins:** modularity (a) and the keystone state fix (F19) — tied with the
others, achieved cheapest; S5/S7/S9 defeated or avoided cleanly; F18/F15 satisfied for free
(Starlark already owns it); lowest comprehension cost; fastest path to green. It is the right
choice *if razel's future is more languages + bigger graphs, single config, no overlays.*

**Where A1 clearly loses:** F12/C7 (config/transitions) — it does not just defer the feature,
it bakes single-config identity into the fact-store key type, making the eventual pivot a
contract migration rather than an additive change; F17 (orthogonal derivation) — structurally
absent, no seam to attach overlays; F11's typing — two-tier (privileged cc fields vs escape-map
user providers), the S3 seed. And it slightly under-budgets F7 parallelism by calling the
`!Sync`-Engine rewrite "commodity."

**The crux:** A1 is the right architecture *unless* multi-config, transitions, or orthogonal
derivation are coming — in which case its thinness is a deferred bill payable in the one
currency (data-contract migration) the project's own framing (R4/S3/S6) names as most
expensive. A1 is a calibration detent on a single shared design, distinct mainly in *where it
stops*, and its stopping point is a deliberate, well-argued, conditionally-correct bet.
