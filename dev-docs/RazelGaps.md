# RazelGaps — unplanned work (backlog)

A running collection of things razel will need that are **not on a phase plan** (no roll-build slot
yet), surfaced during development. Promote an item into a plan (e.g. `RazelStarlarkBoundaryPlan.md`
§10) when it's scheduled. Keep entries actionable: what, why, and the known specifics.

## bazelrc / razelrc processing

razel must eat both `.bazelrc` (Bazel-compatible) and `.razelrc` (razel extensions). Today only the
`--bazelrc` *flag name* is recognized (`razel-cli/src/bazel_flags.rs`); there is **no rc-file parsing**.

- **Format:** `<command> <args>` lines (`build`, `test`, `common`, `always`); `import` /
  `try-import <file>`; `#` comments; line continuations; `--config=X` → expand the `<command>:X` lines.
- **Locations / precedence:** system → workspace `.bazelrc` → home `~/.bazelrc` → `--bazelrc=`
  overrides, lines accumulating in order.
- **`.razelrc` layering:** read `.bazelrc` first (compat), then `.razelrc` last (overrides + carries
  razel-only flags Bazel would reject) — so a project keeps its working `.bazelrc` and adds razel-isms
  separately.
- **Unknown-flag tolerance:** the `bazel_flags` table already exists so razel *recognizes* Bazel flags
  it doesn't act on (no hard error) — that's what makes eating a real `.bazelrc` safe.
- **Toolchain hook:** rc settings select the toolchain (native vs adopt-Bazel) **per language**
  (cc, py, rust) — see the toolchain item below + `RazelStarlarkBoundaryPlan.md` §7.

## Toolchain-change cache invalidation

Build-correctness requirement: when the build tool changes (e.g. `rustc`/`clang` is updated), the
actions that used it MUST re-run. The tool is an **input**; its **content digest** belongs in the
action's cache key.

- **Key on content, not timestamp.** `(size, mtime)` is a fast "did it change?" stat-proxy (to skip
  re-hashing a 300 MB `rustc` every build); the *key* is the content digest. Timestamp-as-key is
  unsafe both ways — misses mtime-preserving updates (→ stale/wrong output) and fires on `touch`.
- **Per-action, precise:** a new tool digest → cache miss only for actions that had that tool as an
  input — not a global flush.
- **Native toolchains especially:** they're *non-hermetic* — the host `rustc`/`clang` can change
  underneath you with no signal, so capturing the resolved tool's digest per build is what keeps
  native builds correct. Hermetic (downloaded) toolchains are pinned → safe by construction (an update
  is a new pin = a new key anyway).
- **razel gap:** the digest infra exists (`razel_ir::FileNode.digest`, `content_key`), but the action
  key today is description/path-based (`mnemonic | input-paths | output-paths`) — it does **not** fold
  in input *contents*, incl. the tool. Needs: (1) the resolved toolchain tool tracked as an input,
  (2) its digest folded into the action key (mtime/size as the stat fast-path). Belongs with toolchain
  resolution.
- This is *why* Bazel lists toolchain files (`cc_wrapper.sh`, the `rustc` binaries) as action inputs —
  the cache-invalidation mechanism, not just sandbox detail (cf. `RazelStarlarkBoundaryPlan.md` §8).

## C++20 modules build workflow (P1689 scan-as-production)

Support C++20 named modules (and C++23 `import std;`). Note the version: **modules are C++20**, ~5
years old; C++23 only added `import std;`. What's new is the **tooling** maturing 2023–25 (P1689R5;
`clang-scan-deps -format=p1689`; MSVC `/scanDependencies`; CMake 3.28 named modules non-experimental,
3.30 `import std;`) — which makes a native module build now table-stakes for modern C++.

- **This is NOT the "wrap CMake" item** (cf. `RazelMultiPackageWorkspaceProposal.md` §2.2 — CMake-the-
  config-language stays wrap-first). Modules are a *build-ordering capability* independent of how the
  project is described: any C++ source set with `export module`/`import` needs it.
