# Architectural Analysis of Bazel (for razel)

Lens: `ArchitectSkillRules.md` — score on (a) modularity/isolatability, (b) testability,
(c) calibrated extensibility; distill the finite shape under the feature-ocean.

Source examined: `/Users/owebeeone/limbo/bazel-dev/bazel` (~46k commits;
`d62d21b378` HEAD, a 10.0.0-pre line). Paths below are relative to that root.

---

## 1. Architecture overview — phases, layers, core abstractions

Bazel is a **3-phase pipeline over a single incremental dependency graph (Skyframe)**.
The phases are *conceptual ordering of node types*, not separate executables — they all
run as `SkyFunction`s in one graph.

### The Starlark interpreter (the foundation seam)
- `src/main/java/net/starlark/java/{eval,syntax,annot,lib,spelling,cmd}` — a **standalone
  Starlark interpreter** with **zero** imports of `com.google.devtools.build.lib`
  (verified: `grep -rn "import com.google.devtools.build.lib" src/main/java/net/starlark/java/` → 0).
  This is the single most important architectural fact: the language engine is a clean,
  reusable library; Bazel-the-build-tool is a *host* that registers globals/builtins into it.
- Embedding seam: `@StarlarkMethod` / `@StarlarkBuiltin` annotations (`net/starlark/java/annot`)
  expose Java methods as Starlark callables. Build-tool API surface lives behind **interface
  packages** (`lib/starlarkbuildapi/*` — `RuleFunctionApi`, `StarlarkActionFactoryApi`,
  `DefaultInfoApi`, …): the *Starlark-visible contract* is separated from the Java impl.

### Phase 1 — Loading (BUILD/.bzl → packages, targets, macros)
- `lib/packages/` — `Package`, `Rule`, `RuleClass` (2.4k LOC), `Attribute` (2.5k LOC),
  `Provider`, `StarlarkInfo`, `RuleFunction`, `MacroClass`.
- SkyFunctions: `skyframe/PackageFunction.java` (evaluate a BUILD file → `Package`),
  `skyframe/BzlLoadFunction.java` (load+exec a `.bzl`), `skyframe/StarlarkBuiltinsFunction.java`
  (inject the `builtins_bzl` overlay).
- Output: a `Package` of unconfigured `Target`s. No configuration, no actions yet.

### Phase 2 — Analysis (targets × configuration → ConfiguredTargets + Actions)
- `lib/analysis/` — `ConfiguredTarget`, `ConfiguredTargetFactory`, `RuleContext`,
  `RuleConfiguredTargetBuilder`, `ConfiguredRuleClassProvider` (the native rule registry).
- The rule-implementation→graph bridge: `lib/analysis/starlark/StarlarkRuleContext.java`
  (1.3k LOC — the `ctx` object) and `StarlarkRuleConfiguredTargetUtil.java` (calls
  `rule.implementation(ctx)`, collects returned providers).
- SkyFunction: `skyframe/ConfiguredTargetFunction.java` (`compute(SkyKey, Environment)` →
  `ConfiguredTargetValue`). A `SkyKey` here is **(Label, BuildConfiguration)** — the same
  target under two configs is two graph nodes.
- Configuration: `lib/analysis/config/` — `BuildConfigurationValue`, `BuildOptions`,
  `Fragment` (typed slice of config, `StarlarkValue`), `FragmentRegistry`,
  `ConfigMatchingProvider` (powers `select()`), `StarlarkDefinedConfigTransition`.
- Output: an action graph (`lib/actions/`), still not executed.

### Phase 3 — Execution
- SkyFunction: `skyframe/ActionExecutionFunction.java` runs a `Spawn` via a strategy
  (`lib/exec/`, `lib/sandbox/`, `lib/remote/`, `lib/worker/`, `lib/dynamic/`). The action
  abstraction is backend-agnostic: local / sandbox / remote / persistent-worker are
  pluggable spawn strategies behind one interface.

### Skyframe — the substrate under all three phases
- `src/main/java/com/google/devtools/build/skyframe/SkyFunction.java`: each node type
  implements `SkyValue compute(SkyKey key, Environment env)`. **The restart/build-up-dependencies
  pattern is the core idiom**: a function requests deps via `env.getValue(...)`; if any are
  missing it returns `null` and Bazel re-invokes `compute` after computing them. This makes
  dependency discovery *dynamic and demand-driven* rather than statically declared — the key
  enabler of incrementality, parallelism, and aspects.
