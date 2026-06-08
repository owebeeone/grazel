# Architectural Analysis — razel

*Independent review. Claims in `RazelStatus.md` / `RazelDialect.md` were verified
against source; where I disagree I say so and show the evidence. All LOC measured
(`wc -l` / `git numstat`) at working-tree `8b89e56`-ish state, 62 commits.*

---

## 1. Architecture overview

razel is a 13-crate Cargo workspace (`crates/*` + `xtask`). Measured `src` LOC:

| crate | src LOC | role | global state? |
|---|---:|---|---|
| razel-cli | 7986 | CLI; `bazel_flags.rs` (7245, generated-ish flag table) + `main.rs` (741) | — |
| **razel-loading** | **2451** | **Starlark eval → analyzed targets. `rules.rs` (1658) is the core.** | **7 thread-locals (rules.rs) + 1 (lib.rs)** |
| razel-build | 1454 | build driver: analyze→action→exec (`lib.rs` 1052, `incremental.rs` 402) | — |
| razel-daemon | 826 | warm-analysis RPC server (`rpc.rs` 517) | — |
| razel-wire | 704 | taut IR → `generated.rs` (270) + CBOR codec; drift-gated | — |
| razel-exec | 619 | sandbox + content-addressed action execution | — |
| razel-conformance | 566 | `.star` Starlark golden runner | — |
| razel-analysis | 384 | `Depset<T>` order semantics + `analyze`/`wire_to_ir` | — |
| razel-engine | 285 | Skyframe-lite incremental graph (`Engine`) | — |
| razel-core | 241 | `Digest`/`FileId`/`TargetId` primitives | — |
| razel-ir | 215 | build graph IR (`Graph`, reverse edges) | — |
| razel-actions | 203 | content-addressed `Action` | — |
| razel-vfs | 132 | vfs | — |

**Live build path** (CLI → product):
`main.rs` → `razel_build::build_bazel_with` / `build_workspace_with`
→ `razel_loading::analyze_bazel_with` / `analyze_workspace_with` (= `rules.rs`,
thread-local state)
→ `razel_analysis::wire_to_ir(&[AnalyzedTarget])` (IR build)
→ `razel_build::execute` (sequential deps-first DFS, one `razel_exec::build_action`
per action).

**Warm/daemon path** is separate: `razel_build::IncrementalBuilder` wraps
`razel_engine::Engine`; the daemon holds a warm `Engine`. The plain CLI build does
**not** route through `Engine` — `execute()` is a straight loop
(`build/lib.rs:180-224`).

Core abstractions, by where they live:
- **`AnalyzedTarget` / `AnalyzedAction`** (`rules.rs:35-58`) — the real currency of the
  live path: name, deps, actions (argv/inputs/outputs), default_info, hdrs, cflags.
- **Starlark value types** (`rules.rs`): `Actions`, `Args`, `File`, `Depset`, `Ctx`,
  `RuleObjGen` — all in the god-module.
- **`Ruleset { prefix, module }`** (`rules.rs:1337`) + `ruleset_modules()`
  (`rules.rs:1390`) + `BzlLoader` (`rules.rs:1347`) — the one real extensibility seam
  (see §3).
- **`Engine`** (`engine/lib.rs:33`) — a *passed* value with interior `RefCell`s; clean,
  well-tested, the structural opposite of `rules.rs`.

---

## 2. Distilled shape (independent of the existing proposal)

**Capabilities (verbs the engine must do):**
1. **Parse + evaluate** a Starlark BUILD/`.bzl` module.
2. **Resolve `load()`** — to a project `.bzl` file (read+eval) or to a native ruleset
   shim keyed by `@repo//` prefix.
3. **Provide a vocabulary** of builtins (`glob`, `select`, `rule`, `attr`, `Label`,
   `native.*`, `depset`, `DefaultInfo`, package decls, stdlib).
