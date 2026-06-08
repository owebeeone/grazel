# Razel Dialect — an additive architecture for the BUILD/.bzl surface

**Problem.** `rules.rs` is a god-module (1658 LOC) backed by 7 global thread-locals.
Every feature edits it → merge hotspot, no seams, and the global state makes units
untestable so coverage defaulted to coarse, toolchain-gated integration tests.

**Requirement (Gianni).** *New feature = new file.* Lots of small bits that compose.
A designed metaphor, not a folder split.

---

## 1. The metaphor

> razel understands a **Dialect** — the BUILD/`.bzl` language. The dialect is a
> **vocabulary** assembled from small, self-contained pieces. The engine folds the
> vocabulary into the Starlark environment and never needs to know any single piece.
> Meaning (analysis state) lives in a **Session** that is *handed to each word as it
> is spoken* — never a global.

Three kinds of vocabulary piece, plus the Session and a Lexicon of pure helpers:

| piece | metaphor | is a… | contract (one file each) |
|---|---|---|---|
| **Word** | a verb the dialect knows (`glob`, `depset`, `select`, `attr`, `Label`, `rule`, `provider`) | builtin global / namespace | `fn install(&mut GlobalsBuilder)` |
| **Noun** | a thing words produce/consume (`File`, `Depset`, `Args`, `Ctx`, `Target`, `Provider`) | `StarlarkValue` | the type + its methods |
| **Phrasebook** | a foreign vocabulary you `load()` (`@rules_cc`, `@rules_rust`, `@bazel_skylib`, `@local_config_*`) | ruleset module | `fn phrasebook(&Session) -> Ruleset` |
| **Session** | the meaning of *this* analysis | per-analyze state | the `Analysis` struct (replaces all thread-locals) |
| **Lexicon** | spelling rules | pure helpers (`canon_label`, `qualify`, `file_path`, glob-match) | plain functions, directly unit-tested |

The engine = a tiny **assembler** that folds Words → `Globals`, Phrasebooks → the
loader's ruleset map, and threads a fresh `Session` through each evaluation. That
assembler is the *only* core; it almost never changes.

---

## 2. The two manifests (the declarative wiring)

Adding a piece costs **one new file + one line in a data table** — the table *is* the
legible surface (config-as-data: "here is razel's vocabulary"), greppable, no macro
magic.

```rust
// dialect/mod.rs  — the vocabulary manifest
const WORDS: &[fn(&mut GlobalsBuilder)] = &[
    words::rule::install,
    words::providers::install,   // DefaultInfo, provider()
    words::depset::install,
    words::select::install,
    words::glob::install,
    words::package::install,
    words::graph::install,       // alias/filegroup/config_setting/test_suite
    words::label::install,
    words::native_ns::install,
    words::attr_ns::install,
    words::define_config::install,
    words::stdlib::install,      // print/map/filter/...
];

// rulesets/mod.rs — the phrasebook manifest
const PHRASEBOOKS: &[fn(&Session) -> Result<Ruleset, String>] = &[
    rulesets::cc::phrasebook,
    rulesets::rust::phrasebook,
    rulesets::py::phrasebook,
    rulesets::sh::phrasebook,
    rulesets::skylib::phrasebook,
    rulesets::autoconfig::phrasebook,
];
```

The assembler:
```rust
pub fn dialect() -> Globals {
    let mut g = GlobalsBuilder::extended_by(STDLIB_EXTENSIONS);
    for install in WORDS { install(&mut g); }   // each word teaches itself
    g.build()
}
```
(If we ever want *zero* central edits, swap the const arrays for `inventory::submit!`
auto-registration — but the manifest-as-data form is the declarative choice: it keeps
the vocabulary visible in one place. Recommend the manifest.)

---

## 3. The Session — first principle: **no thread-locals**

> **P0 — global thread-local state is banned.** It is ambient (invisible in
> signatures), global (one instance, leaks across calls unless every entry
> `reset`s), and lifetime-less — which makes the code hard to reason about *and*
> structurally forbids two analyses at once. That last point is the flexibility
> killer: it blocks the parallel scheduler (P6) and a clean two-phase (P3). Killing
> the thread-locals is a prerequisite, not cleanup.

Today's 7 thread-locals (`STATE`, `RESULTS`, `CONFIGS`, `WORKSPACE`, `CURRENT_PKG`,
`LOADED`, `GLOBAL`) become fields of one **`Analysis` session**, created per
`analyze_*` call and handed to each Word.

**The seam (and an honest constraint).** Starlark builtins only receive
`&mut Evaluator` — the *only* way to pass per-analysis state into a builtin is
`Evaluator::extra` (a `&dyn` shared ref) or `extra_mut` (`&mut`). `extra_mut` does
**not** compose under nested evaluation (a `rule` impl calls `ctx.actions.run`, which
re-borrows), so the composable choice is a `Session` carried by `eval.extra` with
**interior mutability (`RefCell` fields)**.

That `RefCell` is *not* the smell returning. The smell was *ambient + global +
lifetime-less*. The `Session` is **scoped** (one per `analyze_*`, lifetime-bound, no
`reset`, no leakage), **explicit** (it appears in signatures — `session(eval) ->
&Analysis`), and **plural** (many can coexist → concurrent package analysis, nested
loading/analysis phases, snapshot-for-incremental). The `RefCell` is a contained
detail forced by Starlark's `&`-only builtin seam, not a statement about *where*
state lives.