- **The seam fits the DDS exactly.** Modules force a two-phase build: `scan → P1689 JSON → topo-sort →
  compile interfaces to BMIs → codegen`. P1689 is per-TU `{provides:[mod], requires:[mod]}` — a
  compiler-authored, deterministic, provenance-carrying **EDB-fact source**, i.e. a Gc3 scan production
  emitting `DepEdge` facts. The module-name→producing-TU map is the `RazelMultiPackageWorkspaceProposal`
  §2.4 name→target index, generalized to *intra*-target ordering.
- **razel is structurally better-placed than Bazel here.** The compile-order DAG is discovered *after*
  the scan (Ninja solved it with `dyndep`). Bazel fights this — *"Bazel does not support dynamic
  outputs, so actions cannot declare BMI files as outputs when only determined during execution"* — so
  rules_cc uses a 3-action workaround (`c++-module-deps-scanning` → `c++20-module-compile` →
  `c++20-module-codegen`), two-phase Clang-only, still experimental. razel's DDS is demand-driven with
  on-the-fly rule add/remove (`DdsQuerySystem.md` §4) — **dynamic edge discovery is the native mode**,
  not a bolt-on. This is a genuine differentiator, worth not squandering.
- **Gotcha — BMI non-portability is the correctness landmine.** `.pcm` (Clang) / `.gcm` (GCC) / `.ifc`
  (MSVC) are *(compiler × version × flags)*-specific. A BMI is a `(TU × toolchain × config)` product,
  never a shareable artifact. This folds into **both** per-config keying (`GrazelForecast.md` D5/D6/D12,
  `Label → (Label, Config)`) **and** the toolchain-digest action key (the toolchain item above). Wrong
  key ⇒ silent miscompile.
- **Gotcha — uneven matrix / scan cost.** Two-phase (BMI separate from codegen) is Clang-only; GCC/MSVC
  differ → the toolchain matcher must branch. Full-preprocess scan is expensive; the fast string-scan
  path trades edge-case correctness (macro-conditional imports).
- **razel gap:** no cc module support; the rulepack cc rules are header-model. Needs (1) a
  deps-scanning action mnemonic emitting P1689 → facts, (2) a BMI action class keyed by
  `(TU, toolchain, config)`, (3) post-scan dynamic-edge insertion into the DDS. Calibration: the
  algorithm (scan → topo-sort → ordered compile) is **commodity** (Ninja/CMake/build2 all do it); the
  cost is integration surface — BMI keying + the toolchain matrix — not novelty.

## Extensible cross-cutting goals (`fmt` / `lint` / …) — Pants's union-dispatch model

razel inherits Bazel's verb set (`build`/`test`/`run`/`query`/…) but Bazel has **no first-class
`fmt`/`lint` goals** — they're bolted on via aspects + external rulesets (`rules_lint`). Pants's
strongest extension is making these **first-class, language-pluggable goals**, and it's increasingly
table-stakes (and core to the griplab/agent-IDE story — the F17 derivation layer already names "lint",
`RazelV2FinalArchProposalPlan.md` §5.1).

- **What to borrow (the Pants model).** A core goal (`fmt`) is a thin driver: it asks the engine for
  all members implementing a request type (`FmtTargetsRequest`), each contributed by a language backend
  as a `UnionRule` (ruff→py, gofmt→go, clang-format→cc), then partitions targets by owner and runs each
  (the partitioner/batch pattern — batch files per tool invocation for speed). **Rules compose by type,
  not by name** (`history/ArchAnalPants.md` §2–3). Adding a language adds a `UnionRule`; adding a goal
  iterates `UnionMembership`. No central per-language switch.