4. **Run rule implementations** → capture actions + providers (analysis).
5. **Flow providers across targets** (a dependent reads its dep's outputs/hdrs/cflags).
6. **Order + execute** actions deps-first, content-addressed/cached/sandboxed.
7. **Answer graph queries** (rdeps/`affected`) without executing.
8. **Serve warm/incremental** builds (Engine, daemon).

**Concepts (nouns):**
- *Module/source* (BUILD or `.bzl`) and the **analysis run** over it.
- *Ruleset shim* (`@repo//` → synthetic FrozenModule of native rules).
- *Builtin/value type* (the Starlark vocabulary).
- *AnalyzedTarget* (the captured result) and its *Action*.
- *Provider* (today only the implicit DefaultInfo/hdrs/cflags bundle).
- *Analysis state* (targets-so-far, results-by-label, current pkg, workspace, flags) —
  **today this is 7 thread-locals, not a value.**

**Interactions:** an analysis run installs a globals environment + a loader, evaluates
the module; each builtin call mutates the ambient analysis state; rule instantiation
runs the impl in-scope and records an `AnalyzedTarget`; cross-target flow happens by a
builtin reading `RESULTS` keyed by canonical label; the driver then orders + executes.

The shape genuinely *does* collapse to a small set: **vocabulary pieces + a loader + an
analysis state threaded through them**. RazelDialect's Words/Nouns/Phrasebooks/Session
distillation is a faithful read of this shape (I checked it against the code, not the
doc) — see §4 for where I think it over/under-reaches.

---

## 3. Extension model — as built today

There are **two** extension stories, one decent and one a funnel.

**(a) Adding a native language ruleset — the one good seam.** Confirmed: `rust_rules.rs`
(196 LOC), `py_rules.rs` (195), `sh_rules.rs` (97) are each one file with a
`#[starlark_module]` and a `module() -> FrozenModule` that re-exports natives under the
names BUILD files `load()`. Wiring cost = **one row in `ruleset_modules()`**
(`rules.rs:1391-1422`), the closest thing to a manifest in the codebase. This seam was
deliberately introduced (commit `12dbdbe` "ruleset registry seam — generalize the
loader beyond @rules_cc"). It is the correct calibrated indirection.

*But the seam is half-built:* cc/skylib/autoconfig rule bodies still live **inline** in
`rules.rs` (`cc_rules` 897-1019, `skylib_rules` 1088, `auto_config_fns` 1193) — they
never got pushed out to files like rust/py/sh did. And the per-language files still
`use crate::rules::{record_target, resolve_dep, canon_label, qualify, unpack}`
(`rust_rules.rs:14`) — i.e. they reach back into the thread-local state through helper
calls, so they are not independently testable and have **no inline tests** (their tests
are toolchain-gated in `razel-build/tests/*`).

**(b) Adding a builtin / value / provider — the funnel.** Every other kind of feature
lands in `rules.rs`. `git numstat` on `rules.rs` is the smoking gun — representative
feature commits and the lines they bolted on:

```
+172  c984873  cpp-tutorial stage3 — multi-package loading
+168  07a29fc  build-graph + skylib builtins
+146  b522d23  ctx.actions.args() + File type
+114  658cae7  Label / native / auto-config helpers
+110  33b4144  attr namespace + ctx.outputs/files/executable
+103  eb5ecfb  copts/defines/includes on cc rules
 +94  f6a8dc8  depset + DefaultInfo accepts it
 +52  12dbdbe  ruleset registry seam
 +26  d512e23  package-declaration builtins
```

22 commits touch `rules.rs`; nearly every feature adds 20-170 lines to it. That is a
textbook gravity well (ArchitectSkillRules "God-module / gravity well"). The file holds:
all value types, all builtins (rule/cc/skylib/autoconfig/native/attr), the loader
(`BzlLoader`/`resolve_bzl`/`ruleset_modules`), **and** the orchestration
(`analyze_bazel`/`analyze_workspace`/`analyze_starlark`/`load_package`/`reset_analysis`).
A new builtin therefore = edit `rule_globals` (or add a new `#[starlark_module]`) **inside
rules.rs** + possibly a new thread-local + a reset edit in each of the 3 entry points.

RazelStatus's hotspot claim (`rules.rs`, second loader, `build/lib.rs`, `daemon/rpc.rs`)
is **confirmed**.

---

## 4. Shortcomings & dead ends (evidenced)

**4.1 The `rules.rs` god-module (1658 LOC, 4 tests).** Per §3. Its only tests are 4
coarse `analyze_starlark` round-trips at the tail (`rules.rs:1563-1660`). The pure
helpers that *should* be trivially unit-tested — `canon_label`, `qualify`, `pkg_of`,
`do_glob`, `walk_files`, `shquote`, `flatten_arg`, `extract_files`, `file_path`,
`unpack`, `define_flags`, `include_flags` — have **zero** direct unit tests. This is the
"coverage gap = bad seam" symptom: they can't be tested cheaply because they're buried
in a 1658-LOC module and several read thread-locals (`canon_label` reads `CURRENT_PKG`).

**4.2 Ambient global state — 8 thread-locals (RazelDialect says 7; there are 8).**
`rules.rs`: `STATE`, `CONFIGS`, `RESULTS`, `WORKSPACE`, `CURRENT_PKG`, `LOADED`, `GLOBAL`
(7, lines 66-101). **Plus** a separate `CTX` in `lib.rs:58` belonging to the second
loader. RazelDialect only counts the 7 in rules.rs and misses the 8th — minor, but the
8th matters because it's the orphaned-loader smell (4.3).

The state is ambient (invisible in signatures), global (one instance), and lifetime-less.
Concrete damage, all verified:
- **Manual, divergent resets.** `analyze_starlark` (1534) clears STATE/CONFIGS/RESULTS
  only; `analyze_bazel_with` (1470) calls `reset_analysis()` (which clears STATE/CONFIGS/
  RESULTS/LOADED) **then** sets CURRENT_PKG/WORKSPACE/GLOBAL and resets GLOBAL on the way
  out. Three entry points each hand-reset a different subset — exactly "leaks across calls
  unless every entry resets." This is fragile and a known bug surface.
- **No concurrency / no nesting.** One global STATE forbids two analyses at once →
  structurally blocks the parallel scheduler (P6) and a clean two-phase (P3). This is the
  real flexibility cost, correctly identified by RazelDialect.
- **`analyze_starlark` is a third, divergent globals build.** It rebuilds the globals
  inline (1542-1552) instead of calling `build_globals()` (1298), and omits the
  `native`/`attr` namespaces and the loader — so the "custom rule" path and the "real
  BUILD" path don't share a vocabulary. Duplication + drift risk.

**4.3 The orphaned second loader — worse than RazelStatus admits.** RazelStatus calls it
a hotspot; it's actually a **dead parallel pipeline**. `lib.rs` defines `TargetDecl`,
`load_build`, `query_targets`, its own `build_rules` (cc_library/binary/test/filegroup/
genrule/glob), and its own `CTX` thread-local — a complete second, simpler loader. It
feeds `razel_analysis::analyze(package, &[TargetDecl]) -> Analysis` and the `Depset<T>`
postorder/preorder machinery (`analysis/lib.rs`, `analysis/analysis.rs:48`).

I traced every caller: `razel_analysis::analyze` is called **only from its own test**
(`analysis.rs:193`). `load_build`/`query_targets` are called only from analysis's tests
and not from `razel-build`/`cli`/`daemon`. The live path uses `analyze_starlark` +
`wire_to_ir`, never `load_build`/`analyze`. So `lib.rs`'s loader, `TargetDecl`, the
`razel_analysis::analyze` half, and the `Depset<T>` order semantics are **dead on the
product path** — a denormalized second source of truth for "what is a target / how do
deps flatten," maintained in parallel with `rules.rs`. This is the "duplication =
denormalized DB of code" smell at architecture scale. Either it's a strangler-fig stump
that should be deleted, or it's where the real depset semantics are supposed to live and
the live path is the hack — either way the two should not both exist undocumented.

**4.4 Testability is coarse and toolchain-gated.** No shared razel test harness — each
crate uses raw `tempfile` + `#[test]` ad hoc (the only common harness is
`razel-conformance`, and that only runs `.star` *language* goldens, not razel's own
units). Worse, the meatiest tests skip silently without a toolchain: **~28
`Path::new("/usr/bin/cc"|"rustc"|"python3"|"/bin/sh").exists() { return; }` guards**
across `razel-build/src/lib.rs` (15+), `razel-cli/tests`, `razel-daemon/tests`,
`razel-exec`, `rust/py/sh` tests. On a CI box missing a compiler, the build pipeline's
coverage silently collapses to the 4 `rules.rs` round-trips + the loader unit tests.
ArchitectSkillRules flags env-gated tests explicitly: "they make coverage silently
conditional." Confirmed and significant.

**4.5 Smaller items.**
- `bazel_flags.rs` is 7245 LOC; it's a generated flag inventory (only 2 commits touch
  it), so it's *data*, not a god-module — fine, but it dominates the cli crate's size and
  shouldn't be read as hand-maintained.
- `select()` "picks default or first branch" (RazelStatus 🟡30) — real, a hard-coded
  dead end until config_setting lands; honestly labeled.
- `attr.*` descriptors are parsed then ignored (`attr_members` 1246) — the schema seam
  is a stub, which is why `ctx.attr` Targets / schema-driven deps are 0%.

### Does RazelDialect/Session actually fix these? (criterion-c calibration)

**Yes on the core, and the calibration is mostly right.** Verified the seam claim: the
*only* way to pass per-analysis state into a `starlark` builtin is `Evaluator::extra` /
`extra_mut` (the builtins receive `&mut Evaluator`). RazelDialect's reasoning that
`extra_mut` can't compose under nested eval (a rule impl calling `ctx.actions.run`
re-borrows) is correct, so a shared `&dyn` `Session` carried in `eval.extra` with
`RefCell` fields is the honest minimum, not a smell relapse — the smell was *ambient +
global + lifetime-less*, and a per-run, signature-visible, plural `Session` removes all
three. The double-payoff claim ("`Session.providers` *is* the AE store P1 needs") checks
out against the code: `RESULTS` is exactly that store today, just thread-local. So
killing the thread-locals and building the provider store are genuinely one move. That's
well-calibrated indirection at the seam most likely to move (concurrency, two-phase,
providers).

