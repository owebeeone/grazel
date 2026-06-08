# Architecture A2 — Typed Provider Graph (● typed-rich, Pants-informed, single-language)

**One-line bet.** razel already owns the ultimate relief valve (Starlark). A2 spends the
*next* relief valve on a **typed model above the interpreter**: first-class typed
**Providers** as the only inter-target contract, and a **single demand-driven Engine that
IS the live execution model**. The provider store *is* the Session. We borrow Pants's two
best ideas — composition-by-typed-value and open-set union dispatch — but pay **neither**
of Pants's two taxes: no second language (all Rust + Starlark) and no
startup-failure rigidity (Starlark stays dynamic; we validate seams, we don't refuse to
start). This is the **right-hand column** of the Part C dial, deliberately, with the
over-build risk owned in §6.

Read against `ArchFundamentals.md` (F1–F23), `ArchPatterns.md` (Part C dial), `ArchModel.md`
(R1–R15), and grounded in the actual crates (`razel-loading/src/rules.rs`,
`razel-engine`, `razel-ir`, `razel-build`, `razel-analysis`). It is a sibling to the thin
`RazelDialect.md` proposal: A2 keeps the Dialect's Word/Noun/Phrasebook/Session vocabulary
verbatim and adds typed structure on top of it. A1 (thin) and A3 (rich-graph) are the
other two points on the dial.

---

## 0. Where A2 sits — the Part C dial, per ★ fundamental

| ★ Fundamental | ◐ thin | ◑ mid | ● rich | **A2 picks** |
|---|---|---|---|---|
| **F11** decoupling | label→bundle | typed providers | composition-by-type | **● composition-by-typed-value**, expressed through ◑ **typed providers** as the named contract |
| **F5/F7/F10** graph | IR + straight loop | demand-driven Engine on live path | static rule-plan + runtime memo | **◑→● the Engine becomes THE live path** (today it is off-path, S7); analysis nodes join it too |
| **F12** config | pick-default select | select+config_setting matching | config-as-graph-axis + transitions | **● config as a graph axis**: `(Target × Config)` keying + real `select`/`config_setting` matching + transitions as configurable edges |
| **F16** extension | bare registry | registry + open-set dispatch | union-membership + typed plan | **● open-set union dispatch** (`ProviderType`-keyed) layered over the ◑ Phrasebook manifest |
| **F18/F15** eval | thin builtins | dialect + Session | typed value graph above Starlark | **● typed value graph above Starlark** — Starlark stays the dynamic surface; the *values it produces* are typed Nouns/Providers |

A2 is the column that says: **commit the typed structure now, while there are ~0 consumers
to migrate, because retrofitting a typed provider contract and a unified graph later is
exactly the multi-year debt Bazel (struct→declared-provider, S3) and Pants (v1→v2) paid.**
The symmetric risk — over-build (S8/S9/S10) — is named and bounded in §6.

---

## 1. The model — Words, Nouns, Providers, Graph-nodes, Facts (in razel-crate terms)

A2 inherits the Dialect metaphor and sharpens the data contract. The vocabulary maps to
concrete crate types:

