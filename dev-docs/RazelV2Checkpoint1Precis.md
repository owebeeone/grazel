# RazelV2 Checkpoint 1 — Précis

*2026-06-10, at `razelV2-RSB/D4.4` (133 green tags). Companion to `RazelStarlarkBoundaryPlan.md`
(the phase plan) and `RazelHookSeam.md` (the C3 seam). This doc answers: how far is razel from
building a complex real codebase (TensorFlow as the yardstick), and how do we drive there.*

## 1. Where we are (grounded inventory)

**Code:** ~19.8k LOC Rust across 17 crates (+1.6k test LOC), 53 green test binaries, three
enforced gates (AD2 no-ambient-state, razel-dds dependency boundary, C3c no-language-in-core).

**Proven (phases A–C, all green-tagged):**
- A generic analysis engine where **languages are data**: one provider value algebra (razel-dds
  `FieldValue`/`FieldKind`), one registry-driven transitive fold shared by both dep-resolution
  paths, one generic provider constructor (`razel_build.info`), a toolchain registration seam.
  Adding a language is a registration, and a gate enforces it.
- Five rulesets ride that engine (cc, java, py, rust, sh — razel's own `.bzl` + native rules),
  with **build-and-run** parity on toy corpora (hello-world-scale packages; the parity corpus is
  3 BUILD files). Bazel-parity goldens exist for the AdoptBazel cc path and the java spike.
- Per-analysis `Session` (multi-instance capable), workspace mode with cross-package deps,
  `.bzl` loading with freeze/load.

**Phase D so far:** the attrs schema is real (defaults/`mandatory`/schema-driven label
resolution — D1a–c); **real upstream Starlark loads from `third-party/`** (D4.1–4.4): real-file
loads override razel's shims, real `bazel_skylib` `paths.bzl` + `common_settings.bzl` load and
evaluate, `provider()` exists, and real `rules_rust/rust/private/rust.bzl` (1781 lines) now
**compiles end-to-end** — every free variable resolves — blocked only on `@rules_cc` not being
vendored (`cc_info.bzl` load, rust.bzl:19).

**The honest capability statement:** razel today is a *validated architecture spike*. It
analyzes and executes toy packages over its own rulesets, and has just crossed the threshold of
*loading* (not yet instantiating) real upstream rules. Nothing real has been *built* through a
real upstream ruleset yet.

## 2. The distance to TensorFlow (grounded)

TF's Bazel surface (`third-party/tensorflow`): **1,324 BUILD files, 359 `.bzl` files (53k LOC of
Starlark), 51 external archives** (`tf_http_archive`), WORKSPACE+MODULE hybrid, **53
`repository_rule`s** (the cuda/python/etc. configure system — arbitrary discovery logic run at
fetch time), 220 `select()`s, 129 genrules, 10 aspects, 3 transitions, plus Bazel's *native*
cc/py/proto rules and the full C++ feature-configured toolchain underneath all of it.

Gap by subsystem (✅ have · 🟡 partial/stub · ❌ not started):

| Subsystem | State | Notes |
|---|---|---|
| Loading: `load()`/freeze, real external repos | ✅/🟡 | Real files load; 42-file rules_rust graph not yet walked; no bzlmod/WORKSPACE eval |
| `rule()`/attrs schema | 🟡 | defaults/mandatory/label-resolution real; types, `cfg=`, `providers=`, implicit attrs absorbed-not-honored |
| `ctx` surface | 🟡 | razel has 6 members; rules_rust's 3 core files alone use **16** (`bin_dir`, `runfiles`, `expand_location`, `toolchains`, `configuration`, …) |
| Providers: define/construct | ✅ | D4.2; capture-from-return + `dep[P]` indexing ❌ |
| Configuration: `select()`/`config_setting`/platforms | ❌ | `select()` takes first branch; no config resolution at all |
| Transitions / aspects / exec groups | ❌ | stubs absorb, nothing applied |
| Toolchain resolution (`rule(toolchains=)` → `ctx.toolchains`) | ❌ | one hardcoded cc config behind the registry seam |
| Repository rules / fetch / configure | ❌ | vendored-only; TF's 53 repo rules are a whole subsystem |
| Native rule fidelity (cc/py/proto at Bazel semantics) | 🟡 | toy cc/py; no proto, no genrule, no runfiles/data |
| Execution at scale (10⁵–10⁶ actions, remote cache) | ❌ | toy executor proven on hello-worlds |

