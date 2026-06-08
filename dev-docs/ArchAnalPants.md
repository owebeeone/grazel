# Architectural Analysis: Pants (for razel)

Analyzed through the lens of `ArchitectSkillRules.md`: (a) modularity/isolatability,
(b) testability, (c) calibrated extensibility — where Pants spent strategic indirection
vs. where it hard-coded. Pants source: `/Users/owebeeone/limbo/bazel-dev/pants` (single
squashed commit `95da181`; git-history mining unavailable, so rationale is drawn from
in-repo docs and code).

The headline: Pants is the *opposite* architectural bet from Bazel. Bazel spends its
top-tier relief valve on an **embedded interpreter** (Starlark) and a hard-coded native
analysis/execution core. Pants spends its top-tier relief valve on a **typed,
statically-solved rule graph** where the build logic itself is the extensible surface, and
hard-codes only a tiny primitive kernel (the Rust intrinsics). This is the single most
important contrast for razel.

---

## 1. Architecture overview

Three layers, with a hard language boundary between the bottom (Rust) and the top (Python).

### 1a. The Rust execution engine — `src/rust/engine/` (+ sibling crates in `src/rust/`)
A workspace of ~30 crates (`engine`, `graph`, `rule_graph`, `fs`, `store`,
`process_execution`, `options`, `watch`, `pantsd`, …). Compiled to a `cdylib`
(`Cargo.toml` `crate-type = ["cdylib"]`) and loaded into Python via PyO3. Key pieces:

- **`graph` crate** (`src/rust/graph/README.md`) — the *runtime* memoization graph. A
  generic `Node` trait; nodes are their own `Eq`/`Hash` identity (the memo key). Edges are
  data dependencies, used for fine-grained invalidation ("dirtying and cleaning"). Nodes
  run concurrently on `tokio`. In production the `Node` is an enum over rule-tasks +
  intrinsics (`src/rust/engine/src/nodes/`: `task.rs`, `execute_process.rs`,
  `snapshot.rs`, `scandir.rs`, `downloaded_file.rs`, …).
- **`rule_graph` crate** (`src/rust/rule_graph/`) — the *static* rule graph, built once at
  startup. Nodes are `Rule`s, `Query`s (roots = external callsites), `Param`s (leaves =
  inputs). See §5 for the construction algorithm — it is a real dataflow compiler.
- **`scheduler.rs` / `tasks.rs` / `context.rs` / `session.rs`** — `tasks.rs` registers the
  `Rule` definitions handed down from Python; `scheduler.rs` drives execution; `session.rs`
  holds per-run state (a *scoped*, explicit session object — not ambient globals).
- **`intrinsics/`** (`process.rs`, `digests.rs`, `dep_inference.rs`, `docker.rs`,
  `downloads.rs`, `interactive_process.rs`) — the **hard-coded primitive kernel**: rules
  implemented in Rust because they touch the world (filesystem, subprocess, network,
  content-addressed store). These are the only "world-touching" operations; everything
  above is pure.

### 1b. The rules model (Python) — `src/python/pants/engine/`
- **`rules.py`** (618 LOC) — the `@rule` decorator. A rule is a pure `async` coroutine
  mapping statically-typed params → a statically-typed output. The decorator extracts the
  return type, param types, and the set of `await`ed sub-rule calls ("awaitables", via
  `collect_awaitables` AST inspection in `internals/rule_visitor.py`) and packages a
  `TaskRule` shipped to the Rust `tasks.rs`.
- **`unions.py`** (154 LOC) — `@union` + `UnionRule` + `UnionMembership`. The polymorphism
  seam (§3). `UnionRule`/`UnionMembership` are themselves Rust-backed (re-exported from
  `native_engine`).
- **`target.py`** (2641 LOC) — `Target`, `Field`, `FieldSet`, `Subsystem` declarative
  data model. Targets are bags of typed `Field`s; `FieldSet` selects the subset a rule
  needs.
- **`intrinsics.py`** — thin `@rule` wrappers that forward to `native_engine.*`, surfacing
  the Rust kernel into the rule graph as ordinary awaitables (`create_digest`,
  `execute_process`, `merge_digests`, `download_file`, …).
