# Draft Architecture A1 — Thin Dialect over Starlark (◐ thin-dynamic, Bazel-grain)

*One of three candidate architectures scored against `ArchFundamentals.md` (F1–F23),
the Bazel input surface (`ArchBazelConstraints.md` C1–C9), and the four invariants. This
is `RazelDialect.md` **hardened**: same metaphor (Words/Nouns/Phrasebooks/Session), now
made rigorous, with the dead second loader resolved and every place "staying thin"
hard-codes a future axis marked honestly. Claims are grounded in the code at 62 commits;
file:line citations throughout.*

**The bet (stated plainly).** razel already owns the expensive relief valve — an embedded
Starlark interpreter (`starlark = "0.14.2"`, `rules.rs`). That is *Bazel's* bet, and it
satisfies F18/F15/F1 outright. So A1 spends **almost nothing more** above it: the
interpreter does the heavy lifting; the engine ships primitives. The only strategic
indirection A1 adds is the **one seam that is irreversible-if-skipped** — a passed,
plural `Session` replacing the 8 thread-locals (which *is also* the provider store) — plus
the cheap formalization of the registry into a const-array manifest. Everything else stays
direct, with the hard-codes named. A1 is the leftmost ◐ column on the Part-C dial wherever
it can be, mixing one notch right (◑) only at F5/7/10, where a build tool's reason to exist
forbids staying thin.

---

## 0. The metaphor, made precise (what is a Word / Noun / provider / graph-node / fact)

In razel-crate terms, the Dialect's pieces map to concrete Rust:

| metaphor | what it *is* in razel | concrete type / seam |
|---|---|---|
| **Word** | a Starlark builtin/global the dialect speaks (`rule`, `glob`, `select`, `depset`, `DefaultInfo`, `attr.*`, `native.*`, `Label`, package decls, `config_setting`) | `fn install(&mut GlobalsBuilder)` — one file under `words/`, today all inside `rule_globals`/`native_members`/`attr_members` (`rules.rs:580,1219,1246`) |
| **Noun** | a `StarlarkValue` a Word produces/consumes (`File`, `Depset`, `Args`, `Ctx`, `Actions`, `RuleObj`) | the `#[derive(... ProvidesStaticType ...)]` value types, today `rules.rs:160–465`; one file each under `nouns/` |
| **provider** | the *published output contract* a target exposes to dependents | today an **implicit bundle**: `AnalyzedTarget{default_info, hdrs, cflags}` (`rules.rs:44–58`); A1 gives it a thin typed identity (§1.F11) but keeps it a struct, not a value-graph |
| **fact** (YIDL borrow) | a captured, identity-bearing analysis record | **`AnalyzedTarget`** keyed by canonical label *is* razel's fact (`rules.rs:45`); the `Session.results` map (§1.F19) is the fact collection. This is the one YIDL idea A1 steals — facts-as-substrate (F11). |
| **graph-node** | a node in the build/exec graph | `razel_ir::{FileNode, ActionNode, TargetNode}` + `NodeRef` (`razel-ir/src/lib.rs:17–43`); and, on the live path under A1, an `Engine` key (`razel-engine`) |
| **Phrasebook** | a `@repo//` ruleset you `load()` | today `Ruleset{prefix, module}` + `ruleset_modules()` Vec (`rules.rs:1337,1390`); A1 makes the Vec a `const`-array manifest |
| **Session** | the meaning of *this* analysis run | a new `Analysis` struct carried via `eval.extra` — replaces the 8 thread-locals (§1.F19) |
| **Lexicon** | pure spelling helpers | `canon_label`/`qualify`/`pkg_of`/`glob_match`/`shquote`/`fold_depset` — today buried + untested (`rules.rs`, `lib.rs:76`) |

The **engine** is a tiny assembler: fold Words → `Globals`, fold Phrasebooks → the
loader's ruleset map, thread one fresh `Session` per `analyze_*`. The assembler is the
only "core" and is closed for modification.

YIDL framing: A1 takes YIDL's *authoring/decoupling seam* — facts (`AnalyzedTarget`) as
the substrate, label→contract lookup as the producer/consumer decoupling — but **rejects
YIDL's evaluation engine entirely**. YIDL is once-through, fixpoint-free, no incrementality
(`YIDLDigest.md` §4,§10); a build tool exists *for* incrementality. A1's graph notch (◑,
F5/7/10) is exactly the thing YIDL lacks and razel already half-owns in `Engine`.