**Where it's right-sized:** the Phrasebook manifest just *formalizes* the
`ruleset_modules()` Vec that already exists and works — low-risk, additive. The Lexicon
extraction is pure upside (those helpers are the untested ones). The strangler migration
plan (§7 of the doc) is sound: Lexicon → Session → Nouns → Words → Rulesets, green at
each step.

**Where I'd push back / watch for over-engineering:**
- **The `nouns/`+`words/` split is 20+ files for a 1658-LOC body.** That's defensible
  (one-bit-per-file is the stated goal) but it trades a god-module for a wide shallow
  tree; the risk is *navigation cost* and `mod.rs` manifests that must stay in sync. For
  a tool with ~12 builtins and ~6 value types this is near the upper bound of useful
  decomposition — fine, but don't add a `dialect/` super-layer on top of it.
- **The `inventory::submit!` aside** (auto-registration) is correctly *rejected* in
  favor of the explicit const-array manifest. Good — auto-registration here would be
  abstraction nobody needs (the "too abstract" dead end). The doc names this; I agree.
- **It does NOT address the orphaned second loader (4.3).** RazelDialect is scoped to
  `rules.rs` and never mentions that `lib.rs`'s `load_build`/`TargetDecl`/
  `razel_analysis::analyze`/`Depset<T>` are a dead parallel pipeline. The refactor should
  *first* decide that pipeline's fate (delete, or make it the real depset home) — otherwise
  the new `nouns/depset_value.rs` becomes a *third* depset implementation. This is the
  one real gap in the proposal.