**Order-of-magnitude calibration:** the remaining semantic surface is roughly **10× the code
that exists today** — and that is the *expected* shape, not a surprise: the assessed risk was
always integration surface, never algorithmic novelty. TF is the far end of a ladder, not the
next step. Loading-phase semantics are maybe 5–10% done; analysis-phase fidelity less;
configuration/toolchain/repo-rule subsystems are at zero. No calendar estimate is honest at this
distance; the ladder below makes progress *measurable* instead.

## 3. The corpus ladder (how we proceed)

Each rung is a **build-and-run golden** (load-only green is a trap), and names its prerequisite
subsystems. Don't skip rungs; every rung retires a shim or stub.

- **L2 — real `rust_library` hello-world** *(current frontier)*. Needs: `rules_cc` vendored
  (resource), the 42-file rules_rust load graph walked, instantiation semantics (provider
  capture-from-return, `dep[P]`, the missing `ctx` members rust.bzl's impl actually touches, a
  rust `ctx.toolchains` stand-in). Exit: A0's rust corpus green via REAL rules_rust; **both**
  razel rust backends deleted (the D4 exit).
- **L3 — real toolchain resolution.** `rule(toolchains=…)` + registered toolchains →
  `ctx.toolchains[type]`; the generic `Toolchain` type the C3 doc deferred becomes honest here.
- **L4 — a real cc codebase (abseil-cpp).** Needs: `select()`/`config_setting` resolution against
  a real configuration, platforms, native-cc fidelity, runfiles. This rung is where the biggest
  not-started subsystem (configuration) lands.
- **L5 — protobuf.** TF's most load-bearing dep: proto rules, genrule fidelity, generated-code
  dep chains.
- **L6 — a TF subtarget** (e.g. `//tensorflow/core/platform` or tsl). Needs: repository-rule
  subsystem (or vendored pre-configured repos as a cheat rung L6a), the `tf_*` macro layer,
  aspects/transitions as actually exercised.
- **L7 — TF at large.** Adds execution scale (action-graph size, caching, scheduling) on top of
  full analysis fidelity.

## 4. Near-term (the next rolls, in order)

1. **Vendor `rules_cc`** (resource gate — Gianni). Unblocks rust.bzl:19.
2. Walk the rust.bzl load graph (probe loop: each missing builtin/member, test-first, tagged).
3. **`xtask probe`** — automate the loop I've run by hand: run ladder corpora, emit
   first-failure per corpus, classified (missing-global / missing-member / semantic / resource).
   This is the ticket generator for §5.
4. Instantiate `rust_library`: provider capture-from-return, `dep[P]`, `ctx` member expansion.
5. L2 exit: rust corpus green on real rules_rust; delete both rust shims; update plan docs.

## 5. Multi-agent plan (drive-to-completion machinery)

The D4 probe loop is mechanical and parallelizable: *probe → first error → classify → failing
test → smallest fix → workspace green + gates → commit + tag*. Automate it with one supervisor
and N cheap implementers.

**Roles**
- **Supervisor (1, frontier model).** Owns the ladder + a `State.md`; runs `xtask probe`;
  triages failures into *tickets* (one gap each, with repro, files, and a stub-vs-semantic
  ruling); reviews every implementer diff against AGENTS.md/TDD + the gates; serializes
  commits+tags (roll-build preserved); maintains the **stub debt register**; escalates to
  Gianni. Never implements.
- **Implementers (2–4, cheaper model, isolated worktrees).** One ticket each: write the failing
  test first, smallest fix, full workspace + gates green, hand back a diff. Narrow context:
  the ticket, the contract docs (`RazelHookSeam.md`, this précis), and the 2–3 files involved —
  not the whole history. Parallel only across independent gaps (distinct builtins/members);
  anything touching the engine core or a registry is serialized.
- **Verifier (optional, cheapest model).** Re-runs `cargo test --workspace` + `xtask gates` on
  each handoff before supervisor review; bounces fast on red.

**Policy (the guardrails that make cheap agents safe)**
- Tests + the three gates are the objective function. **No agent edits a test, gate, or
  allowlist without supervisor sign-off; the supervisor doesn't weaken a gate without Gianni.**
- Stub-vs-semantic is a *supervisor* ruling, recorded per ticket: stubs are allowed to keep the
  loop moving but every stub lands in the debt register with the rung that must retire it
  (absorbed kwargs, first-branch `select`, namespace stubs are already debt).
- Every green step commits + tags (`razelV2-RSB/...`) — the existing roll-build discipline is
  the recovery mechanism when an agent goes sideways.
- Escalate to Gianni: design seams (new registries, value-algebra changes), gate changes,
  vendoring/resources, any rung-exit declaration.