---

## 1. Placement on the Part-C dial, per ★ fundamental — mechanism + how well/cheaply

For each load-bearing ★, the column A1 stakes out, the concrete mechanism, and an honest
"how well / how cheaply."

### F11 — producer↔consumer decoupling → **◐ label→bundle lookup** (thin), thinly typed

**Mechanism.** A consumer's `deps` resolve through the **fact collection keyed by canonical
label**. Today: a dep label is `canon_label`'d, looked up in `RESULTS`, and its
`default_info` is re-wrapped as `struct(files=[...])` for the consumer's `ctx`
(`rules.rs:500–522`). A1 keeps this label→contract lookup but (a) moves `RESULTS` into
`Session.results`, and (b) gives the bundle a **thin typed identity**: a `ProviderSet`
newtype with named, typed fields (`default_info: Vec<File>`, `hdrs`, `cflags`, plus an
open `BTreeMap<ProviderId, ProviderInstance>` for user `provider()`s). The consumer names
the *contract* (`dep.files`, `dep[CcInfo]`), never the producing rule — which is already
true today (the consumer reads `RESULTS`, never the producer's identity).

**How well / cheaply.** *Cheaply* — this is one struct + moving an existing map. It clears
the *invariant* of F11 (consumer never names producer) because the lookup is by label, not
by rule. **It does not clear S3 fully**: the bundle stays semi-structured (a fixed struct +
an escape-hatch map), not a fully typed provider lattice (the ◑/● columns). That is the
deliberate A1 hard-code — see §5 risk. The cheap typing (`ProviderId` identity, `Info`-style
indexing) is the *minimum* that lets third-party `provider()` not collapse back into the
`default_info` blob; going further (composition-by-return-type, ●) is rejected as
over-build for ~6 value types (`ArchAnalRazel.md` §4 "wide shallow tree" caution).

**Honest defect carried.** Today's dep resolution requires forward declaration — a dep
*not yet analyzed* hard-errors (`rules.rs:510–516`). That is a same-scope single-pass
artifact, not real two-phase analysis. A1 inherits it unless the F5/7/10 notch (below)
brings a load→analyze split; flagged as a near-term scar, not designed away.

### F5 / F7 / F10 — graph & incrementality → **◑ IR + demand-driven Engine on the live path** (one notch right)

**This is the only place A1 leaves the ◐ column, and it must.** A build tool that isn't
incremental has no reason to exist (F5 forcing). Today there are two graph stories and the
good one is **off the live path**:

- Live CLI build: `build_bazel_with`/`build_workspace_with` → `execute()`, a straight
  post-order DFS that re-runs every action's cache check serially
  (`razel-build/src/lib.rs:120–224`; CLI calls it at `razel-cli/src/main.rs:539,558`).
- Warm/daemon build: `IncrementalBuilder` wraps `razel_engine::Engine` — a clean,
  well-tested ([6 tests] `razel-engine/src/lib.rs:179–285`) Skyframe-lite with early
  cutoff. `IncrementalBuilder::configure()` **already maps `AnalyzedTarget` → Engine nodes**
  (file→input, action→derived-over-inputs, target→derived-over-actions;
  `incremental.rs:106–`). Only the daemon uses it (`daemon/rpc.rs:225` still calls the
  straight `execute`, not even the Builder — the Engine is used only via the warm-analysis
  path).

**Mechanism (the resolution of the live-path/Engine split).** Delete `execute()`'s straight
loop as the *product* path; route **all** builds — cold CLI included — through
`IncrementalBuilder` over the existing `Engine`. The Engine is already a passed value with
interior `RefCell`s (`razel-engine/src/lib.rs:33`) — the structural opposite of `rules.rs`,
needs no rewrite. F7 (parallel) is then a *scheduler over the Engine's dependency edges*
(independent derived nodes have no path between them), not a new graph. F10 (demand-driven)
is `Engine::request(key)` — it already loads only the requested node's transitive deps
(`request_inner`, `lib.rs:112`); the loader's on-demand `load_package` (`rules.rs:1482`)
already gives demand-driven *loading*.

**How well / cheaply.** *Cheaply-ish* — the Engine and the mapping both exist; the work is
making the CLI call the Builder and adding a parallel scheduler (commodity: a worklist over
the ready-frontier). F5/F6/F10 land essentially for free (Engine has early-cutoff today,
`changed_at` firewall, `lib.rs:169`). **What stays thin / hard-coded:** the Engine is
`String`-keyed `Digest`-valued (`type Key = String; type ComputeFn = Box<dyn Fn(&[Digest])
-> Digest>`, `lib.rs:15–17`) and lives *below* the analysis layer — it incrementalizes
**execution**, not **analysis**. Re-analysis of an edited BUILD is still whole-package
(the Session is rebuilt per `analyze_*`). That is an accepted A1 limit: incremental
*execution* now; incremental *analysis* (Skyframe-over-analysis-nodes, ●) deferred — the
plural Session makes it *possible* later (you can hold N sessions) without forcing it now.

### F12 — configuration / variation → **◑ select + config_setting matching** (one notch right of today's stub)

**Mechanism.** Today `select()` picks `//conditions:default` or the first branch
(`rules.rs:744–753`) and `config_setting` records an *empty target* (`rules.rs:648–657`) —
a labeled no-op. A1 makes the minimum real: `config_setting(name, values={...},
flag_values={...})` records a **matcher fact** in `Session.configs` (flag/`--define`/
`--//flag` predicates, Eq-only); `select({label: value})` resolves by evaluating those
predicates against the build's `GlobalFlags` + `--define` set and picking the
highest-specificity match (Bazel's rule: most-specific `config_setting` wins; `default` is
the fallback). This is YIDL's **Eq-only, AND-combined, highest-score-first matcher**
(`YIDLDigest.md` §2) applied to exactly the one place razel needs it — and it's a *good*
fit because `config_setting` matching genuinely is static, finite, Eq-only dispatch.

**How well / cheaply.** *Cheap and correct for the common case* (platform/mode/`--define`
selects). **Hard-codes the future axis explicitly:** A1 does **not** build
`(Target × Configuration)` keying as a graph axis, `Fragment`s, or **configuration
transitions** (`cfg = "exec"`/custom, C7). That is the single largest deferred surface.
The honest mark: *transitions will need rework* — they change a target's identity per
configuration, which a thin select-matcher can't express; retrofitting them is the S6
risk A1 most courts (config bolted on late → `*AttributeMapper` thicket). A1's mitigant
is only partial: deciding the matcher model now (R9 "decide the config model early, even
if to defer explicitly") and keeping the attribute schema seam present (today `attr.*`
descriptors are parsed-then-ignored, `rules.rs:1246`) so the configurable-edge concept has
a home to grow into. **This is named as A1's characteristic dead-end.**

### F16 — open-set composition / extension → **◑ registry + open-set dispatch via const-array manifest**

**Mechanism.** The ruleset registry already proves open-set registration for languages:
rust/py/sh are each one file (`rust_rules.rs` 196 LOC, etc.) wired by one row in
`ruleset_modules()` (`rules.rs:1390–1423`), introduced deliberately as a seam (commit
`12dbdbe`). A1 (a) makes that `Vec` a `const PHRASEBOOKS: &[fn(&Session) -> Result<Ruleset,
String>]` manifest, and (b) extends the *same* discipline to Words and Nouns:
`const WORDS: &[fn(&mut GlobalsBuilder)]`, `const NOUNS` registered the same way. A generic
operation (`build`, `test`) works over the open member set because dispatch is by
target-kind/provider-contract, never by an `if rule == "cc_library"`. New language = new
`rulesets/<x>.rs` + one manifest row; new builtin = new `words/<x>.rs` + one row.

**How well / cheaply.** *Cheap and right-altitude.* The manifest is config-as-data: razel's
vocabulary visible in one greppable place, ordered, subset-assemblable for tests
(`ArchPatterns` P-F: "explicit const-array, chosen"). `inventory`-style auto-registration is
**rejected** — Rust makes you `mod` the file anyway, and we want ordering + subset assembly
(`ArchAnalRazel.md` §4 agrees; the too-abstract dead end for ~12 builtins). **What stays
hard-coded:** dispatch is over a *flat* contract (target-kind + provider bundle), not
Pants-style `@union`/`UnionMembership` typed plan (●). For razel's member count that is
correct; if cross-cutting overlays (aspects, F17) arrive, this notch needs the next column
— flagged, not built.

### F18 / F15 — evaluation environment → **◐ thin builtins over Starlark + Session** (full left, the core bet)

**Mechanism.** The interpreter is `starlark-rust`, already embedded and decoupled (it
imports zero build-tool types — R1 satisfied). F18 (sandboxed, side-effect-free, no
arbitrary I/O) and F15 (new rules added in-language, zero engine edits) are *already met by
owning Starlark*. A1 adds only the thin layer: Words register builtins into
`GlobalsBuilder`; rule impls run in-scope; the Session carries state. No typed value-graph
above Starlark (that is the ● column and the Pants two-language tax, S8 — rejected).

**How well / cheaply.** *This is the cheapest possible — it's already done.* The art, per
`ArchModel.md` §2, is the layer above, which is precisely the Session (F19). The thinness
*is* the leverage: maximal dynamism, zero engine edits to add `cc`/`rust`/`py`. The cost is
weaker static guarantees — unknown providers/loads surface at analysis, not at a compile
step (R13: A1 deliberately takes the lazy/dynamic end, *not* Pants's startup-failure
rigidity S9). Eager seam-validation (unknown `@repo`, unknown builtin) is kept where it's
cheap (`BzlLoader` already errors on unknown prefixes, `rules.rs:1355`).

### F19 — analysis state → **passed, plural `Session` (no dial; thread-locals = S1)**

**Mechanism.** The keystone. The 8 thread-locals — `STATE, CONFIGS, RESULTS, WORKSPACE,
CURRENT_PKG, LOADED, GLOBAL` (`rules.rs:66–101`) **plus** the dead loader's `CTX`
(`lib.rs:59`) — become fields of one `Analysis` struct, created per `analyze_*` and handed
to every Word via `eval.extra`. The mechanism is *verified real*: `Evaluator` exposes
`pub extra: Option<&'a dyn AnyLifetime<'e>>` and `extra_mut` in starlark 0.14.2
(`evaluator.rs:176–178`). `extra_mut` (`&mut`) does **not** compose under nested eval (a
rule impl calls `ctx.actions.run`, which re-borrows the evaluator) — so the composable
choice is `eval.extra` (`&dyn`) carrying a `Session` with **interior-mutability fields**
(`RefCell`/`Cell`), exactly the pattern the clean `Engine` already uses
(`razel-engine/src/lib.rs:33`).

```rust
// analysis/session.rs  (new)
pub struct Analysis {
    pub workspace: Option<PathBuf>,
    pub current_pkg: RefCell<Option<String>>,
    pub global_flags: GlobalFlags,
    targets: RefCell<Vec<AnalyzedTarget>>,
    results: RefCell<BTreeMap<String, AnalyzedTarget>>, // the fact collection (F11)
    providers: RefCell<BTreeMap<ProviderId, ProviderInstance>>, // thin typed providers
    configs: RefCell<Vec<ConfigSetting>>,               // config_setting matchers (F12)
    loaded: RefCell<HashSet<String>>,
}
pub fn session<'v>(eval: &Evaluator<'v, '_, '_>) -> &Analysis { /* downcast eval.extra */ }
```

**How well / cheaply.** This is the highest-leverage, must-do move and it is **cheap and
irreversible-if-skipped**. The `RefCell` is *not* the smell returning: the smell was
*ambient + global + lifetime-less*; the Session is *scoped* (one per run, no `reset`, no
leakage — killing the divergent hand-resets at `rules.rs:1447,1470,1518,1534`), *explicit*
(`session(eval)` appears in signatures), and *plural* (many coexist → concurrent package
analysis, two-phase, snapshot-for-incremental). **Double payoff:** `Session.results` *is*
the F11 provider store — the refactor and the AE provider work are one move
(`ArchAnalRazel.md` §4 verified this against the code). There is **no dial here**; every
viable architecture does this; only S1 (thread-locals) is wrong.

---

## 2. Bazel input-surface fidelity (C1–C9)

A1 keeps razel's existing front-end and *strengthens* its seam; it consumes unmodified
`BUILD`/`.bzl` as today.

- **C1 (Starlark semantics, `.bzl` loading, macros):** ✅ owned — `starlark-rust` +
  `BzlLoader` (recursive `.bzl` load + freeze, `rules.rs:1353–1385`); macros are plain
  `def`s that work via evaluation. A1 changes nothing here.
- **C2 (labels/packages/visibility):** ✅ parse — `canon_label`/`qualify`/`pkg_of`; package
  decls are recognized no-ops (`rules.rs:622–642`); visibility vocabulary parses (absorbed).
- **C3 (native BUILD builtins):** ✅ `glob`/`select`/`package`/`filegroup`/`genrule`/`alias`/
  `config_setting`/`test_suite`/`exports_files` are Words today (`rules.rs:648–704`,
  `lib.rs:171`); A1 just relocates them to `words/` + manifest. **Gap honestly noted:**
  `existing_rules`, full `genrule` make-vars/`$(location)` are partial.
- **C4 (rule-authoring API):** 🟡 `rule()`/`ctx`/`actions.run|write`/`DefaultInfo`/`depset`/
  `Label`/`attr.*`/`native.*` exist (`rules.rs`); `provider()` gets the thin typed identity
  (§1.F11); **`aspect()` and transitions are NOT built** (the F12/F17 deferral).
- **C5 (ruleset `load()` surface):** ✅ `@rules_cc/_rust/_python/_shell/_skylib/_local_config_*`
  resolve via the registry (`rules.rs:1390–1423`) — the manifest *is* this surface.
- **C6 (per-language conventions):** 🟡 cc compile→archive→link + flag shapes exist (inline
  `cc_rules`); rust/py/sh shims exist; correctness is asserted on **captured argv** (§3b),
  not by running compilers.
- **C7 (config/platforms/toolchains):** 🟡→ select+config_setting matching (§1.F12);
  **platforms/`constraint_*`/toolchain *resolution*/transitions deferred** — A1's biggest
  fidelity gap, named.
- **C8 (external deps `MODULE.bazel`/`bazel_dep`):** ◐ `@repo//`→local-path mapping (no
  fetch) — read the module graph to resolve `@repo`, defer fetching (`ArchPatterns` P-H).
  Declarative-from-day-one intent (R11) kept even though minimal.
- **C9 (CLI surface):** ✅ inventoried + parsed (`razel-cli`, `bazel_flags.rs` 7245 LOC
  generated table).

**The seam is preserved and sharpened.** The lowering line stays `AnalyzedTarget` →
`wire_to_ir` → `razel_ir::Graph` (`razel-analysis/src/analysis.rs:126`). Below it the engine
knows only File/Action/Target/Provider — never `cc_library`. A1's only seam change: make
`wire_to_ir` consume the unified `AnalyzedTarget` (after the dead-loader deletion, §6) so
there is exactly one lowering path.

---

## 3. The four invariants

1. **Analysis state is a passed, scoped, plural value [F19]:** ✅ the `Session` (§1.F19) —
   *the* central move; 8 thread-locals deleted.
2. **World-effects quarantined to a content-addressed kernel, pure above [F2/F3]:** ✅
   unchanged — `razel-exec` (sandbox + content-addressed `Action`) and `razel-actions`
   stay the only world-touching code; Words/Nouns/rule impls are pure over the Session.
   This area *converges* across all three architectures (`ArchPatterns` P-D) — not a dial.
3. **Units unit-testable on pure data, no toolchain [testability]:** ✅ by construction once
   state is passed (§3b below) — assert on captured `AnalyzedAction.argv`
   (`rules.rs:35–41`, pure data), not on a compiler run.
4. **Clean front-end→IR seam; engine never knows `cc_library` [seam]:** ✅ §2 — and made
   *cleaner* by deleting the second lowering path.

---

## 4. Honest a/b/c assessment

**a) Modularity / smallest-edit — strong, the design's whole point.** After the refactor a
new builtin = `words/<x>.rs` + one manifest row; new language = `rulesets/<x>.rs` + one row;
new value = `nouns/<x>.rs`. The assembler and Session don't move. This directly kills the
`rules.rs` gravity well (S5): the 22 commits that each bolted 20–170 lines onto a 1658-LOC
file (`ArchAnalRazel.md` §3 numstat) become disjoint files. *Honest cost:* it trades one
god-module for a **wide shallow tree** — ~20 files for a ~1658-LOC body; near the upper
bound of useful decomposition for ~12 builtins + ~6 value types (`ArchAnalRazel.md` §4). A1
explicitly **forbids a `dialect/` super-layer** on top (that would be the too-abstract dead
end). Manifest-sync is a small recurring cost (a row must be added) — accepted as the
legible config-as-data surface.