- Incrementality falls out: change a node, invalidate its reverse-deps transitively, recompute
  only the dirty subgraph.

### External deps (a parallel loading sub-graph)
- `lib/bazel/bzlmod/` — `BazelDepGraphFunction`, `BazelModuleResolutionFunction`,
  `ModuleExtension`, `IndexRegistry`, `BazelLockFileFunction`. `MODULE.bazel` + registry +
  lockfile replace the old imperative `WORKSPACE`.

---

## 2. Distilled shape (the finite model under the feature-ocean)

The thousands of rules/attrs/providers/flags collapse to a small set.

### Capabilities (verbs the engine must do)
1. **Parse & evaluate** a sandboxed config language (Starlark) deterministically.
2. **Register & instantiate** rule classes from declarations (`rule(impl, attrs=…)`).
3. **Resolve a labeled dependency graph** over targets (loading), demand-driven.
4. **Configure** targets: parameterize a target by a `BuildConfiguration`; apply
   **transitions** along edges; resolve **`select()`** against config conditions.
5. **Analyze**: run each rule's impl over a `ctx`, producing **providers** (typed outputs)
   and **actions** (work to do).
6. **Resolve toolchains/platforms** for actions.
7. **Execute** actions via a pluggable spawn strategy; cache by content hash.
8. **Incrementally invalidate & recompute** on any input change.
9. **Aspect**: re-traverse the resolved graph applying a side-computation that injects
   extra providers/actions without editing the rules.

### Concepts (nouns)
- **Label** (`//pkg:target`), **Package**, **Target**.
- **RuleClass** (the *type*: name + attribute schema + impl fn) vs **Rule** (an instance/BUILD
  declaration). Mirrors razel's Word vs Noun.
- **Attribute** (typed, possibly configurable via `select`, possibly a dep edge with a transition).
- **Provider** (`Provider` = constructor/type-id; `Info`/`StarlarkInfo` = instance) — the *only*
  legal way one target hands typed data to another. The dependency-graph data contract.
- **ConfiguredTarget** = (Target × Configuration) → its providers.
- **Configuration / Fragment / Transition** — the parameterization axis.
- **Action / Spawn** — unit of execution; **Artifact** — a file in the graph.
- **Toolchain / Platform / constraint** — late-bound, resolved deps.
- **Aspect** — a graph-traversal overlay.
- **Module / ModuleExtension / Repo** — external-dep universe.
- **SkyKey / SkyValue / SkyFunction** — the graph substrate everything is expressed in.

### Interactions (how they flow)
```
.bzl: rule(impl, attrs={...}, provides=[...])  → RuleClass (registered)
BUILD: my_rule(name, deps=...)                 → Rule (Target)
                       │ loading
                       ▼
(Target, Config) ── transitions/select ──▶ SkyKey ──▶ ConfiguredTargetFunction
                       │ analysis: ctx = providers-of-deps + attrs + toolchains
                       ▼   impl(ctx) →
        returns [Provider…]  +  ctx.actions.run(...) registered
                       │ execution
                       ▼
        ActionExecutionFunction → Spawn (local/remote/sandbox/worker)
```
Providers are the *consumer-doesn't-know-producer* seam: a rule depends on
`CcInfo`/`JavaInfo`, never on the rule that produced it. Same discipline razel wants.

---

## 3. Extension model — where indirection lives vs hard-coding

Bazel spends its "strategic indirection" deliberately and unevenly. The big relief valves:

### (i) The embedded interpreter — the ultimate relief valve
`rule()`, `attr.*`, `provider()`, `aspect()`, `ctx.actions.*` are all just Starlark-callable
Java builtins (`lib/analysis/starlark/StarlarkRuleClassFunctions.java`,
`StarlarkAttrModule.java`, `StarlarkActionFactory.java`). **A new rule or even a new
*language* is added entirely in Starlark, in an external repo, with zero Java edits.** This
is the dominant extension axis and it is maximally calibrated: the core is genuinely closed
for modification.

