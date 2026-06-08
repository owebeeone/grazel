# Architect Skill — how to design & refactor (for a future AI or human)

A repeatable method for architecture decisions in this codebase, plus the judgment
behind it. Distilled from real sessions; the failure modes here are ones we hit.

## When to invoke

- Adding a feature would bloat a hot file, or you can't tell *where* it should go.
- A module has become a god-object / gravity well.
- You're choosing between a quick hard-coded fix and a more general structure.
- You're planning a refactor, or scoring competing designs.

---

## Cardinal rule: generalize before you architect

**Do not architect against the ocean of features. Architect against the finite shape
underneath it.** There may be thousands of specific features, but they interact with
the engine in a *small, finite number of ways*. Find that shape first.

> If you find yourself enumerating features to design the structure, stop. You're at
> the wrong altitude.

---

## The method (in order — don't skip step 1)

**1. Distill the model before proposing any structure.**
Slice the problem at the joints where the ocean collapses to a handful. Produce:
- **Capabilities** — what the engine must be *able to do* (verbs).
- **Concepts** — the core entities/nouns the system is made of.
- **Interactions** — how those concepts compose and flow into each other.

Good slicing makes the architecture *fall out* and the right indirection *obvious*.
Bad slicing is what produces hard-coded dead ends. This step is the leverage.

**2. State the optimization target — and that it's a balance.**
Optimize *on balance* (no single winner) for:
- **a) Modularity / isolatability** — adding a feature is obvious *where* it goes and
  touches the *fewest* places; features don't bleed into each other.
- **b) Testability** — a common harness; small, focused, thorough tests per unit.
- **c) Calibrated extensibility** — room to grow along axes you can't name yet (below).

**3. Generate ≥3 options as a *spectrum of indirection investment*** — not arbitrary
layouts. Each option must state explicitly:
- *where it spends "relief valves"* (indirection) vs *where it stays hard-coded*, and
- its characteristic **dead-end risk** (too-rigid vs too-abstract).

**4. Score on a/b/c + dead-end risk; pick on balance; record *why*.**
For uncertain/irreversible choices, present options for discussion — don't pick silently.

---

## The hard part: calibrating extensibility (criterion c)

Extensibility is **not** predicting the future. It's deliberately spending *strategic
indirection* — a virtual call, trait, generic, macro, or at the extreme an embedded
interpreter (Starlark is the ultimate) — at the **seams most likely to move**, as
relief valves for directions you can't name yet.

Two **symmetric** dead ends:
- **Too rigid** — the "simplest pragmatic" hard-coded choice. *This is the default
  failure of AIs and many humans*, and it's what needs rework later. Reaching for the
  simplest is *often* right — but not as a reflex.
- **Too abstract** — generality nobody needs; ossifies, obscures, also a dead end.

There is **no provably-right amount** (it depends on undefinable futures). That's
*why* we lay out options and judge the trade-off rather than declare one answer. The
calibration is an art; name it, don't pretend it away.

Heuristic: add an unobvious level of indirection when (a) the seam plausibly moves
along an *orthogonal* axis (new language, backend, frontend, query kind…), and (b) the
indirection is cheap to introduce now and expensive to retrofit. Otherwise stay direct.

---

## Modularity target — "new feature = new file"

- Adding a feature should mean **dropping in a small file that registers itself**,
  with the smallest possible shared edit — ideally **one line in a data manifest**
  (config-as-data: the manifest *is* the legible surface).
- The core ("assembler") is **closed for modification, open for extension.**
- Design an organizing **metaphor** — a vocabulary of small composable bits with clear
  contracts — *not* a folder split. A folder split of a god-object just makes smaller
  god-objects.

---

## Force the path, don't just enable it (the YIDL test)