**b) Testability — strong, and the largest single win after the Session.** Today: 4 coarse
`analyze_starlark` round-trips (`rules.rs:1563`), ~28 `Path::exists(){return}` toolchain
skips that silently collapse coverage on a compiler-less CI (`ArchAnalRazel.md` §4.4, S11).
A1: every Word is `fn(args, &Analysis)` — build a fresh `Session`, call the pure core,
assert on it; no Evaluator, no toolchain, order-independent. The Lexicon helpers
(`canon_label`, `glob_match`, `fold_depset`, `shquote`) become directly unit-tested (they
have **zero** direct tests today, `ArchAnalRazel.md` §4.1). Rule bodies assert on captured
`AnalyzedAction.argv` (R12). *Honest limit:* A1 makes the *Words* unit-testable but does not
by itself remove the toolchain-gating on the *cc/rust/py/sh rule bodies' end-to-end* tests —
that requires the orthogonal discipline of argv-assertion, which A1 adopts but which is not
a structural consequence of the Session.

**c) Calibrated extensibility — deliberately minimal, and that is the gamble.** A1 spends
its relief valves at exactly two seams (the plural Session, the manifest registry) and the
one forced notch (Engine on the live path), and stays hard-coded everywhere else: the IR,
the action/exec/wire contracts (codegen governs drift), the flat provider bundle, the
select-matcher config model. This is the *right* altitude **if** razel's future is more
languages and bigger graphs; it is **under-built** if the future is rich configuration
(transitions, platforms) or orthogonal overlays (aspects). A1 does not pretend otherwise —
it names those as the rework points (§5). Per `ArchitectSkillRules`, the heuristic for
adding indirection is "(a) the seam plausibly moves on an orthogonal axis AND (b) cheap now,
expensive to retrofit." A1 judges: state/registry meet both (spend now); config-transitions
meet (a) but A1 *bets* (b) is tolerable later — that bet is its central risk.