- **`internals/`** — graph construction glue, BUILD-file parsing (`parser.py`,
  `mapper.py`), `parametrize.py`, `selectors.py` (the `Get`/`concurrently` machinery).

### 1c. Backends — `src/python/pants/backend/*`
**One directory per language/feature**: `python`, `go`, `java`, `scala`, `kotlin`, `cc`,
`rust`, `shell`, `docker`, `helm`, `k8s`, `terraform`, `javascript`, `typescript`, `sql`,
`cue`, `openapi`, … plus cross-cutting ones (`project_info`, `visibility`, `tools`,
`codegen`). 108 `register.py` manifest files; 333 files defining `@rule`s. Each backend is
a Python package whose `register.py` exposes `rules()` and `target_types()` hooks
(`docs/.../writing-plugins/overview.mdx`). A backend is activated purely by listing it in
`backend_packages` in `pants.toml` — config-as-data.

**Flow:** `pants.toml` selects backends → each `register.py` contributes `rules()` +
`target_types()` + `UnionRule`s → the union of all rules + the active `Query`s is compiled
into one static `RuleGraph` at startup → at request time the Rust `graph` executes/memoizes
nodes, calling back into Python coroutines and down into Rust intrinsics for I/O.

---

## 2. Distilled shape (Capabilities / Concepts / Interactions)

### Capabilities (verbs the engine must do)
- **Resolve** a typed request (`Query`) to a plan over rules — *statically*, before
  running anything ("fill in the blanks": find the rules that produce a needed type from
  available params).
- **Execute** rule coroutines concurrently, awaiting sub-results.
- **Memoize & invalidate** at node granularity (content-addressed; file-watch driven).
- **Touch the world** only through a fixed intrinsic set (fs, process, net, store).
- **Dispatch polymorphically** to plugin-provided implementations (unions).
- **Partition / batch** work units (the lint/fmt partitioner pattern).

### Concepts (nouns)
- **Rule** (`@rule`): pure typed coroutine = one build step. The unit of modularity.
- **Param / Query / DependencyKey**: the static graph vocabulary (input type, root
  request, edge).
- **Get / MultiGet / `await … implicitly()`**: in-body rule invocation; `implicitly()`
  asks the engine to fill missing params from scope.
- **Target / Field / FieldSet**: the declarative data model of what's in a BUILD file.
- **Subsystem**: a scoped bag of typed options (config), injected like any other param.
- **`@union` / `UnionRule` / `UnionMembership`**: the open-set polymorphism seam.
- **Intrinsic**: a Rust-implemented primitive rule (the I/O kernel).
- **Backend**: a directory + `register.py` manifest that bundles the above.

### Interactions
- Rules compose by **type**, not by name: a rule that returns `T` is automatically eligible
  wherever a `T` is needed and its params are in scope. The graph is the wiring.
- Plugins extend by **adding rules and `UnionRule`s**; consumers (core goals like `lint`,
  `fmt`, `test`) iterate `UnionMembership` to find all registered members and dispatch.
- World effects flow exclusively through intrinsics, keeping the entire upper graph pure ⇒
  cacheable & parallelizable for free.

This is a clean realization of the skill doc's "finite shape under the feature ocean":
thousands of language/tool features collapse to **Rule + Param + Union + Target/Field +
Subsystem + Intrinsic**.

---

## 3. Extension model — how a new backend/rule/language plugs in

This is Pants's strongest dimension. Worked example, the `cc` backend
(`src/python/pants/backend/cc/`):

1. **Declare data** — `target_types.py`: `CCSourceField(SingleSourceField)`,
   `CCSourceTarget(Target)` with `core_fields = (*COMMON_TARGET_FIELDS, …)`. New target
   types are just subclasses; they "still work with core Pants" because core rules consume
   them by `Field` type, not by knowing about CC.
2. **Write rules** — pure `@rule` coroutines (`dependency_inference/rules.py`,
   `util_rules/toolchain.py`, `subsystems/compiler.py`).
