# Patterns & Scars — the mechanism menu + the dead-ends

The **evidence**, demoted from requirements: *how* the Fundamentals (`ArchFundamentals.md`,
F1–F23) get satisfied (the menu the architectures pick from), and *what to avoid* (the
scars). Provenance: **[Bz]** Bazel, **[Pa]** Pants, **[Rz]** razel-as-built. Where a
pattern is a point on the **thin-dynamic ↔ typed-rich** dial (the axis the architectures
vary on), it's marked **◐ thin / ◑ mid / ● rich**.

Nothing here is a requirement. These are options and warnings.

---

## Part A — Pattern menu (by the fundamental served)

### P-A. Extensible evaluation — serves F15, F18, F1
- **◐ Embedded sandboxed interpreter.** [Bz Starlark; Rz has it] Rules/languages added in
  the language, in external files, zero engine edits. Max dynamism; weaker static checks.
- **● Typed host-language rules compiled to a graph.** [Pa `@rule`] No interpreter; logic
  *is* typed host code, validated by a compiler. Max static safety; two-language tax; no
  dynamic surface.
- **(clean-slate) Typed declarative data + minimal expression sub-language.** [thought-exp]
  Most of the surface is data; computation is a constrained slice, not a full interpreter.
- *razel note:* the interpreter is already chosen (F18 satisfied); the live question is the
  **layer above** it.

### P-B. Producer→consumer decoupling — serves F11, F4
- **◑ Typed declared providers.** [Bz `Provider`/`Info`] Consumer requests a provider
  *type*; any producer of it works. Explicit named contracts.
- **● Composition-by-return-type.** [Pa] A rule returning `T` is auto-eligible wherever `T`
  is needed. Automatic wiring; risks wrapper-type proliferation (S10).
- **◐ Implicit output bundle keyed by label.** [Rz today: DefaultInfo+hdrs+cflags] Weak —
  no typed third-party contracts; the thing F4/F11 want replaced.

### P-C. Dependency graph & incrementality — serves F5, F6, F7, F9, F10
- **● One uniform demand-driven graph + restart protocol.** [Bz Skyframe: `compute`
  returns null until deps ready] Incrementality, parallelism, and overlays (aspects) all
  fall out. The restart protocol is a discipline to write to.
- **● Two graphs: static rule-plan + runtime memo graph.** [Pa `rule_graph` + `graph`]
  Compile-time validation of the plan; node-granular memo/invalidation at runtime. Powerful;
  the static builder is an expert-only subsystem (S9).
- **◐ IR + straight deps-first execution loop.** [Rz live path] Simple; no incrementality
  on the live path. (Rz also has a `Engine` Skyframe-lite, but *off* the live path — S7-ish
  split-brain.)

### P-D. Effect quarantine & execution — serves F2, F3, F4, F8, F22
- **Pluggable spawn strategies behind one action interface.** [Bz local/sandbox/remote/
  worker] Execution policy ≠ build logic.
- **Small primitive kernel; everything above pure.** [Pa Rust intrinsics: fs/process/
  net/store] The only world-touching ops are fixed and content-addressed.
- **Content-addressed Action + sandbox (materialize/isolation).** [Rz `razel-exec`] Already
  good; this area *converges* across all three — quarantine I/O to a fixed kernel is the
  calibrated hard-code, not a dial.