---

## 5. Characteristic dead-end risk + the scars it courts

**Dead-end type: TOO RIGID** (the symmetric opposite of the ●-rich options). A1's reflex is
"stay thin / hard-code the pragmatic choice," which `ArchitectSkillRules` names as the
*default failure of AIs and many humans*. A1 is consciously near that edge and survives only
because the *forced* indirection (Session, Engine-on-live-path) is spent and the hard-codes
are named, not hidden.

**The specific dead-end: configuration.** A1's `select`+`config_setting` matcher is real but
flat. The moment razel must build the same target two ways in one invocation — **configuration
transitions** (`cfg="exec"`, custom transitions, C7) and `(Target × Configuration)` identity
— the thin matcher cannot express it, because it has no notion of a target's identity varying
by config. Retrofitting this into a label-keyed fact collection is expensive (Bazel paid for
it with the `*AttributeMapper` thicket).

**Scars from `ArchPatterns` Part B that A1 most courts:**
- **S6 (configurability bolted on late)** — the primary risk, per above. Mitigant: decide the
  matcher model *now* and keep the attribute-schema seam present (R9), but A1 does not build
  transitions, so S6 is *deferred, not defeated*.
- **S3 (under-typed early data contract)** — the provider bundle stays semi-structured. A1's
  thin `ProviderId` typing is the *minimum* hedge; if third-party typed providers proliferate,
  the flat-struct-plus-escape-map will want the ◑ typed-provider lattice, and migrating a
  data contract is "among the most expensive things to undo" (`ArchModel.md` R4).
