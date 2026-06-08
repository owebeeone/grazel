# Build-Tool Fundamental Requirements (problem-derived, mechanism-free)

The properties and capabilities a build tool **must** have, derived from the problem
itself — not from Bazel, Pants, or razel-as-built. The litmus for every line:

> *A from-scratch build tool, designed by someone who never saw Bazel or Pants, would
> still be forced into this to be correct / reproducible / incremental / parallel /
> extensible-at-scale.*

**Rules of this list:** every entry is a **property the tool guarantees** or a
**capability it has** — never a mechanism. "Embed an interpreter", "use providers",
"one graph", "a scoped session", "a registry" are *architecture choices* and live in the
Patterns catalog, not here. The three candidate architectures are scored **against this
list**; they differ in *how* they satisfy it.

**★** = architecturally load-bearing (this fundamental strongly dictates structure; the
architectures' main bets cluster here).

> This supersedes the mixed "§3 requirements" in `ArchModel.md`. The mechanism-laden
> material there becomes the separate **Patterns & Scars** evidence catalog (next).

---

## 1. Correct & reproducible

- **F1 — Deterministic description evaluation.** Evaluating the same build description
  over the same inputs always yields the same target graph. *Forcing: without it nothing
  downstream can be reproducible or cacheable.*
- **F2 ★ — Input-pure steps.** A build step's output is a function solely of its
  *declared* inputs (sources, tools, dependencies, configuration). Undeclared inputs must
  not affect it. *Forcing: this is the precondition for every form of caching, reuse, and
  parallelism — it's the deepest requirement and it dictates effect-isolation.*
- **F3 ★ — Declared-graph soundness (enforceable).** The tool can detect/forbid a step's
  use of inputs it didn't declare, so the dependency graph is the truth, not an
  approximation. *Forcing: F2 is worthless if it can't be enforced; silent undeclared
  inputs corrupt every cached/parallel result.*
- **F4 — Reproducible identity.** Outputs are a deterministic function of declared inputs,
  so a step's result is identifiable from its inputs without re-running it. *Forcing:
  "has this changed?", cache hits, and cross-machine reuse all require input-derived
  identity.*

## 2. Fast (the reason it exists, not a script runner)

- **F5 ★ — Incrementality.** After a change, only work affected by that change is redone;
  unaffected results are reused. *Forcing: this is the entire value proposition over
  re-running everything.*
- **F6 — Early cutoff.** If a step's output is unchanged despite an upstream input change,
  its downstream consumers are not redone. *Forcing: incrementality without early-cutoff
  still cascades a whole subtree on a no-op edit; real builds depend on the cutoff.*
- **F7 ★ — Parallel execution.** Independent steps run concurrently, bounded only by the
  dependency graph and available resources. *Forcing: serial builds don't scale to real
  repos.*
- **F8 — Reuse across builds and machines.** A result computed once is reusable in later
  builds and shareable across users/CI. *Forcing: team- and CI-scale economics; rests on
  F2/F4.*

## 3. The dependency model

- **F9 — Explicit dependency graph.** The build is a graph of steps/targets with declared
  edges that the tool resolves and traverses. *Forcing: ordering, parallelism, and
  incrementality are all impossible without the graph as first-class.*
- **F10 ★ — Demand-driven resolution.** Only the portion of the graph needed for the
  requested targets is loaded and analyzed. *Forcing: you cannot load/analyze a whole
  monorepo to build one target.*
- **F11 ★ — Producer–consumer decoupling.** A consumer depends on a dependency's
  **published output contract**, never on the identity of the rule/step that produced it;
  any producer satisfying the contract is substitutable. *Forcing: composition across
  thousands of targets and many teams is impossible if consumers name producers.*

## 4. Configurable & resolvable

- **F12 — Build-time configuration / variation.** The same target can be built under
  different configurations (platform, mode, options), and the description may legitimately
  vary by configuration. *Forcing: real builds are multi-platform / multi-mode.*
- **F13 — Toolchain & platform resolution.** Steps bind to the right tools for the
  target/execution platform by resolution, not by hard-wiring. *Forcing:
  cross-platform and cross-compilation.*
- **F14 — Reproducible external-dependency resolution.** Third-party code resolves to
  specific, reproducible versions. *Forcing: real projects depend on external code; ties
  to F4. (The scar: imperative/ordering-dependent resolution can't be hermetic.)*

## 5. Extensible by an open ecosystem

- **F15 ★ — Engine-closed extensibility.** New build logic — rules, languages, tools — is
  added by *users* without modifying the engine. *Forcing: the engine cannot enumerate
  every language/tool; the ecosystem is open-ended and versions independently of the
  engine.*
- **F16 ★ — Open-set composition.** A generic operation (build, test, lint, package)
  works over an open set of contributed implementations, and gains a new one without that
  operation being edited. *Forcing: "support a new language under `test`" must not require
  touching `test`.*
- **F17 ★ — Orthogonal derivation (razel's product surface).** New cross-cutting
  information (IDE/LSP index, affected-sets, lint, coverage, codegen, provenance) can be
  derived from the analyzed graph **without editing the rules that built it**, and *served
  to external consumers* (UI, MCP/agents, peer nodes). *Forcing: razel exists **for
  grip-lab** — a distributed human+AI IDE — where the graph is a live, queried derivation
  substrate, not a one-shot binary emitter. This is the **differentiator over Bazel**;
  producing binaries is one derivation among many. Promoted to ★: it is committed, not
  extended-core.*
- **F25 ★ — Sound cross-module compositional behavior-change.** An extension may *change*
  the behavior of an **imported** model (not just add to it) — a config/aspect/derivation/
  override that matches another module's facts — and the result MUST be **confluent**
  (order-independent), **scoped** (the importer's view, not silent global mutation),
  **terminating**, and **explainable** (every derived/overridden fact carries provenance).
  *Forcing: razel must consume Bazel's `select`/aspects/transitions/package-defaults (all
  behavior-change-at-a-distance), Model-G's descriptor composition (GrazelProposal §6), and
  F17 derivations over the distributed fact substrate — the same hard problem. This is the
  build tool's deepest correctness hazard and the most likely too-abstract dead-end (Pants's
  5-phase ambiguity solver). **Discipline (bounded, not a general logic engine):**
  additive-by-default (monotone → trivially confluent/terminating); behavior-change only via
  explicit schema-declared merge-classes at explicit precedence scopes (never "last file
  wins", never silent global mutation); confluence checked at **definition** time; provenance
  mandatory. **F18's no-imperative forcing is the prerequisite** — confluent composition over
  side-effecting units is impossible.*

## 6. Evaluation environment & analysis state

- **F18 ★ — Constrained, side-effect-free description evaluation.** The build description
  is evaluated in an environment that guarantees termination-friendly determinism and no
  uncontrolled I/O or nondeterminism. *Forcing: F1/F2 collapse if the description can do
  arbitrary I/O.*
- **F19 ★ — Concurrency-safe, partitionable analysis state.** The state produced while
  analyzing the description is per-evaluation and composable, permitting concurrent and
  nested analysis. *Forcing: F7 + F10 + F12 imply analyzing many targets/configs at once;
  a single global mutable analysis state forbids exactly that — **and F24 multiplies it**
  (many instances, in-process and across the mesh). (The razel scar: thread-locals; stated
  here only as the property they violate.)*

## 7. Interrogation & operation

- **F20 — Graph interrogation without building.** The tool answers structural questions —
  dependencies, reverse-dependencies, "what is affected by editing X", target sets —
  without executing actions. *Forcing: IDEs, CI test-selection, and tooling need the graph
  as data.*
- **F21 — Explainability / provenance.** The tool can explain *why* a step ran and *what*
  an output depends on. *Forcing: debuggability at scale.*
- **F22 — Execution-agnostic build logic.** Where and how a step runs (locally, isolated,
  remotely, persistently) is a selectable policy; the build logic does not encode it.
  *Forcing: one description must run across very different execution environments.*
- **F23 — Partial-failure policy.** The tool can either stop on first failure or continue
  past independent failures and report them together — caller's choice. *Forcing:
  usability at scale.*

## 8. Distribution & multi-instantiation (razel-as-server)

- **F24 ★ — Many graph instances, in-process and across machines.** The analysis graph is
  a first-class **value** instantiable many times within one process (per config/platform,
  per agent session) and **serialized/merged across machines** (the iroh p2p mesh of
  per-platform build nodes feeding MCPs + UI). *Forcing: cross-platform dev + a p2p
  derivation server mean N concurrent graphs and derived views shipped between nodes. Two
  hard consequences: (1) it **hardens F19** — a single global analysis state makes a
  multi-instance server impossible; (2) it **hardens F4/F11** — the inter-unit contract
  (providers/derived facts) must be **serializable data**, not in-process handles, because
  closures don't cross the mesh; facts do. razel already owns the serialization substrate
  (the taut IR → CBOR wire); this extends its purpose from daemon-RPC to the distributed
  fact substrate.*

---

## How this is used

- **These 25 are the scoring axes.** Each candidate architecture is judged on *how well
  and how cheaply* it satisfies each — especially the **★** ones (F2, F3, F5, F7, F10,
  F11, F15, F16, F17, F18, F19, F24, F25), which are where the architectures' real bets diverge.
  **F17 and F24 are razel's reason to exist** (the grip-lab derivation server); a design
  that aces F1–F16 but is weak on F17/F24 builds "another Bazel," not razel.
- **Distinct from the architecture-quality lens (a/b/c).** a) modularity, b) testability,
  c) calibrated-extensibility-of-*our*-codebase are how we judge an architecture's
  *construction*; F1–F23 are what the *product* must do. A design can satisfy all
  fundamentals and still be a bad architecture (god-module, untestable) — both lenses
  apply.
- **Mechanisms and dead-ends are demoted to evidence.** "Embed Starlark / providers /
  uniform graph / scoped session / open-set registry / declarative external deps" are the
  *menu* for satisfying these; "`incompatible_*` debt / `rule()` god-constructor /
  ambient-state gravity well / imperative repo setup" are the *warnings*. Both belong in
  the Patterns & Scars catalog, not in this list.

*List 1 of 3. Next: Patterns & Scars (the mechanism menu + warnings), then the three
candidate architectures scored against F1–F23.*