```rust
// analysis/session.rs
pub struct Analysis {
    pub workspace: Option<PathBuf>,
    pub current_pkg: Option<String>,
    pub global_flags: GlobalFlags,
    targets: RefCell<Vec<AnalyzedTarget>>,
    results: RefCell<BTreeMap<String, AnalyzedTarget>>,
    providers: RefCell<BTreeMap<String, ProviderSet>>,   // ← the AE provider store
    loaded: RefCell<HashSet<String>>,
}

/// Every Word reaches the session the same way — no globals.
pub fn session<'v>(eval: &Evaluator<'v, '_, '_>) -> &Analysis { /* downcast eval.extra */ }
```

A Word is then **a pure function of `(args, &Analysis)`**:
```rust
// words/depset.rs
pub fn install(g: &mut GlobalsBuilder) { g.with(module) }
#[starlark_module] fn module(b: &mut GlobalsBuilder) {
    fn depset<'v>(direct: Option<Value<'v>>, ... , eval: &mut Evaluator<'v,'_,'_>) -> ... {
        fold_depset(direct, transitive)          // ← pure core, unit-tested directly
    }
}
fn fold_depset(...) -> Vec<String> { ... }       // no eval, no globals
#[cfg(test)] mod tests { /* fold_depset(["a","b"], [["b","c"]]) == ["a","b","c"] */ }
```

Why this fixes both pains **and** is the AE:
- **Testable:** a Word's logic is `fn(args, &mut Analysis)`. Tests build a fresh
  `Analysis`, call the core, assert on it — **no Evaluator, no toolchain, no global
  state, order-independent.** Per-file `#[cfg(test)]` becomes the norm.
- **The AE provider flow is literally `Analysis.providers`** — the `'v`-scoped store I
  said P1 needs *is* the Session. So this refactor and P1 are the same move: do it once.
- **No hotspot:** P1 = add `providers` field + `words/providers.rs`; P2 = `nouns/target.rs`
  + schema in `words/attr_ns.rs`; P3 = two-phase in `analysis/`. Three different files.

---

## 4. File layout

```
razel-loading/src/
  lib.rs                 // public API (analyze_starlark/bazel/workspace) + re-exports
  assembler.rs           // dialect() + the loader wiring (the only "core")
  analysis/
    session.rs           // Analysis (state), session(eval)
    target.rs            // AnalyzedTarget, AnalyzedAction, ProviderSet
    orchestrate.rs       // analyze_*; (P3) two-phase load-then-analyze
  words/                 // one verb per file
    rule.rs providers.rs depset.rs select.rs glob.rs package.rs
    graph.rs label.rs native_ns.rs attr_ns.rs define_config.rs stdlib.rs
    mod.rs               // WORDS manifest
  nouns/                 // one value type per file
    ctx.rs actions.rs args.rs file.rs depset_value.rs rule_obj.rs target.rs provider.rs
  rulesets/              // one @repo per file
    cc.rs rust.rs py.rs sh.rs skylib.rs autoconfig.rs
    mod.rs               // PHRASEBOOKS manifest + Ruleset, BzlLoader
  lexicon/               // pure helpers, each with unit tests
    label.rs paths.rs glob_match.rs shquote.rs
```

Every directory is "lots of small bits"; every `mod.rs` is a manifest. A new builtin
touches `words/<x>.rs` + one line in `words/mod.rs`. A new ruleset: `rulesets/<x>.rs`
+ one line. A new value: `nouns/<x>.rs`. The assembler and Session don't move.

---

## 5. Worked example — adding `provider()` (P1) under this design

1. `nouns/provider.rs` — `ProviderId` (constructor, identity) + `ProviderInstance`
   (fields) value types. New file.
2. `words/providers.rs` — `provider()` word + `dep[Info]` indexing reads
   `session(eval).providers`. New file (or extend the providers word).
3. `analysis/session.rs` — add the `providers` field (the store). One field.
4. `nouns/target.rs` — `Target` carries its `ProviderSet`. New file.
5. Tests: `nouns/provider.rs` unit-tests construct+read; `analysis/session.rs`
   unit-tests producer-stores → consumer-reads — **no toolchain, no build**.

No edit to `rules.rs` (it's gone), no central churn beyond the field + manifest line.

---

## 6. Why this is enough (vs. the 7-way split)

- **Additive by construction:** the manifest + Session contract mean a feature is a
  file that registers itself and talks to state through one seam. The core is closed.
- **Testable by construction:** state-as-session turns every Word/helper into a
  unit-testable pure-ish function; the toolchain-gated integration tests become the
  *top* of the pyramid, not the only layer.
- **Parallelizable by construction:** the AE spine spreads across `session.rs` /
  `nouns/*` / `words/*` / `analysis/orchestrate.rs`; tracks (genrule, scheduler,
  patterns, repo-map) are already disjoint dirs/crates. The `rules.rs` collision that
  forced the spine to be sequential disappears.

---

## 7. Migration (incremental, green at every step)

Strangler pattern — `rules.rs` shrinks file-by-file, tests stay green throughout:

1. **Lexicon first** (lowest risk, highest test ROI): move pure helpers to `lexicon/`
   + add their unit tests. `rules.rs` calls them.
2. **Session**: introduce `Analysis` + `session(eval)`; route *one* thread-local
   through it; migrate the rest one at a time. (This is also P1's groundwork.)
3. **Nouns**: lift `File`/`Args`/`Depset`/`Ctx`/`RuleObj` into `nouns/`.
4. **Words**: lift each builtin into `words/<x>.rs` + manifest; delete from `rules.rs`.
5. **Rulesets**: `cc` joins rust/py/sh in `rulesets/`; the registry becomes the manifest.
6. `rules.rs` → `assembler.rs` (the thin core) + `analysis/orchestrate.rs`.

Each step is a small, independently-testable bit — which is the whole point.

---

*Design proposal. Not yet implemented. The Session (step 2) is the keystone and
doubles as AE Phase 1.*