- **S9 (too-rigid startup) is *avoided*** — A1 keeps Starlark's dynamism (lazy validation),
  deliberately *not* Pants's static-graph startup rigidity. Worth stating: A1 sits at the
  opposite scar-pole from the ●-rich option.
- **S12 (migration debt via guarded flips)** — minor: routing the CLI onto the Engine and
  unifying the loader are flag-able transitions; A1 should do them as outright cutovers
  (green-at-each-step strangler, §6), not long-lived `incompatible_*`-style flags.

---

## 6. Migration path from today (62 commits; `rules.rs` god-module + 8 thread-locals + dead loader)

Strangler pattern — green at every step, each step independently testable. Ordered by
risk-ascending / leverage:

0. **Resolve the dead second loader FIRST (R14 — the gap RazelDialect missed).** `lib.rs`'s
   `TargetDecl`/`build_rules`/`load_build`/`query_targets`/`CTX` (`lib.rs:33–231`) and
   `razel_analysis::analyze` (`analysis.rs:48`) + the `Depset<T>` order machinery are
   reachable **only from their own tests** (verified: `razel_analysis::analyze` is called
   only at `analysis.rs:191`; the live path is `analyze_starlark`+`wire_to_ir`). **Decision
   for A1: delete the `lib.rs` loader, `TargetDecl`, `razel_analysis::analyze`, and the
   `CTX` thread-local.** Keep `glob_match` (move to `lexicon/`) and `wire_to_ir`. This must
   precede the refactor, else `nouns/depset.rs` becomes a *third* depset implementation
   (`ArchAnalRazel.md` §4.3). One PR, deletes code + its tests, live tests stay green.