### (ii) Providers — open data contract
`StarlarkProvider` lets rule authors declare new typed inter-target contracts in Starlark
(`provider(fields=…)`). No core change to add a new kind of cross-target data.

### (iii) ConfiguredRuleClassProvider.RuleSet — the native-rule registry
`lib/analysis/ConfiguredRuleClassProvider.java`: `interface RuleSet { configure(Builder) }`,
`addRuleDefinition(RuleDefinition)`. Native rules self-register into a builder — close to the
"new file + one manifest line" ideal — but this is the *legacy* path being actively retired
(see §5).

### (iv) Fragment / FragmentRegistry — config extension
A new configuration dimension = a new `Fragment` subclass (`lib/analysis/config/Fragment.java`)
registered in `FragmentRegistry`. Rules declare which fragments they need; the analysis only
materializes those. Pluggable config axis.

### (v) Spawn strategies / repository rules / module extensions
Execution backends, fetchers, and external-dep generators are all interface-pluggable
(`lib/exec`, `lib/repository`, `lib/bazel/bzlmod/ModuleExtension`).

### (vi) Aspects — orthogonal traversal
`skyframe/AspectFunction.java` + `lib/packages/StarlarkDefinedAspect.java`: add cross-cutting
computation (IDE info, lint, codegen) over an existing graph without touching any rule. A pure
relief valve for an axis ("I need extra info from the whole transitive closure") that rules
themselves can't express.

### Where it stays hard-coded (correctly)
- `Label`, `Package`, the Skyframe restart protocol, the loading/analysis/execution *ordering*,
  the provider-as-only-data-channel rule — these are the spine and are intentionally rigid.
- Attribute *kinds* (`attr.int/string/label/...` in `StarlarkAttrModule`) are a fixed,
  hard-coded enum-like set. New attribute *types* require Java. This is a calibrated stop:
  the set rarely moves and a fully-generic attribute type system would ossify the type checker.

---

## 4. Shortcomings & dead ends (evidenced)

### `RuleClass` / `Attribute` god-objects
`packages/RuleClass.java` (2424 LOC) and `Attribute.java` (2493 LOC), plus
`StarlarkRuleClassFunctions.java` (2283 LOC) with a `rule()` builtin taking **22 parameters**
(`StarlarkRuleClassFunctions.java:545-568`). The accreted optionality (`analysisTest`,
`buildSetting`, `initializer`, `parent`, `extendable`, `subrules`, `materializerRule`,
deprecated `outputToGenfiles`/`implicitOutputs`/`hostFragments`) is the textbook "every feature
lands in one file → merge hotspot." Each new rule *capability* widened this signature rather
than dropping in a new unit. **Lesson for razel: the `rule()` constructor surface is a gravity
well; budget for it.**

### Configuration explosion / transitions
`(Target × Config)` keying means a target analyzed under N configs is N nodes. Transitions
(`StarlarkDefinedConfigTransition`, `FunctionTransitionUtil`) were retrofitted and remain a
known correctness/perf minefield (analysis-graph blowup). The `select()` machinery
(`ConfigMatchingProvider`, `ConfiguredAttributeMapper`, plus a half-dozen `*AttributeMapper`
variants in `lib/packages/`) is intricate because configurability was layered onto an
attribute model that started non-configurable.

### `incompatible_*` flag churn — the migration-debt signal
`git log --grep="incompatible_"` → **1092 commits**. Each `--incompatible_*` flag is a guarded,
reversible behavior change used to slow-walk a breaking architectural fix through the ecosystem
(e.g. `--incompatible_disable_target_provider_fields`, `--incompatible_no_implicit_file_export`,
`--incompatible_require_mnemonic_for_run_actions`). The *volume* is the tell: a large fraction
of Bazel's evolution is paying down hard-coded early decisions one guarded flip at a time. Many
graduate to no-ops (`git log --grep="no-op\|NOOP"`), i.e. the dead end is finally sealed.

### Legacy provider model (struct providers → declared providers)
Original rules returned untyped `struct(...)` providers; the typed **declared-provider** model
(`Provider`/`Info`, 2017+) replaced it, but the legacy path lingered for years behind flags
(`--incompatible_disable_target_provider_fields`, now a no-op:
`git show 4fcbdb4a09`). Evidence that an under-typed data contract chosen early is extremely
expensive to retype later.

