# RazelCrateUniversePlan — phased, ~500-LOC steps

*2026-06-15, rev 5 (after reviews `Review55` + `Review55-1` and two owner asks — perf discipline,
then the < 20 s gate). Plan. Owner: RR.
Decomposes [`RazelCrateUniverseDesign.md`](RazelCrateUniverseDesign.md) (rev 9) into ordered
phases → steps, each ≤ ~500 LOC. This is **not** a re-design: the design's `§N` references are
the contract; this file says **what to build, in what order, how it's gated, and how big each
step is**. Where the design left a free choice (crate placement), this plan picks one and
says so.*

*Rev 2 (review 55) tightens the **graph boundary and gates**: keep `razel-query` below
`razel-build` (the load-only resolver moves into `razel-loading`, P1#1); **split raw label
capture from edge-kind resolution** (new P0.4, P1#2); define **per-output `qg` comparators**
beyond sorted label sets (P1#3); fix **explicit step dependencies** that allowed impossible
scheduling (P1#4); gate `--implicit_deps` as plumbing-only in Phase 1 (P1#5); hook capture at the
**central `record_native` seam** with a stated rule-class set (P2#1); re-scope P2.5 to
materialize-and-validate (defs.bzl eval → P3.1, P2#2); split P3.2 attr **acceptance vs semantics**
(P2#3); and define how non-`@crates` traversal targets become query nodes (P2#4).*

*Rev 3 (review 55-1) makes loading-graph capture **schema-aware** — native attrs arrive as plain
strings ([values.rs:821](../crates/razel-loading/src/values.rs)), so a value-only snapshot can't
tell a label from a string; P0.2 gains a per-`rule_class` **attr schema** (R1). It also specifies
the **capture-API contract** the central seam must carry (`record_native` takes only
`eval/label/closure` today — R2); fixes the **dependency-gate wording** so `regex` is allowed
while analysis/exec crates are banned (R3); pins **edge-resolution precedence + canonicalization**
(R4); makes stale-lock detection a **dependency of the first materialization consumer** + a
fail-before-materialize gate (R5); and names **vendored host-repos** as P5.2's dogfood-clean
node source (R6).*

*Rev 4 adds a **performance-regression discipline** (owner ask): the loading-graph capture (P0.4/
P0.5) and query adjacency (P1.3) run per-target/per-edge, so they are where an O(n²) effect would
hide. New **§Performance discipline** + a **`PL` gate**: a baseline captured **first** (P0.0), a
machine-independent **scaling assertion** (the real no-O(n²) guard), an absolute-time budget,
**per-step complexity bounds** on the hot paths, and a **stop-and-review-and-correct** protocol —
a tripped `PL` blocks the step, no silent budget-raising.*

*Rev 5 (owner ask): the `PL` gate **must run in < 20 s** so it can run on every step. The full
`xtask tfload` (whole TensorFlow corpus) is too slow, so the gate uses a **bounded synthetic-corpus
load benchmark** instead (hermetic, deterministic, exact `N`/`2N` control for the scaling
assertion, and it exercises the at-risk paths — deps chains, selects, aliases). `tfload` is
demoted to a **manual realistic-load check**, not the gate.*

## How to read a step

Every step is **test-first** (`AGENTS.md`): write the failing test, then the code. Each step
keeps the standing gates green and adds its own:

- **WS** = `cargo test --workspace` (today 86 groups) · **G** = `cargo xtask gates`
  (AD2/F13 no-ambient-state bans) · **P** = `cargo xtask probe`.
- **PL** = perf-load gate (§Performance discipline) — a **bounded synthetic-corpus load benchmark
  that runs in < 20 s**: load time within budget of the recorded baseline **and** the no-O(n²)
  scaling assertion. A trip **stops the phase**.
- **aq** = aquery analysis-parity golden — render razel's `AnalyzedAction` set →
  `razel_parity::{parse_golden,diff}` against `parity/corpus/<lang>/<case>/golden.txt`, captured
  by the goldens xtask, `omit`-allowlisted deviations (§8).
- **xp** = execution-parity golden — diff razel's build-script flags-file against Bazel's
  captured `<name>.out` (§8).
- **qg** = query golden — a **new kind**: `razel query <expr>` vs `bazel query <expr>` (§13).
  A qg case stores `{expr, flags/output-mode, comparator, deviations}`; the **comparator** is
  per-output, not one-size: `sorted_label_lines` (default `--output=label`),
  `label_kind_lines` (kind string + label per line, `--output=label_kind`),
  `node_set` (order-independent, for `allpaths`), or `shortest_path` (accept **any** valid
  minimal-length path, exact sequence only when unique — rev-9 §13). Picking the wrong comparator
  would reject valid `somepath` output, so the comparator is named in each case.

LOC is a **target, not a fence** — a step that grows past ~500 is flagged with a split.

## Performance discipline — no O(n²), load-time gated

This work touches the **hot path**: the loading-graph capture (P0.4/P0.5) runs **per declared
target**, edge resolution runs **per label reference**, and query adjacency (P1.3) runs **per
edge**. A careless lookup-by-scan turns any of these into O(n²) and silently regresses workspace
load. So the plan carries an explicit perf contract, not just correctness gates.

**The invariant.** No step introduces an O(n²)-or-worse effect in load / analysis / query.
Per-target and per-edge work stays **amortized-linear**: classification and lookups are O(1) hash
hits, never a scan over all targets; snapshots move/`Arc`-share, never deep-copy per dependent.

**The monitor — fast, bounded, hermetic (< 20 s).** The gate must run on every step, so it
**cannot** be the full `xtask tfload` (it loads the whole TensorFlow corpus — too slow, and depends
on that corpus being present). Instead P0.0 builds a **bounded synthetic-corpus load benchmark**: a
deterministic generator emits `W` packages (each a few targets with `deps` chains, a `select()`,
and an `alias` — the at-risk paths) into a temp tree, the loader loads them, wall-time recorded.
`W` is sized so the **whole `PL` run (baseline-size + the `N`/`2N` scaling pair) is < 20 s** (target
~10 s); the gate itself **fails if it exceeds 20 s**, so it can't silently rot slow. It **reuses**
`xtask stress`'s LOUD-failure pattern ([stress.rs](../xtask/src/stress.rs)) and registers as an
`xtask probe` sentinel ([probe.rs:20](../xtask/src/probe.rs)). The existing `xtask tfload`
([tfload.rs:117](../xtask/src/tfload.rs)) stays as a **manual, realistic-corpus** sanity check —
useful, but not the gate.

**The `PL` gate has two parts:**
- **Scaling assertion (the real no-O(n²) guard — machine-independent).** Generate the synthetic
  corpus at size *N* and *2N* targets and load each; assert wall-time grows **~linearly** —
  `t(2N) / t(N) ≤ ~2.3` (a 4× blow-up = O(n²) → FAIL). Ratio-based, so it holds across machines and
  is the primary signal. (Synthetic `N` is the reason this is fast and exact — no whole-corpus run.)
- **Absolute-time budget (constant-factor watch).** The bounded-benchmark load wall-time ≤ recorded
  baseline + **10%**. Catches non-O(n²) constant-factor slowdowns. Machine-relative, so it's a
  soft watch on top of the hard scaling assertion.

**Stop-and-review-and-correct (the protocol the owner asked for).** A tripped `PL` **blocks the
step from being marked done and blocks the next step from starting**. The response is: (1) profile
the hot path (which lookup went super-linear / which copy got deep), (2) correct it, (3) re-run
`PL`. The budget is **not** silently raised — a baseline bump requires a justified, approved reason
recorded in the step. Regressions are corrected where introduced, never accumulated to a "perf
pass" later.

**Per-step complexity bounds (the at-risk hot paths):**

| Step | Hot path | Required bound |
|------|----------|----------------|
| **P0.4** edge resolution | classify each raw label ref | **O(1)** hash lookup per ref in `output_index`/`aliases`/`config_specs` → **O(refs)** total — *never* a scan over targets per ref |
| **P0.5** capture-at-load | snapshot per declared target | **O(attrs)** per target, snapshot by move/`Arc` → **O(Σ attrs)**; no per-dependent re-walk |
| **P1.2** load-only enumerate | package discovery | **O(packages)** dir walk (the existing one) |
| **P1.3** query adjacency + BFS | build index; `deps`/`rdeps` | build **O(V+E)** once; each `deps`/`rdeps` **O(V+E)**; `rdeps` bounded by its `universe`; not re-derived per node |
| **P5.2** external traversal | load vendored slice | slice loaded **once, memoized** → **O(slice)**, not per-edge reload |

`PL` runs from P0.4 onward (the first step that changes load behavior) and is a DoD item for
Phase 0 and Phase 1.

## Crate placement (decided here)

The design pins behavior, not modules. This plan places:
- **loading-phase types** (`LoadedTarget` / `RawAttr` / `QueryNode` / `Edge`) in a new
  `crates/razel-loading/src/loaded.rs` — the loader populates them, so they live with it.
- **the lock reader** in a new `crates/razel-loading/src/lock.rs` (+ `recorded.rs` for the
  `recordedInputs` grammar) — it feeds the same loader.
- **the load-only pattern resolver** (package discovery + load-only `expand_pattern`) in a new
  `crates/razel-loading/src/patterns.rs` — **not** in `razel-build`. Today `expand_pattern`/
  `discover_packages` live in `razel-build` but already delegate to the loader's
  `load_tree_report_with_targets` ([rules.rs:1000](../crates/razel-loading/src/rules.rs)); the
  resolver is moved down so both `razel-build` (analyze mode) and `razel-query` (load-only) call
  it without `razel-query` pulling in the build driver (P1#1).
- **the query engine** (parser, adjacency, evaluator, output) in a **new `crates/razel-query`**
  crate depending on `razel-loading` + `razel-core` + leaf utilities (`regex` for the §12
  predicates, P1.4). The boundary is stated as a **denylist, not an allowlist**: it must **not**
  depend on `razel-build`, `razel-analysis`, `razel-exec`, or `razel-engine` (those pull in the
  analysis/execution path the "query never analyzes" boundary forbids). The CLI verb `cmd_query`
  lives in `razel-cli` and calls `razel-query`.
- **the rustc wrapper** as a **new thin bin crate `crates/razel-rustc-wrapper`** (a
  `process_wrapper` analogue).

Layering stays strictly downhill (`razel-query → razel-loading → razel-core`, with `razel-build`
off to the side as the build driver); the S0 gate (no `razel-*` → `grazel-*`/iroh edge) is
unaffected. **A new gate (P1.0) asserts `razel-query`'s manifest carries no analysis/exec/engine
dependency** so the boundary can't regress silently.

---

## Current state — the grounded gap map

What exists today, with the anchor the steps build from (verified by reading the tree, not the
design):

| Area | State | Anchor |
|------|-------|--------|
| `MODULE.bazel.lock` reader | **MISSING** | — |
| repo fetch/materialize | **PARTIAL** — `fetch.rs` records `RepoSpec` (name/kind/attrs); no download/extract/patch, no `RepoFetch` action | `razel-loading/src/fetch.rs:35,80` |
| generated-BUILD load surface | **PARTIAL** — synthesizes only `@rules_rust//rust:defs.bzl` (`rust_library`/`rust_binary`/`rust_shared_library`/`rust_library_group`/`rust_doc`); no `cargo:defs.bzl`, `cargo_build_script`, `cargo_toml_env_vars`, `selects.bzl` | `razel-loading/src/rust_rules.rs:295` |
| `cargo_build_script` run | **MISSING** — no directive capture | — |
| `rust_proc_macro` | **MISSING** (cdylib dylib-suffix path exists) | `razel-loading/src/toolchains.rs:40` |
| `select()` / `target_compatible_with` | **PARTIAL** — `selects.rs` resolves `config_setting` labels vs `Session.config_specs`; **no** triple→`@platforms`/`@rules_rust//rust/platform` synthesis; **no** `target_compatible_with` | `razel-loading/src/selects.rs:122,179` |
| `rust_library` attr surface | **PARTIAL** — only `name`/`srcs`/`deps`/`edition`; **unknown kwargs silently discarded** (`_kw`); no loud-error path | `razel-loading/src/rust_rules.rs:87` |
| `DepInfo` | **EXISTS** — `{libs, canon, fields}`; no `crate_name`/`proc_macro`/`BuildScriptRun` | `razel-loading/src/deps.rs:38` |
| tree/dir output capture | **PARTIAL** — per-declared-file `fs::copy`; no directory/`OUT_DIR` | `razel-exec/src/lib.rs:56,69` |
| rustc wrapper | **MISSING** — rustc argv emitted directly | `razel-loading/src/rust_rules.rs:105` |
| **loading-phase graph** | **ABSENT** — load goes straight to `AnalyzedTarget`; no `LoadedTarget`/`RawAttr`/`QueryNode`, no retained unresolved-select | `razel-loading/src/state.rs:23`, `razel-analysis/src/analysis.rs:24` |
| `TargetKind` | **EXISTS** — `Library`/`Binary`/`Test`, inferred from name suffix | `razel-ir/src/lib.rs:26`, `razel-analysis/src/analysis.rs:14` |
| `expand_pattern` / `discover_packages` | **EXISTS but LOAD+ANALYZE** in one pass (`load_tree_report_with_targets(…,1)`); no load-only mode | `razel-build/src/lib.rs:90,111,125` |
| `query` CLI verb | **MISSING** — verbs: build/run/test/clean/affected/subscribe/version/daemon | `razel-cli/src/lib.rs:48` |
| parity harness | **EXISTS** — `normalize`/`parse_golden`/`diff`, corpus `parity/corpus/{cc,java,rust}/<case>/golden.txt` | `razel-parity/src/lib.rs:18,151,253` |
| xtask gates | **EXISTS** — substring bans (AD2/F13/S0/lang-leak) | `xtask/src/main.rs:249` |

**Net:** the loader, native-rule action synthesis, `select()` resolution, and the aquery parity
harness are solid. The five big holes are the **loading-phase layer**, the **lock reader +
fetch**, the **`cargo_build_script` + wrapper** pipeline, **`rust_proc_macro`**, and the **query
verb**.

---

## Sequencing & critical path

Per design §10 *build-order sequencing*, the shared §11 layer is built once, first; query on the
workspace validates it cheaply before the expensive `@crates` work:

```
Phase 0  loading-phase graph (§11)         ← prerequisite for BOTH features
   │
   ├──────────────► Track Q (query)        ├──────────────► Track A (@crates build)
   │   Phase 1  q1–q3 over the workspace    │   Phase 2  lock reader + fetch
   │            (proves §11 is faithful)    │   Phase 3  load surface + blake3 (milestone 1)
   │                                        │   Phase 4  proc-macro, richer select, full @crates
   └────────────────────────┬───────────────┘
                             ▼
                  Phase 5  q4 over @crates + dogfood binaries
                             ▼
                  Phase 6  named-deferred backlog (q5+, cross-compile, …)
```

**Tracks Q and A are independent after Phase 0** and can proceed in parallel (different files:
Track Q is `razel-query` + the `expand_pattern` fork; Track A is `lock.rs`/`fetch.rs`/
`rust_rules.rs`/`razel-exec`). They converge at **Phase 5** (q4 needs both the §11.2 select-condition
edges *and* `@crates` materialization). Critical path to the headline result (`razel build
@crates//:blake3`) runs **Phase 0 → Phase 2 → Phase 3**; query can lag without blocking it.

---

## Phase 0 — Loading-phase graph foundation (the shared substrate)

**Goal:** the loader retains a serializable, de-Starlark'd loading-phase node (raw attrs +
**unresolved** `select()` + label-edges) per declared target, *before* analysis — the §11/§5.6
shared-loader obligation. P0.0 captures the perf baseline **first**; steps P0.1–P0.3 are pure
additions (no behavior change, fully unit-tested); P0.4 is a post-declaration resolution pass;
P0.5 is the one invasive wiring step. **Capture (raw refs) is split from resolution (typed
edges)** because edge kinds aren't knowable at value-snapshot time (P1#2 — see P0.4).

- **P0.0 — Bounded perf benchmark + `PL` gate (< 20 s)** · ~300 · dep: — · §Performance discipline
  Before any loader change, build the **bounded synthetic-corpus load benchmark** (a new
  `xtask perfgate`, or a `razel-loading` bench): a deterministic generator emits `W` packages with
  `deps` chains / a `select()` / an `alias` into a temp tree; the loader loads them; wall-time
  recorded. Pick `W` so the **whole run is < 20 s** (target ~10 s), and **fail if it exceeds 20 s**.
  Record the baseline to a checked-in `perf-baseline.json`, and register the **absolute-time budget**
  (≤ baseline + 10%) + the **scaling assertion** (`t(2N)/t(N) ≤ ~2.3` at synthetic `N`/`2N`) as an
  `xtask probe` regression sentinel ([probe.rs:20](../xtask/src/probe.rs)). Gate: **P** (the sentinel
  runs green at baseline); **unit** (the generator is deterministic; the scaling check flags an
  injected O(n²) stub); **PL** is now defined for every later step. *(`xtask tfload` stays a manual
  realistic-corpus check.)*
  *This must land first* — every step from P0.4 on is measured against this baseline.

- **P0.1 — `RawAttr` model + canonical stringification** · ~300 · dep: P0.0 · §11.1
  `razel-loading/src/loaded.rs` (new): the closed `RawAttr` enum (`Str/Int/Bool/None/List/Tuple/
  Dict/Label/Select/Concat`) + the normative Starlark-`repr` stringify (double-quote escaping,
  **source-order** dicts, `select({cond: v,…}, default=…)`, `Concat` = `a + b`). Gate: **unit**
  goldens on the stringify incl. idempotence; **WS**.

- **P0.2 — Starlark→`RawAttr` snapshot + SCHEMA-AWARE raw label extraction** · ~400 · dep: P0.1 ·
  §11.1
  `loaded.rs`: `Value → RawAttr` capture (snapshots `select()`/`+` **unresolved**, no heap
  escape). **Label extraction must be attr-schema-aware (R1):** native rule attrs arrive as plain
  Starlark **strings**, not `Label` objects (`deps = ["//:x"]` → `unpack_strs`,
  [values.rs:821](../crates/razel-loading/src/values.rs)), so a value-only walk cannot tell a
  label-valued `deps` element from a string attr or a `srcs` source label. So P0.2 also defines a
  per-`rule_class` **attr schema** classifying each attr; extraction emits a raw label reference
  `{label, attr, role, in_select_condition: bool}` **only for label-valued attrs**, leaving edge
  *kind* to P0.4:

  | attr role | examples | extracts |
  |-----------|----------|----------|
  | `label_list` (deps) | `deps`, `proc_macro_deps`, `link_deps` | each element → label ref |
  | `label_keyed` | `aliases` (dict label→name) | each key → label ref |
  | `srcs`/source labels | `srcs`, `compile_data`, `data` | label refs (role = source-candidate) |
  | `string`/`string_list`/`int`/`bool` | `crate_name`, `edition`, `rustc_flags`, `tags` | no label refs (raw value only) |

  Schema source: for **native rules** it's the rule's known signature (the loader already knows
  `deps`/`srcs` are label lists); for **Starlark-defined rules** it's the `attr.label`/`label_list`
  declarations. Gate: **unit** — `deps=["//:x"]` yields a label ref but `tags=["x"]` does not; a
  `select`-valued `deps` yields its condition labels (flagged) + all arm values + default; `None`
  → nothing.

- **P0.3 — `LoadedTarget`/`QueryNode`/`Edge` types + kind strings** · ~250 · dep: P0.2 · §11.1/§12
  `loaded.rs`: the node structs; `Edge { to, kind, attr }` with the kind enum (`Rule/Alias/
  SourceFile/GeneratedFile/ConfigSetting/Implicit`); `rule_class: String` (the registered macro
  name, retained at load — **not** the coarse `TargetKind`); the `"<rule_class> rule"` /
  `"source file"` / `"generated file"` kind string used by `kind()`/`label_kind`. `Send` +
  snapshot-friendly. Gate: **unit** on kind-string formatting.

- **P0.4 — Edge-kind resolution pass (post-declaration)** · ~350 · dep: P0.2,P0.3 · §11.2
  Resolve each raw label reference (P0.2) into a typed `Edge` **after** the package's declarations
  are registered, using `output_index`/`aliases`/`config_specs`
  ([state.rs:181-185](../crates/razel-loading/src/state.rs)) + a source-file lookup. **Pinned
  precedence + canonicalization (R4)** — an alias is *also* a declared target, so order is not
  arbitrary. First **canonicalize** each raw label relative to its declaring repo/package
  (`@crates//:x` and a bare `:x` both → the canonical form, §11.3), then classify in this order:
  1. `aliases` hit → **`Alias`** (carry the actual) — *before* declared-rule, else aliases
     misclassify as `Rule`;
  2. `output_index` hit → **`GeneratedFile`** (→ the generating rule);
  3. reached via a `select()` condition (`in_select_condition`) + `config_specs` hit →
     **`ConfigSetting`**;
  4. declared rule target → **`Rule`**;
  5. package source file on disk → **`SourceFile`**;
  6. otherwise → **loud error** (unresolved label).
  Runs in `razel-loading` (it needs session state); query reads the resolved edges. Gate: **unit**
  — one fixture per branch, incl. the alias-before-rule ordering case; **PL** — each ref is an
  **O(1)** index lookup, **O(refs)** total, never a scan over targets (§Performance).

- **P0.5 — Capture-at-load wiring + the capture API (the invasive step)** · ~450 · dep: P0.4 ·
  §5.6 (shared-loader obligation), §11
  Hook the **central declaration seam** so *every* target-creating path captures a `LoadedTarget`,
  retained per package **alongside** the existing `AnalyzedTarget` path (analysis unchanged;
  additive). **The capture API (R2):** today `record_native` carries only `(eval, label, closure)`
  ([decls.rs:647](../crates/razel-loading/src/decls.rs)) — not enough. Add a sibling/builder
  ```
  record_loaded_target(eval, label, rule_class, raw_attrs, declared_outputs, side_effects)
  ```
  where `raw_attrs` is the P0.2 snapshot, `declared_outputs` lets P0.4 resolve `GeneratedFile`
  back-edges, and `side_effects` carries the alias/config-setting metadata that builtins write
  **directly to side indexes today** — `genrule`→`output_index`, `alias`→`aliases`,
  `config_setting`→`config_specs` ([dialect.rs:557,699,655](../crates/razel-loading/src/dialect.rs))
  — so P0.4 reads one consistent source, not scattered cells. The native rules (`rust_*`, `cc_*`,
  `js_*`, `py_*`, `sh_*`), the dialect builtins, and Starlark-rule declarations all route through
  it — **not** `rust_rules` alone (P2#1). Initial covered rule classes: native `rust_*` + `cc_*` +
  `alias`/`config_setting`/`filegroup`/`genrule`; the q1 corpus is a **mixed-rule** workspace
  package (not Rust-only) so a narrow pass can't hide gaps. Gate: **unit** — every q1-corpus target
  has a round-tripping `LoadedTarget` (incl. a `genrule` output and an `alias`); **WS** green (no
  analysis regression); **G**; **PL** — capture is **O(attrs)** per target via move/`Arc`, no
  per-dependent re-walk (§Performance). *This is the most likely O(n²) site — `PL` is mandatory here.*
  *Split if it grows:* land the API + `record_native` hook first, the dialect/Starlark-rule paths
  second.

**Phase 0 DoD:** loading the mixed-rule q1 workspace corpus yields faithful `LoadedTarget`s (raw
attrs, unresolved selects, **resolved** typed edges, `rule_class`) via the central seam, with the
existing build path untouched, **and `PL` green** — capture adds no super-linear effect to
workspace load (the bounded-benchmark scaling assertion holds; load within +10% of baseline).
*(@crates rules are captured in Phase 3 when those natives exist.)*

---

## Phase 1 — `razel query` over the workspace (q1–q3) · Track Q

**Goal:** the query language over the §11 workspace graph, with **no analysis** — validating the
shared layer before `@crates`. New crate `razel-query`.

- **P1.0 — `razel-query` skeleton + boundary gate** · ~150 · dep: — · (P1#1, R3)
  Create the `razel-query` crate (deps: `razel-loading` + `razel-core` + leaf utilities such as
  `regex`) and add a `cargo xtask gates` rule phrased as a **denylist** — it **fails if
  `razel-query`'s manifest gains a `razel-build`/`razel-analysis`/`razel-exec`/`razel-engine`
  dependency** (the "query never analyzes" boundary), **not** an allowlist that would also reject
  `regex` (R3). Enforced via `xtask/src/main.rs:249`'s ban list. Gate: **G** (the new rule);
  **unit** (a fixture manifest with a forbidden dep trips it; one with `regex` does not).

- **P1.1 — Expression parser** · ~400 · dep: P1.0 · §12/§13
  `razel-query/src/parse.rs`: recursive-descent over §12's grammar → an expression AST (word vs
  `"quoted"` tokens, `deps(x[,depth])`/`rdeps`/`kind`/`somepath`/`allpaths`/`filter`/`attr`/
  `labels`, set algebra `+ - intersect union`, `let v = e in e`, parens). Gate: **unit** parse
  goldens.

- **P1.2 — Load-only pattern resolver in `razel-loading`** · ~300 · dep: P0.5 · §13/(P1#1)
  Move package discovery + `expand_pattern` **down into `razel-loading/src/patterns.rs`** and add a
  **load-only** mode: it enumerates §11 `LoadedTarget`s (same BUILD/`.bazelignore` resolution) and
  stops before analysis (§3 step 4). `razel-build`'s existing `expand_pattern`
  ([lib.rs:90](../crates/razel-build/src/lib.rs)) becomes a thin wrapper delegating in analyze
  mode; `razel-query` calls the load-only mode — so query never imports the build driver.
  Package-load errors reported, not swallowed. Gate: **unit** — load-only enumerates the same
  labels as analyze-mode without minting actions; **WS** (build path unchanged).

- **P1.3 — Query adjacency + core evaluator** · ~400 · dep: P1.1,P1.2,P0.4 · §13
  `razel-query/src/graph.rs`+`eval.rs`: build the query's **own** forward/reverse adjacency over
  `LoadedTarget` **resolved edges** (P0.4) — the bidirectional-index *pattern*, **not**
  `razel_ir::Graph`; BFS `deps`/`rdeps` (cycle-safe, depth-bounded, `rdeps` bounded by universe);
  set ops; `let` environment. Gate: **unit** on a synthetic graph; **PL** — adjacency built
  **O(V+E)** once, each traversal **O(V+E)**, not re-derived per node (§Performance).

- **P1.4 — Predicates + output + sort** · ~350 · dep: P1.3 · §12/§13
  `kind(regex,x)`/`filter(regex,x)`/`attr(name,regex,x)` (over the §11.1 stringification)/
  `labels(attr,x)` (the narrower, `includeSelectKeys=false` per-attr view); `--output=label`
  (default)/`label_kind`; **lexicographically-sorted** label-set output (the `--order_output=auto`
  analogue); regex = Rust `regex` crate (allowlisted dialect deviation). Gate: **unit**.

- **P1.5 — `somepath`/`allpaths`** · ~300 · dep: P1.3 · §13
  `somepath` = a BFS shortest path (razel pins its own sorted edge order for determinism);
  `allpaths` = the forward∩reverse **node-set**. Gate: **unit** — `somepath` asserts *a* path of
  minimal length (exact sequence only when unique → the `shortest_path` comparator); `allpaths` is
  an order-independent `node_set`.

- **P1.6 — `cmd_query` CLI verb** · ~250 · dep: P1.4,P1.5 · §13
  New verb in `razel-cli`: parse → evaluate → format; results→**stdout**, progress→**stderr**;
  `--output` + `--[no]implicit_deps` flags. In slice 1 `--implicit_deps` emits the **same explicit
  graph** as `--noimplicit_deps` plus a logged deviation (no native-rust implicit labels exist yet,
  §13) — the flag is **plumbed, not parity-gated** here (P1#5). CLI-local only (no daemon). Gate:
  **unit** — the flag parses and reaches the evaluator options; **WS**.

- **P1.7 — Query-golden harness + q1–q3 workspace corpus** · ~400 · dep: P1.6 · §13
  The **new `qg` golden kind** (`parity/corpus/query/<case>/{cmd,golden.txt}`, with the per-output
  comparator from *How to read a step*): capture `bazel query <expr>` via the goldens xtask. **All
  Phase-1 goldens run `--noimplicit_deps`** (decouple from implicit-label fidelity — Bazel
  `--implicit_deps` parity is the deferred implicit-label rung, Phase 6). Author q1 (patterns +
  `kind` [`label_kind_lines`] + `label`), q2 (`deps`/`rdeps` + set ops — **`--noimplicit_deps`
  only**, plus a unit smoke test that the `--implicit_deps` flag plumbs), q3 (`somepath`
  [`shortest_path`] / `allpaths` [`node_set`] + `attr`/`labels`). Gate: **qg** green on the
  workspace.

**Phase 1 DoD:** q1–q3 green over the workspace graph vs `bazel query --noimplicit_deps` (named
deviations only); the `razel-query`→analysis boundary is gate-enforced; **`PL` green** (query
adjacency + traversal stay linear); the §11 layer is proven faithful before any `@crates` cost.

---

## Phase 2 — Lock reader + repo materialization · Track A

**Goal:** make `MODULE.bazel.lock` the source of truth and materialize both repo kinds (§2, §5.1,
§4.4). No build yet.

- **P2.1 — `MODULE.bazel.lock` reader schema** · ~400 · dep: — · §2.1/§2.2
  `razel-loading/src/lock.rs` (new): parse `moduleExtensions[<crate key>]` (the exact
  `@@rules_rust+//crate_universe:extensions.bzl%crate` string, **version-aware** on
  `lockFileVersion`, tolerant of a trailing `%<isolationKey>`), `generatedRepoSpecs` → root +
  per-crate `RepoSpec`s. All off-schema / missing-required → **loud error**. Gate: **unit** over a
  captured lock fixture (incl. the version-mismatch error).

- **P2.2 — `recordedInputs` grammar parser** · ~300 · dep: — · §2.3
  `razel-loading/src/recorded.rs` (new): the escaped tagged-string format
  `<PREFIX>:<id> <value>` with unescape (`\s`→space, `\n`→newline, `\0`→null) and the five
  prefixes (`FILE` value ∈ {`DIR`,`ENOENT`,hex-sha256}, `DIRENTS`/`DIRTREE`, `ENV`, `REPO_MAPPING`).
  Gate: **unit** golden over a captured `recordedInputs` block incl. escaped/null values.

- **P2.3 — Stale-lock detection** · ~200 · dep: P2.1,P2.2 · §2.3
  Rehash the `FILE`/`DIR*` workspace inputs and compare sha256; mismatch → **loud error**
  ("regenerate offline"); `ENV`/`REPO_MAPPING` are informational (documented divergence). Gate:
  **unit** — fresh passes, mutated `Cargo.toml` errors.

- **P2.4 — Tree / directory output capture in `razel-exec`** · ~400 · dep: — · §5.2/§10
  Add a directory output mode (walk + content-address a tree) beside the per-file `fs::copy`
  (`razel-exec/src/lib.rs:56,69`, `sandbox.rs:124`) — needed by extracted repo trees (P2.6) and
  `OUT_DIR` (P3.8). Gate: **unit** — a directory output round-trips through the cache deterministically.

- **P2.5 — Root `@crates` materialization (files only)** · ~250 · dep: P2.1 · §2.2/§5.1
  Write `attributes.contents` (`BUILD.bazel`/`defs.bzl`/`alias_rules.bzl`) to the repo tree (no
  fetch) and **validate their presence/structure** — *not* evaluating them. Loading `defs.bzl`
  (which `load()`s helper modules) and resolving the root `alias()` to the canonical label need
  the §5.6 load surface, so that behavior lands in **P3.1**, not here (P2#2): Phase 2 has no load
  surface yet, and `@crates` alias resolution is first *needed* by the build (Phase 3) and q4
  (Phase 5), never by Phase-1 workspace query. Touch `fetch.rs`. Gate: **unit** — the three root
  files are written and well-formed.

- **P2.6 — Per-crate `RepoFetch` action (stale-gated)** · ~450 · dep: P2.1,P2.3,P2.4 · §5.1/§4.4
  A `RepoFetch` action: **first run P2.3 stale-lock detection** (loud error *before* any download —
  the materialization entry is where the lock is read, so the "never silently builds a stale graph"
  contract attaches here, R5), then download the `.crate` (`urls`, **sha256 verified**), extract,
  apply `strip_prefix` + patches (incl. `remote_patch_strip`), drop in `build_file_content` as the
  package `BUILD`; content-addressed by the **full §4.4 key** (urls/sha/strip/patch-bytes/
  generated-BUILD/repo-mapping/tool-version). Gate: **unit** — a fixture crate materializes; a
  changed generated-BUILD with unchanged url/sha **invalidates**; **a stale lock errors before any
  fetch**.

- **P2.7 — `--crate-repo-cache` interim mode** · ~200 · dep: P2.5 · §5.1
  Read Bazel's already-fetched `external/` tree to defer download/extract while Phase 3 lands.
  **Dev-only, NOT parity-gating** (must be retired before milestone 4, P4.6). Gate: **unit** — reads
  a Bazel external dir; a log line marks it non-parity.

**Phase 2 DoD:** lock + `recordedInputs` parsers golden-tested; stale-lock loud-errors; a fixture
crate's repo materializes via **pure `RepoFetch`** and via the interim cache; directory outputs
captured. The **stale gate lives at the `RepoFetch` entry (P2.6)**, so both `razel build
@crates//:blake3` (P3.11) and `razel query @crates//...` (P5.3) inherit "fail before materializing
on a stale lock" — those integration gates assert it explicitly rather than re-implementing the
check (R5).

---

## Phase 3 — Generated-BUILD load surface + blake3 build (milestone 1) · Track A

**Goal:** load and build `@crates//:blake3` end-to-end — the core bet. Covers §2.2/§5.1/§5.2/
§5.4/§5.5/§5.6/§6/§8.

- **P3.1 — `cargo:defs.bzl` module + `selects.bzl` + external `glob()`/`alias()` + root `defs.bzl`
  eval** · ~400 · dep: P0.5,P2.5,P2.6 · §5.6/§2.2
  A synthetic `cargo:defs.bzl` exposing the `cargo_build_script`/`cargo_toml_env_vars` natives
  (bodies in P3.5/P3.6), a `crate_universe/private:selects.bzl` passthrough, `glob()` run against
  the **materialized external tree** (not the workspace), and `alias()` over §4.3. **Also** the
  piece deferred from P2.5: load the root `@crates//:defs.bzl` (the `all_crate_deps`/`aliases`
  macros — plain Starlark) and resolve the root `alias()` (`@crates//:X` → canonical
  `@@rules_rust++crate+crates__<name>-<ver>//:X`, §11.3). Touch `rust_rules.rs:295` (`module()`).
  Gate: **unit** — blake3's `load(...)`s resolve, the package *loads*, and `@crates//:blake3`
  resolves to the canonical label.

- **P3.2 — `rust_library` attr surface: accept all, implement the compile-affecting set** · ~450
  · dep: P0.5,P3.1 · §5.5
  Replace the silent `_kw` discard (`rust_rules.rs:87`) with the §5.5 verdict table, in **three
  explicit buckets** so "accept" ≠ "implement semantics" (P2#3):
  - **compile-affecting NOW (this step):** `crate_root`, `srcs`, `crate_name`, `edition`,
    `crate_features`, `rustc_flags`, `rustc_env`, `rustc_env_files`, `compile_data`, `version`,
    `pkg_name`, and `aliases` for normal rlib deps (`--extern <alias>=…`). These shape the rustc
    argv here.
  - **accepted + recorded, semantics DELEGATED (no loud-error, no action effect yet):**
    `proc_macro_deps` → **P4.1**, `target_compatible_with` → **P3.4**, `link_deps` `DEP_*` →
    **P4.5**. The attr is captured into the `LoadedTarget` and validated; its action-graph effect
    lands in the named later step.
  - **ignored (recorded parity deviation):** `data`, `tags`, `visibility`.
  - **loud-error:** any other attr. `--cap-lints allow` always.
  Gate: **unit** — a compile-affecting attr changes the argv; a delegated attr is accepted but
  argv-inert (a regression test pins that, so the delegation can't silently become a no-op
  forever); `tags` does *not* error; an unknown attr does.
  *Split if it grows:* compile-affecting set first, the env/recorded attrs second.

- **P3.3 — Condition source from the target triple** · ~300 · dep: — · §5.4
  Synthesize the `@platforms//cpu:*`/`os:*` + `@rules_rust//rust/platform:*` `config_setting`s from
  the configured triple into `selects.rs`'s condition store (`selects.rs:179`) so blake3's
  `target_compatible_with` select resolves. A subset suffices; expand as goldens demand. Gate:
  **unit** — host triple resolves the expected arm.

- **P3.4 — `target_compatible_with` handling** · ~300 · dep: P3.2,P3.3 · §5.4
  Evaluate vs the platform: incompatible → **no actions**, **skipped** in wildcard/transitive,
  **loud error** on an explicit request; an incompatible **dep** of a compatible target → **loud
  error** (never silent drop). Gate: **unit** — compatible builds; incompatible is absent in
  wildcard, errors when named.

- **P3.5 — `cargo_toml_env_vars` native + env-file format** · ~300 · dep: P3.1 · §6.2
  A native target emitting a newline `KEY=VALUE` env-file of `CARGO_PKG_*` from the crate's
  `Cargo.toml`; precedence (literal `rustc_env`/`version`/`pkg_name` override; among
  `rustc_env_files`, last wins). Gate: **unit** golden on the env-file + precedence.

- **P3.6 — `cargo_build_script` native: compile (action 1)** · ~350 · dep: P3.1,P3.2 · §5.2
  Compile `build.rs` → a host `rust_binary` (`<crate>_bs`) with the single toolchain; the
  compile-phase attr split (`deps`/`srcs`/`crate_root`/`crate_features`/`rustc_flags`). Gate:
  **unit** — the bin action mints with the right `--extern`s.

- **P3.7 — Build-script flags-file parser** · ~400 · dep: — · §6.1
  Parse `cargo:`/`cargo::` directives → JSONL `{"kind","args"}` in **emission order with
  duplicates**; **tokenize `rustc-flags` at parse time**; `metadata=K=V` → `DEP_*` record;
  `warning=`→stderr, `error=`→fail; `rerun-if-*` recorded (not yet narrowing); unknown reserved →
  recorded + one deviation line. Gate: **unit** goldens over captured stdout (≥1 line per `kind`).

- **P3.8 — `CargoBuildScriptRun` action (run, action 2)** · ~350 · dep: P3.6,P3.7,P2.4 · §5.2
  The run action: `argv=[bs bin]`, inputs (bin + crate srcs/build-dep rlibs + declared data),
  outputs (flags-file + **`OUT_DIR` tree** via P2.4), **default-deny env** = the Cargo allowlist
  (`CARGO_PKG_*`, `CARGO_FEATURE_*`, `OUT_DIR`/`TARGET`/`HOST`/`OPT_LEVEL`/`CARGO_CFG_*`, the cc
  `CC`/`AR`/`CFLAGS`, `DEP_<LINKS>_*`). Gate: **unit** — the action's declared env/outputs match
  the contract.

- **P3.9 — rustc wrapper binary** · ~350 · dep: P3.7 · §4.1/§6.1/§7
  New `crates/razel-rustc-wrapper`: read the flags-file → append `--cfg`/`-l`/`-L`/`-C link-arg`/
  env + point `OUT_DIR`, per the normative `kind`→rustc mapping; explicit-in-argv shape
  `[<wrapper>, --rustc=…, --flags-file=…, --env-file=…, --, <rustc args…>]`. Gate: **unit** golden
  on the mapping; an empty flags-file is a no-op passthrough.

- **P3.10 — Wire the build-script edge** · ~300 · dep: P3.8,P3.9 · §4.3
  `DepInfo` += a `BuildScriptRun` projection (`{flags_file, out_dir, build-dep closure}`, no
  `libs`); analysis inspects each dep — a `BuildScriptRun` dep becomes the **intra-target** edge
  (its flags-file + `OUT_DIR` → the rustc wrapper's inputs), every other dep stays a normal
  `--extern`; `:build_script_build` is never passed as `--extern`. Touch `deps.rs:38` +
  `rust_rules.rs`. Gate: **unit** — the lib's rustc action consumes the flags-file, not an extern.

- **P3.11 — blake3 analysis-parity golden** · ~250 (+normalizer) · dep: P3.4,P3.10 · §8
  New corpus case `parity/corpus/rust/crate_blake3/`: capture `bazel aquery 'deps(@crates//:blake3)'`;
  add the **wrapper-prefix normalizer** (strip `<wrapper> --rustc=…--` + flags/env-file paths to a
  canonical token) to `razel_parity::normalize`; `omit`/deviation-allowlist `process_wrapper`/
  `-Cmetadata`/`--extra-filename`/`--remap-path-prefix`. Gate: **aq** green (documented deviations
  only).

- **P3.12 — blake3 execution-parity golden** · ~250 · dep: P3.9,P3.10 · §8
  Capture Bazel's `<name>.out` flags-file + `OUT_DIR` (`bazel build @crates//:blake3__build_script_build`);
  diff razel's flags-file (§6.1) against it; assert the rlib exists and the SIMD `.o`s appear in
  `OUT_DIR`. (Producing the rlib needs the wrapper P3.9 + the build-script edge P3.10, which in
  turn carry the P2.5/P2.6 materialization transitively.) Gate: **xp** green.

**Phase 3 DoD (milestone 1):** `razel build @crates//:blake3` produces the rlib; **aq** + **xp**
green; SIMD `.o`s in `OUT_DIR`. Closes the §10 risks *tree-output*, *rustc wrapper*, *load surface*.

---

## Phase 4 — proc-macro, richer select, scale to full `@crates` (milestones 2–4) · Track A

- **P4.1 — `rust_proc_macro` native** · ~350 · dep: P3.2 · §5.3
  `--crate-type proc-macro` → `lib<name>.{dylib,so}` (single toolchain, host dylib suffix);
  dependents `--extern <name>=<dylib>`; `DepInfo` += a `proc_macro` projection. Gate: **unit**.

- **P4.2 — serde_derive parity golden** · ~150 · dep: P4.1 · §9.2
  Corpus case `crate_serde_derive`. Gate: **aq** green. *(Milestone 2.)*

- **P4.3 — Richer `select()` (per-cfg platform deps)** · ~300 · dep: P3.3 · §5.4/§9.3
  Beyond rung-1 compatibility gating: per-cfg dep selection (`libc`/`getrandom`), expanding the
  `@platforms`/`@rules_rust//rust/platform` subset as goldens demand. Gate: **unit**.

- **P4.4 — libc/getrandom parity + incompatible negative case** · ~200 · dep: P4.3,P3.4 · §9.3
  Corpus cases incl. the **required** incompatible-target golden (absent in wildcard / loud-error
  when named). Gate: **aq** green. *(Milestone 3.)*

- **P4.5 — `link_deps` `DEP_*` propagation** · ~300 · dep: P3.10 · §5.5/§6
  Propagate `links`-crate native link flags + `DEP_<LINKS>_*` via a `DepInfo` projection to the
  consuming `rust_binary`'s **final link** (not the rlib compile); the cross-build-script channel.
  Gate: **unit** — a consumer's link action carries the propagated `-l`/`DEP_*`.

- **P4.6 — Full `@crates` scale + retire interim cache** · ~iterative · dep: P4.1–P4.5 · §9.4
  The resolved graph builds end-to-end; **aq** green across it (documented deviations only); the
  **pure `RepoFetch` path** (not P2.7's interim cache) is the one under the goldens. Gate: **aq**
  across the `@crates` corpus. *(Milestone 4.)*

**Phase 4 DoD:** milestones 2–4 — proc-macro + richer select + full `@crates` build, analysis
parity green on the pure fetch path.

---

## Phase 5 — q4 query over `@crates` + dogfood binaries (milestone 5, q4) · convergence

**Goal:** query the `@crates` loading graph faithfully and build the razel binary with no Bazel.
Needs both tracks.

- **P5.1 — `cargo_build_script` macro children as loading nodes** · ~250 · dep: P0.5,P3.6 · §11.2/§5.6
  Declare the `:_bs`/`:_bs-`/`:_bs_`/`:build_script_build` child `QueryNode`s at load so `deps()`
  traverses into them (Bazel parity); `labels("deps")` shows only the `:build_script_build` alias.
  Gate: **unit**.

- **P5.0 — Vendor the `@rules_rust//rust/platform` + `@@platforms` query slice** · ~200 · dep: — ·
  §13/(R6)
  Add the small platform slice the `@crates` graph references as **vendored host-repo content** via
  the existing mechanism ([host.rs](../crates/razel-loading/src/host.rs): "a host file is a row +
  an `include_str!`" — `host-repos/`). This makes q4 **dogfood-clean**: P5.2 loads real targets
  with real `rule_class`es from vendored BUILD content, with **no dependency on Bazel's `external/`
  tree**. Gate: **unit** — the slice loads; `@rules_rust//rust/platform:*` and `@@platforms//…`
  resolve as `config_setting`/`constraint_value` targets.

- **P5.2 — Traversal reachability into `@rules_rust`/`@@platforms`** · ~250 · dep: P1.3,P2.6,P5.0 ·
  §13
  Pattern **entry** stays `@crates`-only, but `deps`/`rdeps` **traversal** may leave it. **The node
  source (P2#4, now decided — R6):** when traversal follows an edge into
  `@rules_rust//…`/`@@platforms//…`, query loads the real target from the **vendored host-repo
  slice (P5.0)** and captures it as a §11 `QueryNode` — so its `rule_class`
  (`config_setting`/`constraint_value`) is the loaded one. This is **distinct from P3.3's
  analysis-time condition synthesis** (which feeds `select()` resolution, not query nodes); query
  needs the *node*, not the resolved condition. An edge into a repo that is neither workspace,
  materialized-`@crates`, nor a vendored slice is a **loud error**. (Bazel's `external/` tree is a
  non-dogfood dev fallback only, never a q4-golden input.) Query may **materialize** (same lock-only
  `RepoFetch`, P2.6) but never analyzes. Gate: **unit** — `deps(@crates//:blake3)` reaches the
  platform `config_setting`s **and** `label_kind` prints `config_setting rule
  @rules_rust//rust/platform:…` / `constraint_value rule @@platforms//…` (kind correctness, not
  just reachability).

- **P5.3 — q4 `@crates` query goldens** · ~300 · dep: P5.1,P5.2,P1.7 · §13/§9 (q4)
  Goldens for: `deps()` **select-condition edges** + arm values + default; `compile_data` `glob()`
  `SourceFile` edges; alias transparency (both `@crates//:x` and the canonical label appear);
  **per-attr `labels()`** (`labels(deps,…)` incl. the `:build_script_build` alias;
  `labels(target_compatible_with,…)` = `@@platforms//:incompatible` only). Both apparent + canonical
  literals accepted. Gate: **qg** green over `@crates`.

- **P5.4 — Dogfood the binaries** · ~iterative · dep: P4.6 · §9.5
  `razel build //crates/razel-cli:razel` (and grazel) with **no Bazel involved**. Gate: the binary
  builds and runs; **WS**.

**Phase 5 DoD (milestone 5 + q4):** query over `@crates` matches `bazel query` (named deviations);
the razel binary self-hosts. The whole §14 non-conflict contract is exercised — one loader, two
readers.

---

## Phase 6 — Named-deferred backlog (not decomposed — listed with reason)

Per the design's *deferred, named* discipline (§12/§13/§10). Each becomes its own plan when pulled
forward:

- **Query (q5+):** `tests()` (test_suite/manual/finer kinds); `--output=build`/`package`/`graph`/
  `proto`/`xml` (each its own renderer + ordering); `--cbor` (would mint an unschematized wire
  contract — §13); `cquery` (the analyzed/configured graph); `buildfiles`/`loadfiles`/`siblings`/
  `visible`/`rbuildfiles`; full implicit/toolchain-label fidelity (its own rung with a deviation
  list); daemon-backed query over the V2 snapshot (≠ the §12 expression verb — §13 naming).
- **Build:** `rerun-if` narrowing (the dynamic-dependency model — a post-correctness optimization,
  §5.2/§10); **cross-compilation** (the exec/target split + transitions — §5.3/§10); the
  **build-script long tail** (sys-crates needing system libs, scripts that subprocess/probe outside
  the env allowlist, link ordering — §10); rich `DEP_*` beyond P4.5.
- **Lock:** the `cargo-bazel splice+generate` **offline** fallback (§2.4) — only if the lock format
  proves unstable; never at build time.

---

## Risk register (design §10 → the step that retires it)

| Risk (§10) | Retired by |
|------------|------------|
| Loading-phase layer is net-new, shared with Part B | **Phase 0** (P0.1–P0.5; capture/resolve split per P1#2), validated by **Phase 1** (q1–q3) |
| **O(n²) regression** in per-target capture / per-edge query (workspace load slows) | **P0.0** baseline + **`PL`** gate (scaling assertion + budget) + per-step complexity bounds; **stop-and-review-and-correct** on trip (§Performance) |
| Tree-output executor support | **P2.4** (prereq for P2.6, P3.8) |
| The rustc wrapper (new infra) | **P3.9** |
| Lock-format coupling (versioned, Bazel-internal JSON) | **P2.1** (version-aware) + **P3.11**/**P4.6** parity; offline fallback Phase 6 |
| `expand_pattern` load-only fork | **P1.2** |
| Build-script long tail | surfaced rung by rung (Phase 4); the hard cases deferred (Phase 6) |
| Cross-compilation / `rerun-if` narrowing | explicitly **Phase 6** |

## Rough size

~44 steps across Phases 0–5, the bulk at 250–450 LOC each. Critical path to `razel build
@crates//:blake3` = Phase 0 → Phase 2 → Phase 3 (~25 steps). Track Q (Phases 1, 5-query) can run in
parallel after Phase 0. Phase 6 is backlog, unsized. The **`PL` perf gate** (P0.0) runs from P0.4
onward and is a DoD item for Phases 0–1.