### P-E. Analysis-state model — serves F19  *(the keystone area)*
- **State threaded via graph key + context.** [Bz `SkyKey`/`RuleContext`/`Environment`]
- **Scoped Session + typed Params; pure rules.** [Pa `session.rs`] The v1→v2 cure.
- **Passed value with interior mutability (scoped, plural).** [Rz proposed: `Session` via
  `eval.extra`, RefCell fields — forced by Starlark's `&mut Evaluator`-only builtin seam.]
- *All viable options share one property: state is **passed, not ambient** — that's F19.
  The dial here is null; the only scar is thread-locals (S1).*

### P-F. Open-set composition & registration — serves F16, F15
- **● Open-set dispatch.** [Pa `@union`/`UnionMembership` — consumer enumerates members]
  / [Bz providers + toolchain resolution + `RuleSet` self-register]. "Add a linter/language
  without editing the goal."
- **◑ Prefix/registry of contributed modules.** [Rz ruleset registry: `@repo//` → module]
- **Manifest form: explicit const-array** (chosen — legible, ordered, testable-subset) vs
  auto-registration (rejected; Rust makes you `mod` the file anyway, and we want
  ordering + subset assembly — see the inventory discussion).

### P-G. Configuration / variation — serves F12, F13
- **● Config as a graph axis.** [Bz `(Target × Configuration)` keying + `Fragment` +
  transitions + `select`/`config_setting` matching] Powerful; analysis-graph blowup + a
  thicket of attribute mappers if bolted on late (S6).
- **◑ Config as injected typed params.** [Pa `Subsystem` option bags + `parametrize`]
  Lighter; less of a separate axis.
- **◐ Pick-default `select`.** [Rz today] A stub, not real configurability.

### P-H. External-dependency resolution — serves F14
- **Declarative module graph + registry + lockfile.** [Bz bzlmod] Hermetic, versioned.
- **`@repo` → local-path mapping (no fetch).** [Rz candidate] Cheap way past the wall:
  point `@absl` at a checkout; defer fetching.
- *(scar S4: imperative `WORKSPACE`.)*

### P-I. Interrogation — serves F20, F21
- **Query/cquery/aquery over the resolved graph** [Bz] / **reverse edges + impact query**
  [Rz `affected`]. The IR carrying reverse edges is what makes this cheap.

### P-J. Test harness — serves (b)
- **Standalone interpreter tests + analysis harness with in-memory FS.** [Bz `ScriptTest`
  on `.star`; `BuildViewTestCase`/`scratch`] Note cost signal: 2.5k-LOC test bases.
- **Pure-rule unit tests via `run_rule`/`testutil`.** [Pa] Units isolatable because rules
  are pure typed fns.
- **Assert on captured pure data (`AnalyzedAction.argv`) + tiny-session unit tests.** [Rz
  candidate] Avoids the toolchain-gating scar (S11).

---

## Part B — Scars (dead-ends → the lesson)

| # | Dead end | Evidence | Lesson |
|---|---|---|---|
| **S1** | Ambient/global state → gravity well + blocks concurrency | Rz 8 thread-locals; Pa v1 stateful Tasks | state must be passed (F19); ambient state *causes* god-modules |
| **S2** | Native rules baked into the engine | Bz spent years → `exported_rules = {}` | ship primitives, not rules (F15) |
| **S3** | Under-typed early data contract | Bz `struct`→declared-provider migration | type the inter-unit contract from day one (F4) |
| **S4** | Imperative external-dep setup | Bz `WORKSPACE`→bzlmod, years + parallel subsystem | declarative external deps (F14) |
| **S5** | God-constructor / god-module | Bz `rule()` 22 params, `RuleClass`/`Attribute` 2.4k; Rz `rules.rs` 1658 | capabilities are composable bits, not flags-on-one-signature (a, F16) |
| **S6** | Configurability bolted on late | Bz `*AttributeMapper` thicket | decide the config model early (F12) |
| **S7** | Duplicated source of truth | Rz dead 2nd loader + 2nd `TargetDecl`/`Depset`; live `Engine` off-path | one loader, one target rep, one graph (R14) |
| **S8** | Two-language tax + bespoke dataflow compiler | Pa Rust engine + PyO3 + 5-phase monomorphizer | *over-engineering* caution: don't out-build the need |
| **S9** | Static-graph startup rigidity | Pa: missing/ambiguous rule = construction failure | the *too-rigid* end; validate seams but keep dynamism (F18) |
| **S10** | Wrapper-type proliferation (type-as-wiring) | Pa single-field frozen dataclasses | give value-types clear identity; don't overload common types |
| **S11** | Env-gated tests silently drop coverage | Bz/Pa toolchain skips; Rz ~28 `exists(){return}` | unit-test on pure data without a toolchain (b) |
| **S12** | Migration debt via guarded flips | Bz 1092 `incompatible_*` commits | spend calibrated indirection up front; flags are the bill |

---

## Part C — The dial map (the bridge to the architectures)

For each **★ load-bearing** fundamental, the mechanism choice spans the dial. **This table
is what the three candidate architectures stake out** — each picks a column (with mixing),
and that choice *is* its identity and its dead-end risk.

| ★ Fundamental | ◐ thin-dynamic | ◑ mid | ● typed-rich |
|---|---|---|---|
| F11 decoupling | label→bundle lookup | **typed providers** | composition-by-type |
| F5/F7/F10 graph | IR + straight loop | IR + **demand-driven Engine on live path** | static rule-plan + runtime memo |
| F12 config | pick-default select | **select+config_setting matching** | config-as-graph-axis + transitions |
| F16 extension | bare prefix registry | **registry + open-set dispatch (manifest)** | union-membership + typed plan |
| F18/F15 eval | thin builtins over Starlark | **dialect (Words/Nouns/Phrasebooks) + Session** | typed value graph above Starlark |
| F19 state | *(no dial — passed Session always; thread-locals = S1)* | | |

Invariant columns (not a dial — every architecture does the same): **P-D** effect
quarantine (content-addressed kernel), **P-E** passed-Session state, **P-J** pure-data
unit tests, and the clean **front-end→IR seam** (`ArchBazelConstraints.md`).

So the three architectures differ by *how far right on the dial* they commit — and the
cost is symmetric: too far ◐ = hard-coded dead ends (S5/S6/S3); too far ● = over-build
(S8/S9/S10). That calibration, per ★ fundamental, is the next deliverable.

*List 2 of 3. Next: three candidate architectures, each a column-selection across Part C,
scored against F1–F23 + the Constraints, with its dead-end risk named.*