### WORKSPACE → bzlmod
The imperative `WORKSPACE` model was a dead end (ordering-dependent, non-hermetic, no version
resolution). It took a full parallel subsystem (`lib/bazel/bzlmod/`, ~80 files) and years of
deprecation messaging (`git show dc162753b2 2e4cc26c63`) to replace; `--enable_bzlmod` /
`--enable_workspace` are now both no-ops (`37e0d23951`). **Lesson: model external-dep resolution
declaratively from day one.**

---

## 5. Major refactors (from git) — the pivots and what they teach

### A. Native rules → Starlark (`builtins_bzl`) — then fully *externalized*
The headline migration. Native Java rules were re-implemented in Starlark under
`src/main/starlark/builtins_bzl/` (injected via `StarlarkBuiltinsFunction`), then moved out of
Bazel entirely into external repos (`rules_cc`, `rules_java`, `rules_python`, …).
**Evidence that it finished, in this very checkout:**
- `builtins_bzl/common/exports.bzl`: `exported_rules = {}` — empty.
- `builtins_bzl/bazel/exports.bzl`: `cc_binary`, `cc_library`, `cc_test`, `objc_library`, …
  are mapped to `_removed_rule_failure`, which `fail()`s telling users to `load()` from
  `rules_cc`. The `rules/cc/` and `rules/cc_library.bzl` Java/Starlark sources are **gone**
  from the tree (`find . -name cc_library.bzl` → nothing).
- The autoloading bridge that transparently mapped bare `cc_library` to the external repo was
  itself removed: `a3dc34c545 "Remove AutoloadSymbols and deprecate related flags"`,
  `0522f8e58b "Disable autoloads for Python, Java, Proto, Shell and Android"`,
  `eb1b3fc5f4 "Remove rules_cc from autoloads"`, `d87eaf5d6f` flips
  `--incompatible_disable_autoloads_in_main_repo`.
- The Starlarkification was a multi-year grind (`git log --grep="Starlarkify" -i` → hundreds of
  commits, incl. internal helpers like `CppModuleMap`, `LibraryToLinkValues`, test migrations).

**What it solved:** the imperative, hard-to-extend native-rule code (the exact pain motivating
razel) was eliminated; rulesets now version and release independently of the Bazel binary. Java
shrank to *primitives* exposed via `cc_common`/`java_common` (the `cc_common.bzl` wrapper +
`rules/cpp` helpers remain), while *rule shape* lives in Starlark.
**What it teaches razel:** the engine should ship almost no rules. The native surface should be
a thin set of primitives (actions, providers, toolchain resolution); everything composable lives
in the dialect. Bazel proved this is achievable *and* that delaying it costs years.

### B. Skyframe — imperative phases → one incremental graph
Replaced the original phase-coded, full-rebuild loading/analysis with the demand-driven
`SkyFunction` graph (`git log --grep=Skyframe` → 1116 commits). Unlocked fine-grained
incrementality, parallelism, and made aspects/transitions even expressible.
**Teaches:** model the build as **one uniform node graph with a restart/demand protocol**, not
hand-coded phases. Phases become node *types*, not control flow.

### C. WORKSPACE → bzlmod (default in Bazel 7, `51f71f1a0f`, 2023-12; now mandatory)
Declarative module graph + registry + lockfile replaces imperative repo setup (§4).

### D. Declared providers (typed) replacing `struct` providers (§4).

### E. Aspects added incrementally (`git log --grep=aspect`: from early
`fdd788e15f "Add support of aspects to the skyframe implementation of query"` onward) — slotted
in *because* Skyframe + providers made an orthogonal traversal expressible without reworking rules.
**Teaches:** good seams (graph + provider contract) let a genuinely new capability be added as an
overlay, not a rewrite.

---

## 6. Fundamental architectural requirements (the distilled lessons)

What any Bazel-compatible build tool — including **razel** — must get right, scored against
modularity (a) / testability (b) / calibrated-extensibility (c).