| concept | A2 meaning | razel-crate type / location |
|---|---|---|
| **Word** | a builtin verb (`rule`, `provider`, `attr.*`, `select`, `config_setting`, `transition`, `glob`, `depset`, `Label`) | `razel-loading/src/words/<x>.rs`, `fn install(&mut GlobalsBuilder)`; manifest `WORDS` |
| **Noun** | a typed `StarlarkValue` a Word produces/consumes (`File`, `Depset`, `Args`, `Ctx`, `Target`, **`ProviderType`**, **`ProviderInstance`**, `RuleObj`, `AttrSchema`) | `razel-loading/src/nouns/<x>.rs` |
| **Provider** | **the typed inter-target contract.** A `ProviderType` is a constructor with **identity** (a `ProviderKey`); a `ProviderInstance` is its typed field-bag. `CcInfo`, `DefaultInfo`, `PyInfo` are *instances of this one mechanism*, not hard-coded structs | `nouns/provider.rs`; the **typed replacement** for today's implicit `DefaultInfo`+`hdrs`+`cflags` bundle |
| **ProviderSet** | the published contract a configured target exposes: `BTreeMap<ProviderKey, ProviderInstance>` | `Session.providers` field → becomes the engine's analysis-node value |
| **graph-node** | a node in the **one** demand-driven `Engine`: an *input* (source file digest, flag), or a *derived* node — `PackageNode` (load+eval), `ConfiguredTargetNode` (analyze → ProviderSet), `ActionNode` (spawn) | `razel-engine` node enum (today `Input`/`Derived { compute }`; A2 makes the node kinds first-class) |
| **Fact** (YIDL borrow) | a typed analysis datum keyed by identity — here a `(Label, Config)` → `ProviderSet` row, and a declared `DepEdge`. The provider store is literally "facts as providers" (`YIDLDigest` transferable #1). **We borrow the *substrate* (typed facts, definition-time disambiguation), not the once-through, fixpoint-free *evaluation engine* — that is exactly what `YIDLDigest` §7 says to reject for a build tool.** | `Session.providers` + IR `add_dep` |
| **Phrasebook** | a `load()`-able ruleset (`@rules_cc`, `@rules_rust`, …) | `razel-loading/src/rulesets/<x>.rs`; manifest `PHRASEBOOKS` |
| **Session** | the per-analysis scoped value; **`Session.providers` IS the provider store IS the engine's analysis-state** | `razel-loading/src/analysis/session.rs`, the `Analysis` struct that kills the 8 thread-locals |
| **Lexicon** | pure helpers (`canon_label`, glob-match, depset-fold) | `razel-loading/src/lexicon/*.rs`, directly unit-tested |

**The one idea that ties it together (and is A2's identity):** *the provider store is the
Session, and the Session's contents are the values of the analysis nodes in the one
demand-driven graph.* Decoupling (F11), state (F19), and the graph (F5/F10) are not three
subsystems — they are three views of one typed store threaded through one engine. That
unification is the whole leverage; it is also where the over-build risk concentrates (§6).

---

## 2. Per-★-fundamental: mechanism + how well/cheaply it satisfies it

### F11 — Producer↔consumer decoupling ★ (dial: ● composition-by-typed-value, via ◑ typed providers)
**Mechanism.** `provider()` (a Word) mints a `ProviderType` Noun with a stable
`ProviderKey` identity. A rule impl returns a list of `ProviderInstance`s; the Session
stores them at `(Label, Config)`. A consumer reads `dep[CcInfo]` — it names the **provider
type**, never the producing rule. Any rule returning a `CcInfo` is substitutable. We add a
*soft* composition-by-type convenience (a consumer goal can ask "all deps' `CcInfo`")
without Pants's hard "rule-returning-T auto-wires" — keeping the named-contract legibility
and dodging accidental type-collision wiring (S10).
**How well/cheaply.** *Well:* this is the F11 textbook answer and it is **typed from day
one** — the single most expensive thing to retrofit (S3, R4). *Cheaply:* it replaces an
existing structure (the `RESULTS` thread-local bundle + the `dep[Info]` reconstruction at
`rules.rs:498-521`) rather than adding a new one. The cost is real but front-loaded: one
`nouns/provider.rs` + `Session.providers` typed, and every rule now returns typed providers
instead of leaning on the `hdrs`/`cflags` side-channel.

### F5 / F7 / F10 — Incrementality, parallelism, demand-driven ★ (dial: ◑→● unify the Engine onto the live path)
**Mechanism.** Today there are **two** execution models: `IncrementalBuilder`/daemon drive
`razel_engine::Engine` (Skyframe-lite with verified_at/changed_at early cutoff —
`razel-engine/src/lib.rs:149`), while the live `build_target`→`execute` path does its own
`collect_order` deps-first loop with **no incrementality** (`razel-build/src/lib.rs:191`).
That is scar **S7** (split-brain). A2's central move: **make the Engine the only execution
model**, and lift *analysis* into it too. Node kinds become first-class: `PackageNode`
(demand a package's loaded targets — F10: only load what's reached), `ConfiguredTargetNode`
(analyze `(Label, Config)` → `ProviderSet`), `ActionNode` (spawn). A `ConfiguredTargetNode`
demands its deps' `ConfiguredTargetNode`s; the engine's existing early-cutoff
(`changed_at` not bumped when a recomputed value is unchanged) gives **F6 for analysis, not
just execution**. Demand-driven (F10) falls out: requesting `//app:bin` pulls only the
`PackageNode`s on its reverse path.
**How well/cheaply.** *Well:* one graph buys F5/F6/F7/F10 + overlays uniformly; it is the
Skyframe lesson (`ArchPatterns` P-C ●) and `ArchModel` R5. *Cheaply — honestly, not cheap:*
the Engine today is **`String`-keyed, single-threaded (`RefCell`/`Cell`, not `Send`/`Sync`),
and only models opaque `Digest` values** (`razel-engine/src/lib.rs:33-38,102`). Making it
the live, parallel (F7) analysis+execution graph means a typed `Key` enum, `Send`/`Sync`
nodes, and a tokio/rayon driver. **This is the single largest build in A2 and the heart of
its over-build risk** (§6). The mitigation: do it *incrementally* — first route
`build_target` through the existing single-threaded Engine (deleting the second loop, S7),
then parallelize. Keep the restart/demand protocol **soft**: a missing dep re-demands, it
does not `fail()` the build (unlike Pants startup).

### F12 — Build-time configuration / variation ★ (dial: ● config-as-graph-axis + transitions)
**Mechanism.** Decide the config model **now** (R9 — bolting it on late is the
`*AttributeMapper` thicket, S6). An attribute is, from inception, a **select-able,
transition-aware edge**. Config becomes a graph axis: the engine key is `(Label, Config)`,
so the *same* target analyzes under multiple configs as distinct nodes. `config_setting`
defines a matchable fact; `select({...})` is resolved by **matching the active `Config`
against `config_setting` facts** (borrowing YIDL's definition-time-disambiguated, Eq-style
matcher for *this bounded decidable slice only* — `YIDLDigest` transferable #5), not by the
current stub that returns `//conditions:default` or the first branch
(`rules.rs:744-753`). A `transition` (a Word) is a configurable edge that maps the child's
`Config`; `cfg = "exec"` is the built-in instance.
**How well/cheaply.** *Well:* this is the only model that survives multi-platform/mode
without a retrofit; it satisfies F12/F13 structurally. *Cost:* this is **net-new** (today
`select`/`config_setting`/transitions are all stubs) and it is the classic analysis-graph
blowup risk (`ArchPatterns` P-G ● warning). **A2 deliberately stays softer than Pants
here:** we adopt `select`+`config_setting` matching as the *committed* model but **defer the
full `(Target × Config)` cross-product machinery behind a single-config default** until a
real multi-config consumer exists — the model is decided (R9), the machinery is demand-paced.

### F16 — Open-set composition ★ (dial: ● union-membership dispatch over ◑ manifest)
**Mechanism.** A generic goal (`build`/`test`/`lint`) dispatches on **`ProviderType` /
`Target`-kind membership**, not on a hard-coded rule list. A new language registers a
Phrasebook (one file + one `PHRASEBOOKS` manifest line — the ◑ base, kept from
`RazelDialect`) *and* contributes its provider types and a goal-membership entry. `test`
iterates "all targets exposing a `TestInfo` provider" — it gains a language without being
edited (the Pants `@union`/`UnionMembership` lesson, `ArchAnalPants` §3; YIDL phase-splice,
`YIDLDigest` transferable #2). The const-array manifest stays (R7: explicit, ordered,
greppable — **not** `inventory` auto-registration).
**How well/cheaply.** *Well:* exactly the "add a linter under `test` with zero edits to
`test`" property. *Cheaply:* the registry seam **already exists** for rust/py/sh
(`ArchModel` §4: R7 = 🟡); A2 generalizes it from "languages" to "provider/goal membership"
— a widening of a proven seam, not a new subsystem.

### F18 / F15 — Constrained eval + engine-closed extensibility ★ (dial: ● typed value graph above Starlark)
**Mechanism.** Starlark stays the sandboxed, deterministic, I/O-free evaluation environment
(F18 already satisfied — R1, `rules.rs` embeds starlark-rust). A2's contribution is that the
**values Starlark produces are typed Nouns/Providers**, and the engine consumes those typed
values, never Bazel concepts. The engine knows `Target`/`Action`/`ProviderInstance`; it does
**not** know `cc_library` (the front-end→IR invariant). New rules/languages are added in
Starlark + Phrasebooks with **zero engine edits** (F15, R2 — ship primitives not rules; the
S2 lesson Bazel paid for with `exported_rules = {}`).
**How well/cheaply.** *Well:* keeps Bazel's dynamism (the thing Pants lacks) *and* gains
Pants's typed contract — the best of both, which is A2's reason to exist. *Cheaply:* the
typing is additive over the existing Noun layer (`Ctx`, `Depset`, `Args` are already
`StarlarkValue`s in `rules.rs`); we are *typing the provider channel*, the one place
`razel-analysis` is currently untyped.

---

## 3. Core modules / types / seams (concrete)

```
razel-loading/src/
  assembler.rs            // dialect() folds WORDS→Globals; the only "core"; closed
  analysis/
    session.rs            // Analysis struct (kills 8 thread-locals); Session.providers = store
    provider_store.rs     // (Label,Config) -> ProviderSet ; the "facts" table
    orchestrate.rs        // analyze_*; two-phase load→analyze; hands Session to each Word
  words/                  // rule.rs provider.rs attr.rs select.rs config_setting.rs
                          // transition.rs depset.rs glob.rs label.rs package.rs graph.rs
                          // native_ns.rs stdlib.rs  + mod.rs (WORDS manifest)
  nouns/                  // provider.rs (ProviderType/ProviderInstance/ProviderKey) ← NEW typed contract
                          // target.rs (carries ProviderSet) ctx.rs actions.rs args.rs
                          // file.rs depset_value.rs rule_obj.rs attr_schema.rs config.rs
  rulesets/               // cc.rs rust.rs py.rs sh.rs skylib.rs autoconfig.rs + mod.rs (PHRASEBOOKS)
  lexicon/                // label.rs paths.rs glob_match.rs shquote.rs  (pure, unit-tested)

razel-engine/             // THE execution model (was off-path). Node kinds: Package /
                          // ConfiguredTarget / Action. Typed Key enum. Send+Sync. demand/restart.
razel-ir/                 // dialect-agnostic Graph: File/Action/Target nodes + rev edges (kept;
                          // it already has the rdep index for F20). ProviderSet flows as node value.
razel-analysis/           // wire typed AnalyzedTarget+ProviderSet → engine nodes (the lowering seam)
```

**The four named seams:**
1. **Front-end→IR** (`AnalyzedTarget`+`ProviderSet` → engine nodes via `wire_to_ir`): the
   engine never sees `cc_library`. *Invariant honored.*
2. **Provider channel** (`nouns/provider.rs` ↔ `provider_store.rs`): the only inter-target
   data path; typed. *F11/R4.*
3. **Extension** (`WORDS`/`PHRASEBOOKS` manifests + provider/goal membership): open-set,
   data-driven. *F15/F16/R7.*
4. **Effect kernel** (`razel-exec` actions, content-addressed, sandboxed): unchanged,
   pure-above. *F2/F3/R6.*

A **Word** is `fn(args, &Session) -> Value` (pure core extractable + unit-tested without an
Evaluator). A **Noun** is a `StarlarkValue`. A **Provider** is a typed
`(ProviderKey, ProviderInstance)`. A **graph-node** is an engine `Key`-keyed node. A
**Fact** is a `(Label, Config) → ProviderSet` row in the store.

---

## 4. Bazel Constraints (C1–C9) + the four invariants

**Constraints — front-end fidelity (all met by the same Starlark front-end the thin design
uses; A2 changes the *layer above*, not the surface):**
- **C1/C4 (Starlark + rule-authoring API):** `rule()`, `attr.*`, `ctx`, `provider()`,
  `depset`, `Label`, `aspect`, `select`/`config_setting`, transitions, toolchains are Words.
  A2 makes `provider()` and the `attr.*` schema **load-bearing** (today `rule()` ignores
  `attrs` entirely — `rules.rs:587 let _ = attrs;`); schema-driven attrs produce `ctx`
  Targets/files/executable.
- **C3 (native builtins):** `glob`/`select`/`genrule`/`alias`/`config_setting`/… are Words;
  `select`/`config_setting` move from stub (`rules.rs:744`) to real matching (F12).
- **C5 (ruleset `load()`):** Phrasebooks resolve `@rules_cc//…` etc. (the existing registry).
- **C6 (per-language conventions):** live in Phrasebook/rule bodies, *not* the engine (S2).
- **C2/C7/C8/C9 (labels, config/platforms/toolchains, MODULE.bazel, CLI):** labels/CLI
  already parsed (`ArchBazelConstraints` notes `razel-cli` + flag table); C7 is the F12
  model above; C8 stays declarative (R11) — deferred but designed-for.

**The four invariants:**
1. **Analysis state is a passed, scoped, plural value [F19].** The `Analysis` Session
   replaces all 8 thread-locals; `Session.providers` *is* the store. Plural ⇒ concurrent
   package analysis (F7). *Honored — and it is A2's keystone.*
2. **World-effects quarantined to a content-addressed kernel, pure above [F2/F3].**
   `razel-exec` is unchanged and untouched by A2; everything above (analysis, providers,
   graph) is pure. *Honored.*
3. **Units unit-testable on pure data, no toolchain.** A rule body asserts on a captured
   `AnalyzedAction` (pure data); a provider test constructs a `ProviderInstance` and reads
   it from a fresh Session; a `select` test matches a `Config` fact against a
   `config_setting` — **all without a compiler** (kills the ~28 `exists(){return}` skips,
   S11). *Honored — typed providers make this strictly easier than today's bundle.*
4. **Front-end→IR seam (engine knows action/target/provider, never `cc_library`).** Seam #1
   above. *Honored.*

---

## 5. Honest a/b/c assessment

**(a) Modularity / smallest-edit.** *Strong.* New language = new Phrasebook file + one
manifest line + its provider types; the goal (`test`) is untouched (F16). New builtin = new
Word file + one `WORDS` line. The `rules.rs` god-module (1658 LOC, S5) dissolves into
`words/*` + `nouns/*`. **But** A2 adds a typed-provider obligation: a new provider type is a
new file *and* every consumer that wants it must name it — slightly more ceremony than the
thin design's "just stuff it in the bundle." That ceremony is the F11/R4 tax, paid on
purpose.

**(b) Testability.** *Strong, and the best of the three.* The Session-passed state makes
Words pure-testable (the `RazelDialect` §3 argument). Typed providers make the inter-unit
contract assertable as data. The unified Engine has an existing equivalence test
(incremental == from-scratch, `razel-engine/src/lib.rs:262`) we extend to analysis nodes.
Risk: the parallel engine itself needs concurrency tests (a new test burden A1 doesn't have).

**(c) Calibrated extensibility.** *This is where A2 is most exposed.* It spends the **most**
indirection of the three options — typed providers, a unified parallel graph, a config axis,
union dispatch. Per the skill rules (`ArchitectSkillRules` "two symmetric dead ends"), the
*default* AI failure is too-rigid; A2 leans hard the other way to avoid the S3/S6 retrofit
debt. The calibration claim: each relief valve sits at a seam that **provably moves**
(new language, new config, new query — all orthogonal axes named in F12/F15/F16/F17) and is
**expensive to retrofit** (Bazel/Pants both paid years). But the honest counter is in §6.

---

## 6. Characteristic dead-end risk + the scars it courts

**A2's dead-end is `too-abstract` / over-build** — the symmetric opposite of A1's
`too-rigid`. Named scars from `ArchPatterns` Part B:

- **S8 (two-language tax + bespoke dataflow compiler).** A2 *explicitly dodges the language
  half* (all Rust + Starlark, no PyO3, no second engine). But the **"bespoke dataflow"**
  half lurks: lifting analysis into a typed, parallel, config-keyed demand graph is a real
  engine, and the temptation to grow a Pants-style monomorphizing planner is exactly the
  over-build trap. *Guard:* the engine stays a **demand/restart memo graph (Skyframe-lite),
  never a static plan-solver.** No five-phase builder.
- **S9 (static-graph startup rigidity).** Pants `fail()`s on a missing/ambiguous rule at
  construction. A2 **deliberately stays softer:** Starlark stays dynamic; unknown
  builtins/providers/loads are surfaced **at analysis** (R13), not as a refusal to start;
  a missing dep *re-demands* (restart protocol), it does not abort. This is the explicit
  "softer than Pants" stance the brief asks for — we validate seams eagerly but keep the
  dynamic surface.
- **S10 (wrapper-type proliferation / type-as-wiring).** The biggest A2-specific risk:
  first-class typed providers invite a `FooInfo` for every micro-fact, and
  composition-by-type invites accidental collisions. *Guard:* providers carry **explicit
  `ProviderKey` identity** (not bare type-as-wiring); we keep composition-by-type **soft**
  (consumer names the provider) rather than Pants's automatic return-type wiring; provider
  types are reviewed like API, not minted casually.
- **Secondary — S6 (config bolted on late) is what A2 is *avoiding***, but the avoidance
  itself (deciding the full config-axis now) risks the P-G ● "analysis-graph blowup."
  *Guard:* decide the model, defer the cross-product machinery (F12 above).

**The blunt self-critique:** A2 is the option most likely to **out-build the need**. With
~0 real consumers (62 commits in, a working but thin pipeline), committing a parallel
config-keyed typed graph *now* could be generality nobody exercises for a year — the
`ArchitectSkillRules` "too-abstract" dead end. The defense is strictly the retrofit-cost
asymmetry (S3/S6 are the most expensive things to undo) and the fact that **the keystone
move (Session/provider-store/no-thread-locals) is required by A1 too** — so A2's *extra*
spend is really just "the unified graph + the config axis," and both are demand-paced in
the migration (§7). If you don't believe multi-config and parallel analysis are coming,
**A1 (thin) is the right call and A2 is over-built.** A2 is the bet that they are coming.

---

## 7. Migration from today (62 commits in)

Strangler, green at every step. The first three steps are **shared with A1** (they are the
non-negotiable invariants); A2's distinctive spend is steps 4–6, each demand-paced.

0. **Delete the dead second loader first (R14/S7).** `razel-loading/src/lib.rs` carries a
   parallel `load_build`/`TargetDecl` reachable only from its own tests, and
   `razel-analysis::analyze` is a second analysis over `TargetDecl` distinct from the live
   `AnalyzedTarget` path. **Unify on `AnalyzedTarget` before refactoring**, else the refactor
   spawns a *third* target/depset rep. (Confirmed: `lib.rs:33 TargetDecl`,
   `analysis.rs:48 analyze` vs `rules.rs:45 AnalyzedTarget` + live `wire_to_ir`.)
1. **Lexicon** — extract pure helpers to `lexicon/` + unit tests. Lowest risk, highest ROI.
2. **Session (the keystone)** — introduce `Analysis` + `session(eval)`; route the 8
   thread-locals (`STATE`/`RESULTS`/`CONFIGS`/`WORKSPACE`/`CURRENT_PKG`/`LOADED`/`GLOBAL` in
   `rules.rs` + the one in `lib.rs`) through it one at a time. **This is also A2's
   provider-store groundwork** — `Session.providers` is the typed store.
3. **Words/Nouns** — lift each builtin from `rules.rs` into `words/<x>.rs` + manifest; lift
   `Ctx`/`Args`/`Depset`/`RuleObj` into `nouns/`. `rules.rs` → `assembler.rs` (thin) +
   `analysis/orchestrate.rs`. The god-module is gone.
4. **Typed providers (A2's first distinctive step).** `nouns/provider.rs` +
   `provider_store.rs`; migrate the implicit `DefaultInfo`+`hdrs`+`cflags` bundle and the
   `dep[Info]` reconstruction (`rules.rs:498-521`) to typed `ProviderInstance`s. Make
   `rule()` consume its `attrs` schema (today `let _ = attrs;`). Consumers move from
   `t.hdrs`/`t.cflags` field-poking to `dep[CcInfo]`.
5. **Unify the Engine onto the live path (A2's biggest step; kills S7).** Route
   `build_target`→`execute` through `razel_engine::Engine` (it already drives
   `IncrementalBuilder`/daemon), **deleting the second `collect_order` deps-first loop**
   (`razel-build/src/lib.rs:191`). *Then* lift analysis into it (`ConfiguredTargetNode`),
   *then* type the `Key` and parallelize (`Send`/`Sync` + tokio) — three sub-steps, each
   green. Single-threaded-but-unified first; parallel later.
6. **Config axis + open-set dispatch (demand-paced).** Real `select`/`config_setting`
   matching (replace the `rules.rs:744` stub); `(Label, Config)` keying behind a
   single-config default until a multi-config consumer exists; generalize the ruleset
   registry to provider/goal membership for `test`/`lint`.

Each step is independently testable and reversible up to step 4; steps 4–6 are where the
typed-rich bet is actually paid, and each can stop early if the need doesn't materialize —
which is the calibration safety valve.

---

*Draft A2. The typed-rich column, single-language, softer-than-Pants. Its virtue is
maximal by-construction satisfaction of F4/F5/F6/F7/F10/F11/F12/F16/F19; its risk is
out-building the need (S8/S9/S10). Pair with draft A1 (thin) and A3 (rich-graph) for the
dial comparison.*