- **The interesting wrinkle (why this isn't free for razel).** razel's cc support — and java when it
  lands — is **hard-coded native rulepack code** (`razel-rulepack`), not open rules. For those languages
  to participate in `fmt`/`lint`, the seam must reach *into* the native rulepacks: each native language
  module has to **advertise a capability** (`(language, source-role) → format/lint tool-action`) against
  the *same* registry a plugin would. This is exactly the Bazel-vs-Pants tension `ArchAnalPants.md`
  drew: Bazel hard-codes the native core (cross-cutting extension is painful → aspects); Pants makes
  everything a registered rule (a new goal just reads union membership). razel sits between them, so it
  must **decide where the dispatch boundary lives** — a uniform capability registry both native rulepacks
  and plugins populate, or an adapter at the rulepack edge. Don't let cc/java be special-cased into the
  goal; that's how the seam rots.
- **The closest existing primitive** is the DDS `aspect` production (per-node-in-closure traversal,
  `DdsQuerySystem.md` §49/§166) — a `fmt`/`lint` goal is plausibly "an aspect that, per target, resolves
  a registered tool-action by `(language, source-role)` and mints a format/lint action." Missing: (a) a
  **capability registry** keyed by `(language/source-role → tool-action provider)`, populated by native
  rulepacks *and* plugins; (b) a **verb/goal extension point** in the CLI/engine beyond the fixed
  Bazel-compat set.
- **Gotcha — fmt/lint are not artifact-producing build actions.** `fmt` **mutates the source tree**
  (write-back — a side-effecting action, not a hermetic output); `lint` emits **diagnostics**, not files
  (a derivation/F17 output, not an action-graph artifact). Neither fits the pure build-artifact action
  model. Needs either a side-effecting-action class (fmt) or to live in the F17 derivation layer (lint,
  per §5.1) — a real fork to settle before building the goal.
- **razel gap:** no goal-extension seam and no capability registry today. Needed before *or alongside*
  the cc/java rulepacks harden, so they're authored against the registry from the start rather than
  retrofitted. Promote with the F17 derivation work.

## Parallel-spine reconciliation — loader vs razel-dds/rulepack (Phase C; review F3)

The live BUILD-analysis path (`razel-loading`) and the typed DDS spine (`razel-dds` + `razel-rulepack`)
are **two implementations of the same propagation**, and the loader does NOT use the spine:

- `razel-loading` does **not** depend on `razel-dds`. It hand-rolls the transitive fold (the one
  `fold_field`, R1b) over `BTreeMap<String, AnalyzedTarget>`; `DdsRead::fold_depset` (the headline B2
  primitive) has **zero live callers** outside its own tests. `fold_set` is live only via the
  rdep/impact query (`razel-analysis`), not the cc/java rule path.
- `razel-rulepack`'s schema-driven provider engine (`RuleDecl`/`Provides`/`FieldSource`) **already
  exists** and is also unwired (`razel-loading` imports it only for `constrain::{VarValue,Vars}`).

**Decision (R1b):** *parked, not wired.* The bounded fix landed — one `fold_field` + direct unit tests,
so the **run code is tested** (F3's testable half). The **wire-through** (route the loader's provider
capture + fold through the DDS schema map + `FieldKind` fold, so tested==run and the per-field fold
duplication collapses — F24/F29) is the **Phase-C provider-map** epic: the same work as generalizing
`CcInfo`/`JavaInfo` off `AnalyzedTarget`'s flat fields, so do it once, there. Until then the loader's
`fold_field` is the single source of truth; the DDS spine is a parallel design to fold in.

## cc-config eval ergonomics (Phase D; review F35/F33)

- **F35 — config error line numbers are offset.** `parse_feature_config` prepends the
  `cc_toolchain_config_lib` shim and parses the concatenation as one module named `cc_config.bzl`, so
  a config error reports at `line + <lib length>` against the wrong filename. Fine for embedded
  fixtures; for **A5b** (real ~2000-line generated configs) load the shim as a separate `FileLoader`
  module so the config keeps its own filename/line numbers (or subtract the prefix). Commented at the
  call site.
- **F33 — A5a content is hand-frozen for one host.** The API *shape* is real (real constructors +
  `create_cc_toolchain_config_info`, round-tripped — F34); the *content* (`cc_macos_core.bzl`) is a
  one-host `cc_configure` transcription. The generated config is explicitly A5b/Phase D. Flagged-
  acceptable; an optional drift-catching characterization test (fixture vs a fresh host `cc_configure`)
  can land with A5b.

## Native cc path parity / executed==declared convergence (Phase C/D; review F18)

razel has two cc backends (§7): **Native** (`#[default]` — PATH-resolved host `cc` + simple flags;
what razel-build EXECUTES) and **Adopt-Bazel** (razel's `cc:defs.bzl` over the engine → Bazel's
faithful DECLARED graph; what the graph-parity runner checks). **Gap:** only Adopt-Bazel is golden-
tested; the executed Native path is razel's runnable lowering and is **not** Bazel-parity (it can't be
without materializing Bazel's toolchain — `cc_wrapper.sh` + the `bazel-out` execroot). So green
characterization is NOT evidence the executed cc output is Bazel-faithful — it pins Native's *own*
output (a regression guard), per the characterization header.

- **Don't fake it:** there is no "Native parity" golden to write — Native ≠ Bazel's declared graph by
  construction. The honest state (now documented in §7 exit + `CcToolchainMode::Native`): Adopt-Bazel
  is parity-proven; Native is executable + characterized-but-not-parity.
- **Convergence (Phase C/D):** make the executed graph BE the declared graph — materialize Bazel's
  toolchain so razel-build runs Adopt-Bazel's `cc_wrapper.sh` + `bazel-out` graph (the §7 "adopt the
  toolchain" end state). Then executed == parity-tested. Until then the gap is real and named.

## E0/L2 debt register (razelV3 round 1 — 2026-06-10)

- **Cross-package custom-provider flow (L2a limit).** `dep[MyInfo]` reads the package-local
  `DeclStore.captured` (heap-resident instances) — a cross-package dep's custom providers are
  invisible (clear "does not provide" error, but for the wrong reason). The typed-algebra channel
  (registry providers) crosses fine. Retire when a real corpus deps custom providers across
  packages (rules_rust internal layering may hit this in L2).
- **Labels still `String` (E0 deviation; précis §7 item 4).** The E0 declaration structures kept
  string labels; `razel-core::Label` exists unused at the seam. Retire by L4 scale at the latest.
- **`ctx` member expansion backlog (L2, post-rules_cc).** rust.bzl's core three files use 16
  distinct `ctx.*` members; razel has 6. The probe will surface them one-by-one once the
  `@rules_cc` load resolves — expect a ticket batch: `bin_dir`, `runfiles`, `expand_location`,
  `toolchains`, `configuration`, `features`/`disabled_features`, `workspace_name`,
  `genfiles_dir`, `file`, `expand_make_variables`.
- **`ctx.actions.write` (go-shaped languages + py-launcher cleanup).** Grounded by go importcfg
  needs (V3 §4) and the py `printf|sh -c` workaround; small generic engine move.
- **Deferred `select()` resolution.** select resolves eagerly at call time — conditions must be
  declared before use (forward/same-package-later config_settings error clearly). Bazel attaches
  selects to attrs and resolves at analysis; razel's E0 split makes the deferred model natural —
  do it when a real corpus declares conditions after selects.
- **`config_setting` model breadth.** Only `compilation_mode` + `define` are modeled (unmodeled
  keys error loudly). `cpu`/platform constraints land with L4 platforms.
- **genrule breadth.** `$(RULEDIR)`/`$(@D)`/`$(BINDIR)` + `tools=`/`exec_tools=` (exec-config)
  + `toolchains=` make-var sources error loudly today; add as real corpora demand.
- **Absorbing host namespaces (TF-loading).** `cc_common`/`apple_common`/`java_common`/
  `proto_common`/`coverage_common`/`testing`/`platform_common` absorb ANY member (load-time);
  absorbed values surface only when used at analysis (`<host-absorbed>` in tracebacks). Each
  member gains real semantics when a rung needs it. Same for exec_group/aspect/subrule stubs.
- **Generated-repo defaults.** `@python_version_repo` is host-materialized from its generator's
  template with no-env defaults (py 3.11, USE_PYWRAP_RULES=False). Snapshot-from-configure is
  the upgrade path if fidelity bites (L6a decision).

## Worker-pool debt register (razelV3 round 23 — 2026-06-11)

- **P4a — per-worker eval context (the pool's real blocker).** Session carries per-EVAL-STACK
  state as Session-wide fields: `current_pkg` (read by `canon_label`/`qualify` — every label
  resolution), `current_bzl_repo` (load-context stack), `AnalysisState.current` (the in-flight
  target), `analyzing` (re-entrancy guard set). Two workers evaluating concurrently clobber each
  other's context; labels qualify into the wrong packages and the sweep collapses into fast-fail
  cascades (measured: 6-7/53 in <1s at threads≥2 vs 10/53 in 54s sequential, sample-16; the same
  select key qualified into different packages run-to-run — direct evidence). Design sketch: an
  `EvalCtx { session: &Session, current_pkg, current_bzl_repo, current_target }` per worker,
  stashed in `eval.extra` (today's `session(eval)` returns it; `Deref<Target=Session>` keeps the
  shared-state call sites untouched); `analyzing` gets the P3 InFlight/wait treatment for
  cross-worker demand analysis. ~27 `current_pkg` + 7 `current_bzl_repo` sites across 6 files,
  plus the `load_package_entry`→`eval_build_src_in` signature chain. Engine-core: supervisor-grade.
- **`begin_pkg_load` 20s takeover timeout masquerades as work.** A cross-thread wait that cycles
  burns exactly 20s then duplicates the load — sweep walls of ~20s/~40s are timeout multiples,
  not eval (the round-22 "40s at 2% CPU" reading). Post-P4a: real cycle detection (waits-for
  check) instead of the timeout, per the P3 plan note.

## Round-24 register (2026-06-11)

- **Failed-package loads re-eval per consumer (perf debt; the purge fix's cost).** A failed
  package eval now purges its partial `results`/`pending`/`fold_cache` (the round-24 poison
  fix), so every consumer dep-loading a failing package (e.g. tensorflow/core behind @curl)
  pays a fresh eval: full sweep 1:00 → 2:19. Follow-up: tag eval failures by phase —
  DECLARE-phase failure = Bazel's "package in error", memoizable as `PkgState::Failed(err)`
  (cached error per consumer, loud, no re-eval); drive/analysis failure = declarations are
  fine, dep re-load stays legitimate (the test contract in cross_package_providers.rs).
- **Package "load" conflates declarations with whole-package analysis.** `drive_all` entry
  loads fail the PACKAGE when any single target's analysis fails; Bazel fails per-target.
  The tfload report therefore undercounts loadable packages. Revisit when the report needs
  per-target resolution (checkpoint-3 yardstick refinement).
- **`cc_libc_top_alias` is a record-named stub** (native_cc.rs): rules_cc cc/BUILD's
  `:current_libc_top`. No grte/libc model; becomes real with L4 platforms if ever needed.
- **Host doc-target BUILDs are minimal.** @cc_compatibility_proxy/@compatibility_proxy/
  @bazel_features/@proto_bazel_features/@local_config_{cuda,tensorrt,rocm,sycl}/@rules_python
  host BUILDs declare only the bzl_library/filegroup doc targets the vendored tree deps
  today; @rules_python's are filegroups (shim repo has no host-served files — skylib's
  bzl_library fail()s on empty). Extend per probe, don't pre-build the doc graph.
- **@bazel_features is all-True modern posture** (host-repos/bazel_features/features.bzl):
  five version-gates rules_cc consults; unlisted members error loudly by design.
- **Template-variable flow (`toolchains=` attr → `ctx.var`) is unmodeled.** TFRT's
  `make_variable` rule returns `platform_common.TemplateVariableInfo({name: value})` and
  cc_library's `defines = ["$(TFRT_MAX_TRACING_LEVEL)"]` expects it in `ctx.var` (rules_cc
  `_lookup_var` fail()s — loud). platform_common absorbs wholesale, so the variables dict is
  swallowed today. Real fix: TemplateVariableInfo as a real provider + merge the toolchains=
  attr targets' variables into ctx.var at ctx build. Surfaced by the tf_runtime vendor
  (@tf_runtime//:tracing); same mechanism as the genrule `toolchains=` make-var debt.
- **Eager select() composes wrongly with tuples (highway, 15 pkgs) — the deferred-select debt's
  concrete corpus.** highway's BUILD: `HWY_TEST_DEPS = [...] + select({...})` then
  `HWY_TEST_DEPS + extra_deps` where `extra_deps` is a TUPLE. Bazel never resolves select at
  load, so the `+` is select-concat (tuple-tolerant); razel's hybrid select resolves eagerly
  (conditions all declared) → plain list → `list + tuple` errors. Fix = the registered
  deferred-select model (select always returns SelectBranches; pick at attr consumption), plus
  tuple-tolerant part flattening in `resolve_attr_value`. Needs the full-sweep parity check —
  eager resolution is load-bearing for macro paths that inspect resolved values.