1. **Embed a sandboxed config language as the primary extension axis; keep it decoupled (c, b).**
   Bazel's `net.starlark.java` has *zero* dependence on the build lib and is tested in total
   isolation (`net/starlark/java/eval/*Test.java`, `ScriptTest` driving `testdata/*.star`). This
   is the single biggest leverage point: rules/languages are added in the language, not the host.
   *razel already has this (Starlark interpreter). The discipline to preserve: the interpreter
   must not import build-tool types; the build API is registered into it via a builtins manifest.*

2. **Ship primitives, not rules. Make the rule the unit of pluggability (a, c).**
   Bazel's endgame is `exported_rules = {}`: the engine provides actions, providers, toolchain
   resolution, and a `rule()`/`attr.*`/`ctx` vocabulary; *all* concrete rules live in external,
   independently-versioned dialect files. razel's "new feature = new file + one manifest line"
   maps exactly onto this. **Do not bake `cc_library` into the engine** — Bazel spent years
   undoing precisely that.

3. **One uniform, demand-driven dependency graph; phases are node types, not code paths (a, b).**
   The Skyframe restart protocol (`compute` returns null until deps ready) is what buys
   incrementality, parallelism, and the ability to bolt on aspects. Express loading/analysis/
   execution as node kinds over one graph keyed by content/identity.

4. **Providers are the only inter-unit data contract — and they must be typed from day one (a).**
   Consumer never names producer; it names a provider. Bazel's struct→declared-provider migration
   is a cautionary tale: an under-typed early contract is one of the most expensive things to
   retype. razel's Nouns/values should carry typed provider identity from the start.

5. **Make configuration, `select`, and transitions first-class in the attribute model — early (c).**
   Bazel bolted configurability onto a non-configurable attribute model, yielding a thicket of
   `*AttributeMapper` classes and transition fragility. If razel will support configurable
   attributes, the attribute *is* a `select`-able, transition-aware edge from inception.

6. **Resist the `rule()` god-constructor (a).** Bazel's `rule()` grew to 22 params and 2.4k-LOC
   `RuleClass`. Treat rule *capabilities* (test/build-setting/transition/extendable/aspect-host)
   as composable, separately-testable bits — not flags accreting on one signature/file. This is
   the razel `rules.rs` god-module smell, validated at scale.

7. **No ambient/global state in the evaluation core (b, a).** Bazel threads config and state
   explicitly through `SkyKey`/`Environment`/`RuleContext`; this is what permits the parallel
   scheduler and analyzing one target under many configs concurrently. razel's Session-via-eval.extra
   is the right instinct; the rule is: state is *passed*, never thread-local/static.

8. **A common, layered test harness with cheap unit isolation (b).** Bazel has the interpreter
   tested standalone (`ScriptTest`/`.star` fixtures), and a shared analysis harness
   (`BuildViewTestCase`, `AnalysisTestCase`, `FoundationTestCase`) with a `scratch` in-memory
   filesystem so a rule can be analyzed without execution. Note the *cost* signal: 2.5k-LOC test
   bases and a heavy lean on coarse analysis-level tests — a reminder that if the unit needs a
   2k-line harness to test, the seam is too coarse. razel should keep dialect-level units
   (Word/Noun/Phrasebook) unit-testable against a tiny session, with integration tests at the top.

9. **Model external-dependency resolution declaratively (c).** WORKSPACE's imperative model was a
   dead end that took a whole parallel subsystem (bzlmod) and years to replace. Version resolution
   + lockfile + registry should be declarative from the outset.

10. **Use guarded, reversible flips for breaking changes — but treat their volume as debt (b).**
    1092 `incompatible_*` commits are how Bazel migrates an ecosystem safely; they're also the
    fever chart of early hard-coding. The lesson is not "add many flags" — it's "the
    architectural decisions you hard-code now you will pay down one guarded flag at a time, so
    spend the calibrated indirection (1–6) up front."

**Bottom line for razel:** Bazel's whole 46k-commit trajectory bends toward *less native, more
dialect* — empty `exported_rules`, externalized `rules_cc`, removed autoloads. razel's distilled
shape (Dialect = Words/Nouns/Phrasebooks over a Session, assembler closed for modification) is the
same destination Bazel spent a decade migrating toward. The advantage of building it new is
landing on requirements 2, 4, 5, and 9 *from day one* instead of retrofitting them behind a
thousand incompatible flags.