3. **Plug into a goal via a `UnionRule`** — the key seam. To make `clang-format` a formatter,
   `lint/clangformat/rules.py` defines `ClangFormatRequest(FmtTargetsRequest)` with a
   `field_set_type` whose `required_fields = (CCSourceField,)`, then registers
   `*ClangFormatRequest.rules()` (which yields `UnionRule(FmtTargetsRequest, …)`). The core
   `fmt` goal never imports CC; it iterates `UnionMembership` and dispatches. (Same pattern
   for `tailor`: `UnionRule(PutativeTargetsRequest, PutativeCCTargetsRequest)`.)
4. **Manifest** — `register.py` exports `rules()` (splatting each module's `rules()`) and
   `target_types()`. *That `register.py` is the legible config-as-data surface.*
5. **Activate** — add the package to `backend_packages` in `pants.toml`. No core edit.

So "new feature = new file(s) that register themselves," with the smallest shared edit being
**one line in `pants.toml`** — exactly the skill doc's modularity target. The core
"assembler" (engine + core goals) is **closed for modification, open for extension**.

**Where the indirection lives (relief valves), well-calibrated:**
- **The typed rule graph itself** is *the* strategic indirection — composition by type
  means consumers never name producers. (Bazel buys the same decoupling with *providers* +
  Starlark; Pants buys it with the static graph + Python types.)
- **`@union`/`UnionRule`** — the open-set dispatch: a core goal declares a `@union` base
  request; plugins register members; the consumer enumerates `UnionMembership`. This is the
  "new linter/formatter/codegen without touching the goal" seam.
- **`implicitly()` / `Get`** — lets a rule request a type dynamically mid-body without the
  caller threading every param; the graph resolves it.
- **Subsystems / Fields** — extensibility of *config* and *data shape* without touching
  parsing or option plumbing.
- **The Python/Rust boundary** — all extension happens in Python (cheap, hot-reload-ish);
  the perf-critical kernel stays in Rust. New languages never require Rust changes.
- **The daemon (`pantsd`)** — keeps the warm graph + filesystem watch resident; a relief
  valve for cold-start cost.

**Where it stays hard-coded (correctly):** the **intrinsics** — fs, process exec, digest
store, downloads, dep-inference, docker. The set of ways to *touch the world* is genuinely
finite and rarely-moving, so hard-coding it in Rust is calibrated, not a dead end. New
languages reuse `execute_process` + `Snapshot`; they don't invent new I/O primitives.

---

## 4. Shortcomings & dead ends

- **Two-language tax.** Every contributor must understand a Python rule API *and* a Rust
  engine + PyO3 FFI + a homegrown dataflow compiler. The `rule_graph` builder
  (`builder.rs`) is, by its own docs, *"homegrown (and likely problematic) node splitting
  on the call graph"* with a five-phase monomorphization — powerful but a steep, narrow
  expertise. razel's analogue (Rust + embedded Starlark) is arguably a simpler boundary
  because the extension language (Starlark) is off-the-shelf, not a bespoke typed-coroutine
  protocol.
- **Static rule graph is rigid by design.** If you request a `Query` with no rule path, the
  engine *fails at construction* rather than dynamically resolving (internal-arch doc:
  "the engine fails rather than attempting to dynamically determine which Rules to use").
  Ambiguity (two rules producing the same type with the same params in scope) is a hard
  error surfaced only in the `prune_edges` phase, which then *recurses to locate where the
  ambiguity was introduced* — i.e., the error machinery is complex precisely because the
  model is unforgiving. Great for correctness; hostile to ad-hoc/dynamic builds.
- **Type-as-wiring can be surprising.** Because rules compose by type, an accidentally
  shared return type wires unrelated rules together; disambiguation is via newtype wrappers
  (lots of single-field frozen dataclasses — the "denormalized DB of code" smell partly
  reappears as wrapper-type proliferation).
- **`implicitly()` ergonomics.** The doc concedes explicit rule params "must be passed
  positionally… we hope to support keyword arguments in the future," and `implicitly()`
  returns an untyped `dict[str, Any]` precisely because PEP-612 can't express it. The seam
  works but the typing leaks.
- **`target.py` is a 2641-LOC gravity well.** The Target/Field core has accreted into one
  large module — a mild instance of the god-module smell, though the *concepts* inside it
  are clean.
- **Plugin isolation caveats.** Docs warn in-repo plugins "should not depend on other
  in-repo code" and third-party deps "that perform side effects… will not work properly
  with the engine's caching model." Purity is enforced socially/by-convention at the plugin
  boundary, not mechanically — a latent footgun.

---

## 5. Major refactors — the v1 → v2 rewrite (the big lesson)

v1 Pants was imperative Python "Tasks": stateful task objects with manual ordering, ad-hoc
caching, coarse invalidation, and a global product/state bus — the classic god-object +
ambient-state failure the skill doc names. v2 was a **total architectural rewrite**: a Rust
engine + a graph of typed pure `@rule`s. Git history is squashed here, but the architecture
encodes the rationale, and the internal-arch doc states the goals explicitly.

**What the rewrite reveals about v1's dead ends, and the v2 cure:**
- **Ambient/global state → scoped Params + pure rules.** v2 rules are pure coroutines;
  per-run state is an explicit `Session` (`session.rs`), not thread-locals. This is the
  exact "scoped, explicit, passed state" fix from the smell catalog, and it is *what
  unlocks* safe concurrency, memoization, and remote execution. (Directly parallels the
  razel exemplar's finding: 7 thread-locals in `rules.rs` blocked the parallel scheduler.)
- **Manual dependency wiring → composition by type + static graph.** v1 hand-ordered tasks;
  v2 *derives* the plan. The internal-arch doc: build logic is declared as `@rules` with
  "recursively memoized and invalidated results," with stated kinship to **Bazel's Skyframe
  and the Salsa framework." The rule graph is computed at startup using
  **"live variable analysis and monomorphization"** (compiler techniques!) across five
  phases — `initial_polymorphic → live_param_labeled → monomorphize → prune_edges →
  finalize` (`rule_graph/src/builder.rs`) — so memo keys are minimal and invalidation is
  precise.
- **Coarse caching → content-addressed, node-granular memo + invalidation.** The runtime
  `graph` crate makes each `Node` its own identity/memo key with dirty/clean tracking;
  intrinsics are content-addressed (digests/store). This is why fine-grained caching,
  concurrency, and remote execution come "for free" to every plugin.
- **Imperative extension → declarative backends.** v1 plugins hooked imperative tasks; v2
  plugins drop a `register.py` of pure rules + `UnionRule`s.

**Teaching:** the rewrite is the proof that the v1 imperative/stateful model was a dead
end *for a build tool specifically*, because build tools live or die on caching +
parallelism + reproducibility, and those are unattainable on top of ambient state and
hand-wired ordering. The cure was not "more abstraction layers" but a **purity discipline
enforced by a typed graph**, with world-effects quarantined into a small Rust kernel.

---

## 6. Fundamental architectural requirements for a build tool

The distilled, transferable lessons — what razel's architecture *must* get right, with the
Pants-vs-Bazel contrast made explicit.

1. **Purity + quarantined effects is the load-bearing decision.** Caching, parallelism,
   reproducibility, and remote execution are *consequences* of a pure core where the only
   way to touch the world is a small, fixed, content-addressed primitive set (Pants
   intrinsics; Bazel actions/spawn). Hard-code the I/O kernel; keep everything above it
   pure. This is the one place hard-coding is *calibrated*, not a dead end.

2. **No ambient state — ever.** The v1→v2 rewrite *was substantially* the removal of global
   task state in favor of explicit, scoped session + Params. The razel exemplar already
   found the same smell (7 thread-locals in `rules.rs` blocking the parallel scheduler).
   This is non-negotiable for a build tool: ambient state forbids the concurrency and
   nesting the tool exists to provide.

3. **Pick exactly one top-tier relief valve for "logic you can't name yet," and invest
   heavily in it.** This is the calibrated-extensibility crux, and Pants vs. Bazel is the
   cleanest contrast:
   - **Bazel:** embedded interpreter (**Starlark**) as the extension surface; native core
     hard-coded. Maximal dynamism/expressiveness; weaker static guarantees; the "rules are
     untyped data + providers" model.
   - **Pants:** a **typed, statically-solved rule graph** as the extension surface; logic is
     ordinary typed Python composed by type, validated by a compiler at startup. Maximal
     static safety + automatic wiring/caching; rigid (missing/ambiguous rule = startup
     failure), and a heavier conceptual model.
   - **For razel:** Starlark is *already taken* as the language (the ultimate relief valve,
     per the skill doc). So the real calibration is the **layer above Starlark** — the
     Session/provider store / dialect assembler — exactly as the razel exemplar found.
     Pants shows what that layer looks like when pushed to its logical end: composition by
     typed value, open-set dispatch, and a declarative data model. razel should *steal the
     concepts* (typed values, open-set extension, config-as-data manifest) while keeping
     Starlark as the dynamic seam Pants lacks.

4. **Open-set dispatch is the modularity workhorse — provide it as a first-class seam.**
   Pants's `@union`/`UnionRule`/`UnionMembership` is the "add a linter/language/codegen
   without touching the consumer" mechanism. Bazel's equivalent is **providers + toolchain
   resolution + rule registration**. razel needs an explicit analogue: a way for a new
   unit to *register itself* against a published extension point, with consumers
   enumerating members — never importing producers. (razel's Phrasebooks/Words registry is
   this seam; keep it open-set and data-driven.)

5. **"New feature = new file + one manifest line."** Pants nails this: a backend is a
   directory + `register.py`, activated by one line in `pants.toml`. 108 backends, zero
   core edits to add the 109th. razel's target: a new Word/Noun/Phrasebook is a file that
   self-registers, with the manifest as the legible surface. Do **not** let the core become
   a god-module that every feature edits.

6. **Compose by typed value, not by name.** Pants rules wire by return/param type; Bazel
   targets wire by providers. Both decouple producer from consumer. The cost Pants pays —
   wrapper-type proliferation and surprising type collisions — is a real warning: give the
   value-types clear identity and avoid overloading common types as wiring channels.

7. **Static validation has a sharp edge — choose your rigidity deliberately.** Pants
   validates the whole rule graph at startup and *fails* on a missing/ambiguous path; the
   five-phase monomorphizing builder is the price. This is the **too-rigid** end of the
   spectrum: superb correctness, hostile to ad-hoc/dynamic builds, and an expert-only
   subsystem. Bazel sits more toward dynamic. razel, having Starlark, can afford a softer
   stance — but should still validate seams (e.g. unknown builtins/providers) eagerly
   rather than failing deep in execution.

8. **Testability follows from the seams.** Because Pants rules are pure typed functions
   with explicit params, units are isolatable; backends ship co-located `*_test.py`. The
   common harness is the engine's own `run_rule`/`testutil`. The lesson matches the skill
   doc: *if a unit can't be cheaply unit-tested, the seam is wrong.* The caveat to inherit:
   beware **environment-gated** tests (a CC/Go/JVM toolchain that, if absent, silently skips
   coverage) — exactly the "coverage silently conditional" smell. razel should keep the
   dialect/session layer testable without any real toolchain present (mock→real seam from
   day one, à la `tap.set('meteo'|'mock')`).

**One-line takeaway for razel:** Pants proves that a build tool's correctness/perf
properties fall out of *purity + quarantined I/O + composition-by-typed-value + open-set
self-registration*, and that the v1 imperative/ambient-state design was a dead end for
exactly the reasons in the skill doc's smell catalog. Where Pants invested its single
top-tier relief valve in a bespoke typed rule graph, razel already has Starlark for the
dynamic axis — so razel's design leverage is the **assembler/session layer above Starlark**,
borrowing Pants's *concepts* (unions, fields/subsystems, register.py manifests) without
inheriting its two-language tax or its startup-time rigidity.