- **It does NOT fix the toolchain-gating directly.** The Session makes Words unit-testable
  (its main testability win, real), but the cc/rust/py/sh *rule bodies* still emit argv
  that only a real compiler validates. The fix there is asserting on the captured
  `AnalyzedAction.argv` (pure data) instead of running the tool — orthogonal to the
  Session and worth stating as its own testability rule.

Net: RazelDialect is **well-calibrated, not over-engineered**, on the state/vocabulary
axis; its blind spot is the dead second pipeline, which it should absorb into the plan.

---

## 5. Architecture evolution (from git)

62 commits, 58 prefixed `razel-ab` (one development arc), 1 `razel-ae` (analysis engine),
1 `razel-wire`, plus docs/chore. The arc, from the log:

1. **Foundation first (P0):** conformance harness + common IR + provider/VFS skeleton +
   a spike-gate. The *infrastructure* crates (engine, ir, actions, exec, wire, vfs,
   conformance) were laid down clean and have stayed clean — they're passed-value designs
   with co-located tests. This is the half of the codebase that obeys the rules.
2. **Then the loader accreted (the `razel-ab` middle).** `rule()` keystone (`48c865f`) →
   analysis → cc end-to-end → real BUILD files → multi-package → glob → custom `.bzl`
   macros → cc flags → ruleset seam → package builtins → skylib/build-graph →
   Label/native/autoconfig → stdlib → attr/ctx → args/File → depset. **Every one of these
   landed in `rules.rs`** (§3 numstat). The thread-locals were added incrementally as each
   capability needed ambient state, never refactored into a value.