1. **Lexicon first** (lowest risk, highest test ROI): move `canon_label`, `qualify`,
   `pkg_of`, `glob_match`, `shquote`, `fold_depset`, `file_path`, `include/define_flags` to
   `lexicon/` and add their unit tests (they have none today). `rules.rs` calls them.

2. **Session (the keystone)**: introduce `Analysis` + `session(eval)`; carry it via
   `eval.extra` in `eval_build_src`/`analyze_starlark`/`analyze_workspace`. Migrate
   thread-locals one at a time (`RESULTS`→`results` first — it's also the provider store),
   deleting each divergent hand-reset (`rules.rs:1447,1470,1518,1534`). Unify the three
   divergent globals-builders (`analyze_starlark` rebuilds globals inline at
   `rules.rs:1542`, omitting `native`/`attr`/loader — fix the drift, `ArchAnalRazel.md`
   §4.2) into one `dialect()` assembler.

3. **Nouns**: lift `File`/`Args`/`Depset`/`Ctx`/`Actions`/`RuleObj` (`rules.rs:160–465`)
   into `nouns/` one file each.

4. **Words + manifest**: lift each builtin into `words/<x>.rs`, add the `const WORDS`
   manifest; delete from `rules.rs`. Push the still-inline cc/skylib/autoconfig rule bodies
   (`cc_rules` 897–1019, `skylib_rules` 1088, `auto_config_fns` 1193) out to `rulesets/`,
   finishing the half-built seam so they match rust/py/sh.

5. **Phrasebook manifest**: `ruleset_modules()` Vec → `const PHRASEBOOKS`. Low-risk; just
   formalizes what works.

6. **Engine on the live path (the F5/7/10 notch)**: route `build_bazel_with`/
   `build_workspace_with` through `IncrementalBuilder` (which already maps `AnalyzedTarget`→
   Engine, `incremental.rs:106`) instead of `execute()`'s straight loop. Then add a parallel
   scheduler over the Engine's ready-frontier (F7). Switch the daemon off the straight
   `execute` too (`daemon/rpc.rs:225`). This is the step that needs the plural Session
   (step 2) to be done first.

7. **`rules.rs` → `assembler.rs`** (the thin closed core) + `analysis/orchestrate.rs`
   (`analyze_*`). The god-module is gone.

8. **Then, only if/when bitten:** F12 config_setting matching (step into ◑), and — flagged
   as the rework boundary — transitions/platforms (the ● column A1 deliberately did not
   build). Do these as deliberate increments, not by accreting flags (S12).

*Steps 0–2 are the irreversible-value moves and should land first; 3–7 are mechanical
strangler steps; 8 is the honestly-deferred frontier where A1's too-rigid bet gets tested.*