- **Do not automate:** the C3-style seam designs, doc-of-record updates, ladder re-ordering.

**Cadence.** Rounds of ~5–10 tickets; each round ends with the supervisor re-running the full
ladder probe and appending a delta to this précis (which rung moved, debt added/retired). That
delta — not ticket count — is the progress metric.

## 6. Risks

- **Stub debt becomes load-bearing.** `rule()` absorbs `cfg`/`toolchains`/`providers` silently;
  a corpus can go green-at-load while semantically wrong. Mitigation: debt register + rungs are
  build-and-run goldens + (cheap win) an instantiation-time counter of stub hits surfaced in
  probe output.
- **Shim/real divergence.** Until a rung deletes a shim, both live; real-file-first only applies
  when `external_base` is set. Mitigation: each rung's exit *deletes* the corresponding shim.
- **Local maximum on loading.** Walking load graphs is satisfying and endless; instantiation
  (L2 exit) is the only thing that proves anything. Keep probes pointed at build-and-run.
- **Repo-rule subsystem (L6) is qualitatively different** (running configure logic, hermeticity)
  — treat L6a (pre-configured vendored repos) as the honest first cut.
- **Agent-cost blowout.** Implementers must run on narrow context; the supervisor batches; the
  probe harness, not an agent, does the searching.

## 7. Architecture note: is the current shape optimal? (No — two debts block soon)

The spike methodology — cheapest structure that proves the seam — was right, and the seams it
produced (per-`Session` state, the value algebra, the registries, the gates) are keepers. But
the current shape carries five known debts, two of which become **blocking** at near rungs.
Verified in code, ranked by when they bite:

1. **Eager same-scope analysis (no loading/analysis phase split).** Rule impls run *at target
   declaration*; a dep on a later-declared target is an error (`dialect.rs` "not analyzed yet —
   forward references not yet supported"). Bazel loads a whole package before analyzing, and
   real-world BUILD files (rules_rust's own tests, abseil, TF) freely forward-reference — this
   breaks on first contact with real corpora, likely **during L2**. Fix is a re-architecture,
   not a ticket: BUILD eval only *records* declarations; analysis becomes a demand-driven pass
   over the recorded graph (which also unlocks parallel analysis). The registries/folds survive
   unchanged; `invoke`/`record_target` do not.
2. **The DDS is derived, not the store.** `Session.results` (a `BTreeMap` of `AnalyzedTarget`,
   self-described in `state.rs` as "the embryonic DDS fact store") is the real store; a fresh
   `Dds` is rebuilt — cloning **every analyzed target** — on *every dep resolution*
   (`deps.rs:69`, `dialect.rs:151`). Analysis is O(n²) with heavy constants: invisible on
   3-package corpora, noticeable at abseil scale (**L4**), impossible at TF. Fix: the `Session`
   owns one `Dds`, facts asserted at `record_target` time, folds read it incrementally — this
   also collapses the `AnalyzedTarget.providers`/DDS dual representation.
3. **The language boundary is a convention, not a compilation unit.** Engine core + dialect +
   all five language modules live in one 3.4k-LOC crate (`razel-loading`), with the C3c gate
   enforcing by allowlist what a crate boundary would enforce by compiler. Split
   (`razel-loading-core` + `razel-rules-*`) after L2 deletes the rust shims — mechanical (the
   C0 method), and it turns the gate from a linter into a type error.
4. **Stringly-typed labels and files at the seam.** `razel-core::Label` exists but
   `razel-loading` passes `String`s everywhere (`canon_label` munging, files as path strings,
   no artifact identity). Perf + label-form correctness cliff at L4 scale; mechanical to fix,
   cheaper the earlier it's done.
5. **Two instantiation paths.** Native rules and the `rule()` path still duplicate the
   gather/record shape (the fold is unified; the rest isn't). Already scheduled: each ladder
   rung exit deletes the corresponding native/shim path.

**Scheduling implication (amends §4/§5):** items 1–2 are *foundations*, not tickets — grinding
agent tickets onto the eager-analysis core means building on a structure that must be replaced
mid-ladder. The supervisor's first epic, before mass ticket rounds, should be the phase split
(1) with the incremental DDS (2) riding the same rework, both proven by the existing parity
suite staying green. Items 3–5 are rung-exit chores.

---

## Round delta — razelV3 round 1 (2026-06-10)

**Landed** (all green-gated): `razelV3/E0b..E0-exit` — the §7 foundations paid: loading/analysis
phase split (forward refs resolve, Starlark + native, cycle-guarded) and the Session-owned live
DDS (per-dep rebuild deleted; O(n²)→O(n)). `razelV3/L2a` — provider capture-from-return +
`dep[P]` indexing (Bazel's provider model; what `dep[CrateInfo]` rides). `razelV3/L2b` —
`xtask probe` (the classified ticket generator; green rungs are regression sentinels).
55 test bins; three gates green.

**Debt added:** see `RazelGaps.md` "E0/L2 debt register" (cross-package custom providers,
String labels deviation, the 16-member `ctx` backlog, `ctx.actions.write`).

**Rung state:** L2 frontier blocked on ONE resource — `rules_cc` not vendored
(`@rules_cc//cc/common:cc_info.bzl`, rust.bzl:19; the probe classifies it `missing-export`
because the synthetic shim catches the prefix). Escalated to Gianni. Next after unblock:
walk rust.bzl's remaining load graph → instantiation (`ctx` ticket batch above).

## Round delta — razelV3 round 2 (2026-06-10)

**Landed** (green-gated): `razelV3/L4a-select` — REAL `select()`/`config_setting` resolution
(structured config: compilation_mode + define; most-specialized wins; loud errors), retiring the
first-branch stub — the worst silent-wrong behavior for the Bazel-compat goal. `razelV3/genrule`
— Bazel's most ubiquitous native rule ($@/$</$(SRCS)/$(OUTS)/$(location[s]), $$; unmodeled vars
error). `razelV3/args-fidelity` — add(name,value) + add_all before_each/format_each/map_each
(rustc.bzl's 56 call sites; were silently dropped). `razelV3/write-executable`. 58 test bins;
gates + probe sentinels green.

**Debt added:** deferred select resolution; config_setting breadth (cpu/platforms = L4);
genrule breadth (RULEDIR/@D/tools=); File-typed depset elements for map_each.

**Rung state:** unchanged — L2 frontier blocked solely on `rules_cc` vendoring. The unblocked
compatibility lane (configuration + genrule + Args) is exhausted; remaining grounded work is
behind the resource gate or L4-scale design (runfiles, platforms).

## Round delta — razelV3 round 3 (2026-06-10)

**MILESTONE — `rules-rust-load: OK`:** real, unmodified `rules_rust/rust.bzl` loads end-to-end
through real rules_cc + skylib + bazel_tools. Landed: repo-relative loads inside `@repo`
(BzlLoader load_ctx); `provider(init=)` 2-tuple + verbatim arg routing + freezable instances;
host-materialized `@cc_compatibility_proxy` + `@bazel_tools` (razel-as-host bindings, the
Bazel<9 dispatch shape — full rules_cc-Starlark CcInfo internals deferred to L4); Bazel
file-label semantics (srcs entries resolve to source files). `rules-rust-library` now EXECUTES
the real `_rust_library_impl` — frontier is the ctx-member batch (the predicted 16).

**Agent-cycle pilot: VALIDATED.** T-001 (File-typed depsets) ran the full
builder(worktree)→reviewer(fresh)→serial-integration loop: builder delivered test-first +
green, reviewer ACCEPTed with a real residual-risk finding (to_list round-trip — watch),
integration cherry-picked clean. The ctx-member batch is the fleet's first real workload.

**Note (Gianni):** the host materializations live OUTSIDE razel's repo:
`third-party/cc_compatibility_proxy/`, `third-party/bazel_tools/` (+ the rules_cc clone) —
razel-authored content that should be committed in glial-dev.

## Round delta — razelV3 round 5, autonomous session (2026-06-10)

**MILESTONE — `tf-load-leaf: OK`: a real TensorFlow package (`//tensorflow/core/lib/jxl/testdata`)
loads, declares, and analyzes end-to-end** through TF's full macro layer (tensorflow.default.bzl →
tensorflow.bzl → XLA tsl → real rules_cc/skylib/protobuf machinery). ~25 frontiers cleared this
session, all green-gated (`tf-load-walk-1/2/3`, `tf-leaf-loads`, plus select/Label/Depset reworks).

**Substantive engine pieces landed:**
- **Deferred select (Bazel's select-as-value):** select never resolves at load; SelectBranches +
  SelectExpr (list+select concat, add/radd), Label-struct keys, resolution at attr consumption;
  failed condition-package loads SURFACE. Hybrid eager path only for already-declared specs.
- **Cross-repo label semantics:** repo-aware `Label` value (canonical str(), hashable);
  canon_label/pkg_of/load_package handle `@repo//pkg`; EXTERNAL PACKAGE LOADING from the vendored
  base; module load-context for BOTH external and workspace .bzl (relative loads resolve against
  the module's own package/repo).
- **Freeze-generic values:** ProviderInstance, SelectExpr/Branches, Depset — real .bzl construct
  all of these at module level.
- **Bazel native rules as BUILD globals** (cc_library/cc_binary/cc_test) + native.filegroup/alias.
- **Host materializations from generators' own templates:** local_config_{cuda,rocm,sycl,tensorrt},
  python_version_repo, tf_wheel_version_suffix, local_config_remote_execution,
  proto_bazel_features, bazel_tools toolchain_utils — all compiled into the engine (host.rs);
  host-false select conditions for generated GPU repos.
- **Vendored** (repo manifest): xla (198M), com_google_protobuf, com_github_grpc_grpc,
  rules_ml_toolchain, rules_proto.

**Frontiers when paused:** tf-load-cc → skylib `config_setting_group` semantics
(`//tensorflow:is_cuda_nvcc` — AND/OR condition groups need spec support);
rules-rust-library → `ctx.toolchains` (the L2 boss, untouched this session).
59 test bins + 3 gates + 4 probe sentinels green; tf-load-leaf is now a sentinel candidate.

## Round delta — razelV3 round 6 (2026-06-10, autonomous cont.)

The cc rung's dep cascade now walks EIGHT packages deep into TF's real core graph
(risc → core:framework → example → framework → platform → @tsl → @com_google_absl), with REAL
abseil cc_library targets ANALYZING through REAL rules_cc Starlark. Landed: host-native
config_setting_group + alias-following condition resolution; call-site Label/select-key binding;
cpu modeling; package shorthand; surfaced (unswallowed) demand-load failures; label-default
resolution (implicit attrs); native.* namespace wholesale; @xla/@tsl pinned to TF's own vendored
copies. Vendored: abseil, grpc, rules_proto.

**The next boss is the L2a debt, hit exactly as registered:** cross-package custom-provider
instances (rules_cc's FunctionInfo delegate). Design: BUILD modules become freezable (DeclStore →
freeze-generic), packages freeze after their drive completes, captured provider instances harvest
into the Session as OwnedFrozenValues (heap-independent), DepTarget falls back to the harvest for
cross-package dep[P]. This also closes the same-package-only limit for rules_rust's layering (L2).

## Round delta — razelV3 round 7 (2026-06-10, layers session)

**Layer 0 LANDED — cross-package provider flow (the L2a debt closed):** packages freeze after
their drive; captured instances harvest into the Session as OwnedFrozenValues; dep[P] falls back
to the harvest; .bzl module cache is Session-wide (identity holds; TF's macro layer evaluates
once). **Demand analysis landed:** dependency-loaded packages drive native decls only — undriven
Starlark decls harvest as data and analyze ON DEMAND in the consumer's eval (doc-only targets
never analyze); ensure_analyzed owns load→pending→deferred with store-scoped pending.
**Layer 1 LANDED:** ctx.toolchains (ToolchainMap: at()/is_in; host rows in toolchains.rs — rust,
cc, proto types; fields grow probe-driven). Plus: lexical binding for attr-default label strings
((repo,pkg) module-context stack); ctx.build_setting_value; label_flag/label_setting/toolchain/
toolchain_type rules; ctx.fragments absorbs. **dialect.rs split** (2059→634 + ctxv/decls/selects/
provider_values/labels/genrule_cmd — the C0 discipline, glob re-exports keep all paths).

**Frontiers:** tf-load-cc — protobuf root-package toolchain machinery (post-proto-row);
rules-rust-library — Layer 2 ctx members inside the real impl. Both walkable; Layer 3
(process_wrapper run + param-files) unchanged ahead. 60 bins; 3 gates; 4 probe sentinels.

## Round delta — razelV3 round 8 (2026-06-10)

**The rust lane is INSIDE `rustc_compile_action`** (rustc.bzl:1536 → collect_inputs) — the real
compile-action assembly, running over tinyjson (a real crates.io package built by
rules_rust's own bootstrap path). Cleared this round: @platforms + host-matched constraint
conditions; tinyjson repo materialization; real rust-toolchain host scalars; real ctx.label/
ctx.file/ctx.files-defaulting; symlink/run_shell actions; external-repo glob(); operator
absorption. Frontier: cc_common/cc_toolchain interactions inside collect_inputs (should_use_pic)
— the Layer-3/cc_common bridge boundary, exactly where the build-path plan begins.
TF cc lane: protobuf root-package toolchain machinery (unchanged this round).