3. **The `razel-ae` turn:** `f6a8dc8` (depset) is labeled the "first AE primitive" — the
   point where the author recognized the analysis-engine frontier needs a real
   provider store, which is what triggered RazelDialect's Session proposal.

**Gravity-well dynamics:** the loader started as "evaluate a BUILD with rule builtins"
(the simple `lib.rs` loader, which is *still there* — the orphan). When real-BUILD +
analysis demands arrived, a richer second loader (`rules.rs`) was grown alongside rather
than replacing the first, and from then on it was the path of least resistance — each
feature was "+1 builtin in the file I'm already in," and each needed "+1 thread-local
because that's how this file passes state." The thread-locals *caused* the god-module:
because state was ambient, there was no value to hang a new module off of, so everything
funneled back to where the statics live. Classic compounding: ambient state → no seam →
gravity well → more ambient state. The clean infra crates avoided it precisely because
they pass state (`Engine`, `Cache`, `Graph` are all values).

---

## 6. Fundamental architectural requirements (stated independently)

What razel's loading/analysis layer **must** satisfy, derived from the distilled shape
(§2), not from the proposal:

**(a) Modularity / isolatability**
- Analysis state must be a **value passed explicitly**, not ambient — because the shape
  has exactly one "analysis run" concept and multiple capabilities mutate it; if it's a
  value, each capability is a function of it and lives wherever it likes. This is the
  single highest-leverage requirement: it's what makes "new feature = new file" *possible*
  (you can't isolate a builtin while it secretly talks to 7 statics).
- A **builtin and a value type must each be addable as a self-contained unit** with a
  single shared touch-point (a manifest/registry row). The ruleset registry already
  proves this works for languages; the same discipline must extend to builtins/values.
- **One loader, one target representation.** The live and dead pipelines must be unified;
  there must be exactly one definition of "target" and one of "how deps flatten."

**(b) Testability**
- Pure helpers (label canon, glob match, flag synthesis, quoting, depset flatten) must be
  **directly unit-testable** with no Evaluator and no toolchain. This requires (a) — they
  can't read ambient state. A common, cheap harness for "feed a Word args + state, assert
  on state" is the unit-test surface; toolchain-gated end-to-end tests belong only at the
  top.
- Rule bodies must be testable on their **captured `AnalyzedAction` (pure data)**, not by
  invoking a real compiler. The ~28 `exists() { return; }` skips must become a small
  handful of genuinely-integration tests, not the de-facto coverage floor.

**(c) Calibrated extensibility**
- The relief valve belongs at the **two seams that demonstrably move**: (1) the
  per-analysis **state value** (it must become plural to admit concurrency/two-phase — an
  irreversible-if-skipped property, cheap now), and (2) the **load() → ruleset** registry
  (already invested, keep it as the manifest). Starlark itself is the embedded-interpreter
  relief valve and is already taken; the art is the thin layer above it.
- Stay **hard-coded** where the shape isn't moving: the action/exec contract, the IR, the
  CBOR wire (codegen already governs drift). Do **not** add an `inventory`-style
  auto-registration or a `dialect/` meta-layer — that's generality nobody needs for ~12
  builtins. The const-array manifest is the right altitude.

The architecture is sound below the loader and well-seamed at the ruleset registry; the
loading/analysis layer fails (a) and (b) today purely because state is ambient and the
target model is duplicated. Fix those two and the rest of RazelDialect falls out — which
is the doc's own (correct) thesis, with the one addition that the dead second pipeline
must be resolved in the same pass.
