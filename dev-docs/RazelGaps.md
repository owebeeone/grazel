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