The strongest extensibility property is when **the easy path is the *forced* path** — the
imperative shortcut *doesn't compile*, so the agent/human is driven to the general,
declarative, composable solution (YIDL's discipline: it prohibits imperative code, so the
only path that works is the general one). *Enabling* the right path (good seams an agent
*can* bypass) is not enough — under pressure the bypass is taken, and that is exactly how
god-modules and ambient state grow (razel's `rules.rs`). Acceptance test:

> **An agent cannot extend the system imperatively and have it compile.** Extension points
> are closed, **pure-returning** contracts (`fn(args, &State) -> Declared`) with no reachable
> mutable global and no editable core; ambient state (thread-locals/statics) is
> **mechanically banned** (lint/CI deny). The only place to put logic is the declared return.

Note the boundary: where the *input language is mandated imperative* (e.g. consuming
Bazel's `rule()`/`ctx.actions`), you cannot force it — that surface becomes an
imperative→declarative **lowering adapter**, and the forcing applies to *your own*
authoring surface (cf. GrazelProposal G-G7, which prohibits imperative descriptors).

---

## Compose soundly, or bound it hard

If an extension can change the behavior of an **imported** model (a config/aspect/
derivation/override matching another module's facts), that is the deepest hazard and the
most likely *too-abstract* dead-end. Get it right with a **bounded discipline, not a general
logic engine**:
- **Additive-by-default** (monotone fact-addition → confluent, order-independent, terminating).
- **Behavior-change only via explicit, schema-declared merge at explicit precedence scopes** —
  never "last file wins", never silent *global* mutation (the importer composes its *view*;
  it does not mutate the shared model for other importers).
- **Confluence checked at *definition* time** (reject overlapping matchers with no declared
  resolution); start where it's cheap (Eq-decidable: `select`/toolchain).
- **Provenance mandatory** — action-at-a-distance is only debuggable if every derived/
  overridden fact records which matcher/file/match produced it.
- **Defer the general case** — arbitrary cross-module behavioral *override* is the frontier,
  not the v1 (Pants's 5-phase ambiguity solver is the warning).

*The two are linked:* the no-imperative forcing above is the **prerequisite** for sound
composition — you cannot reason about confluent cross-module behavior-change over
side-effecting units. Together they make "the easy path is the right path" hold *across*
modules, not just within one.

---

## Testability target — seams first

- **If a unit can't be unit-tested cheaply, the seam is wrong.** Fix the seam, not the
  test.
- Common harness; tests small/focused/thorough, co-located with the bit they test.
- Integration tests belong at the *top* of the pyramid, not as the only layer.
- **Coverage gaps are a symptom, not laziness** — usually ambient state or a missing
  seam. Also beware *environment-gated* tests (skip without a toolchain): they make
  coverage silently conditional.

---

## Smell catalog (detect → fix)

| Smell | Why it's bad | Fix |
|---|---|---|
| God-module / gravity well | every feature lands in one file → merge hotspot, no seams | additive metaphor; one file per bit + manifest |
| Ambient / global state (thread-locals, statics) | invisible in signatures, leaks across calls, **forbids concurrency/nesting** → kills flexibility | scoped, explicit, *passed* state (a session value) |
| Pragmatic-hardcoded reflex | dead end needing rework | calibrated relief valve at the moving seam |
| Duplication ("denormalized DB of code") | drift, double-maintenance | single source; generate/wire the rest |
| Only coarse / integration / env-gated tests | symptom of bad seams | make units isolatable, then unit-test them |

---

## Process discipline

- **Restate the problem crisply and get alignment *before* producing.** Let it be
  refined. (This doc exists because that loop paid off.)
- **Verify before asserting** — measure the code (LOC, state, coverage, consumers);
  never pattern-match a conclusion.
- **Options, not answers**, for uncertain/irreversible design forks.
- **Calibrate difficulty honestly** — don't inflate solved problems or hand-wave hard
  ones.

---

## Razel exemplar (the method applied)

- *Feature ocean*: every Bazel builtin/rule/attr/provider/flag.
- *Distilled shape*: a **Dialect** = Words (builtins), Nouns (values), Phrasebooks
  (rulesets), a **Session** (state), a Lexicon (pure helpers). Adding a feature = a new
  file + one manifest line; the assembler is closed.
- *Smell found*: `rules.rs` god-module + 7 thread-locals (ambient state) → untestable,
  unmergeable, blocks the parallel scheduler and two-phase analysis.
- *Calibrated indirection*: the Session via `eval.extra` is the relief valve that also
  *is* the analysis-engine's provider store — one move, two payoffs. The ultimate
  relief valve, Starlark itself, was already taken; the art was in the layer above it.

*Use this as the lens. If the architecture still feels arbitrary, you haven't distilled
the shape yet — go back to step 1.*
