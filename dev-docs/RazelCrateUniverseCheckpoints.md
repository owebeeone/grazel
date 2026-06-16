# RazelCrateUniverse — roll-build checkpoints

Roll-build of [`RazelCrateUniversePlan.md`](RazelCrateUniversePlan.md) on branch `razelv3`, tag
prefix `razelv3-rust/`. The method's `Checkpoints.md` analog (razel is independent of the
plan-docs GLP system) — tag, verification, and rollback per integration gate.

| Tag | Marks | Base |
|-----|-------|------|
| `razelv3-rust/p0-start` | clean tree before P0.0 | — |
| `razelv3-rust/p0-foundation` | Phase-0 foundation + rust-rule capture | `p0-start` |
| `razelv3-rust/p0` | **Phase 0 complete** — loading-phase graph; all q1 rules capture | `p0-foundation` |

---

## `razelv3-rust/p0-foundation` — Phase 0 foundation (loading-phase graph)

**Scope.** The complete loading-phase graph *machinery* (P0.0–P0.5) wired into the loader for the
**rust rules**. NOT yet the full Phase-0 DoD — cc + the dialect natives
(`filegroup`/`alias`/`genrule`/`config_setting`) and the mixed-rule q1 corpus are **P0.5b**, the
plan's explicit "dialect/cc paths second" split.

**Commits (`p0-start..p0-foundation`):**
- `9a30677` P0.0 — `perfgate` PL gate (<20s synthetic-corpus benchmark; scaling 1.86–1.90 linear)
- `d5c813a` P0.1 — `RawAttr` model + canonical stringification
- `cdec73c` P0.2 — Value→`RawAttr` snapshot + schema-aware label extraction (R1)
- `9959560` P0.3 — `LoadedTarget`/`QueryNode`/`Edge` + kind strings
- `52e86ea` P0.4 — edge-kind resolution (`classify_kind` R4 precedence + `resolve_edges`)
- `9553530` P0.5 — capture-at-load + `finalize_edges`; rust_library/rust_binary migrated to
  raw-Value capture; `drive_tree` finalizes

**Verification.**
- `cargo test -p razel-loading --lib loaded::` — 18 unit tests green.
- `cargo test -p xtask perfgate` — 3 tests green; `cargo xtask perfgate` OK (~8s, scaling ≤ 2.3).
- `cargo xtask gates` — green (no AD2/F13; S0 intact).
- `cargo test --workspace --no-fail-fast` — green **EXCEPT** the 2 pre-existing cc/java parity
  carve-out reds (`live_cc_graph...`, `java_graph...`; from `dc0e863`, tracked `task_0448b415`).
  **No new regressions** from the capture wiring.

**Rollback.** `git reset --hard razelv3-rust/p0-start`. Low-risk: every step is additive (nothing
in the build path reads `loaded_targets`); the only behavior change is the rust_library/rust_binary
`str_attr_parts` migration, which is behavior-preserving for plain lists.

**Remaining for full Phase 0 (`razelv3-rust/p0`):** P0.5b — capture for cc + dialect natives + the
other 3 rust rules; the mixed-rule q1 corpus; then the strict (non-lenient) `finalize_edges`.

---

## `razelv3-rust/p0` — Phase 0 complete (loading-phase graph)

**Scope.** The Phase-0 DoD: the loading-phase graph is captured via the central seam for **every
q1-corpus rule family** — rust (`rust_library`/`rust_binary`), cc (`cc_library`/`cc_binary`), and
the dialect natives (`filegroup`/`alias`/`config_setting`/`genrule`) — and `finalize_edges`
resolves all edge kinds over the live session indexes. Additive; the build path is unchanged;
PL stays linear with capture on.

**Commits (`p0-foundation..p0`):**
- `8e0386d` P0.5b — capture for cc + dialect natives; generalized `capture_rule`; q1 mixed-rule test

**Verification.**
- In-crate `p05b_mixed_rule_corpus_captures_the_loading_graph`: a hermetic mixed corpus captures
  every family with the right `rule_class`; finalize resolves Rule / SourceFile / Alias /
  GeneratedFile edges.
- `cargo test --workspace --no-fail-fast` — green EXCEPT the 2 carve-out reds (no new regressions).
- `cargo xtask perfgate` — scaling **1.95** (cap 2.3), budget green, 7.5s: capture is **linear**.
- `cargo xtask gates` — green.

**Rollback.** `git reset --hard razelv3-rust/p0-foundation` (capture is additive).

**Deferred (NOT Phase 0):** the 3 rarer rust rules (`rust_shared_library`/`rust_library_group`/
`rust_doc`); scalar attrs (`name`/`edition`) in the attr map; **strict** (loud-error)
`finalize_edges` — it needs external-target resolution, a Phase-1 (P5.2) item; a checked-in q1
corpus fixture lands with the query goldens (P1.7).

---

## `razelv3-rust/p1-engine` — Phase 1 query engine (q1–q3 behavior)

**Scope.** The complete `razel query` engine over the §11 graph (P1.0–P1.7): a new `razel-query`
crate (boundary-gated — no analysis/exec dep), the §12 parser, the load-only pattern resolver,
the query graph + evaluator, all v1 operators (deps/rdeps/kind/filter/attr/labels/somepath/
allpaths + set ops + let/$var), `--output=label|label_kind`, and the `cmd_query` CLI verb. NOT
the full Phase-1 DoD: the `qg` goldens are **razel-authored behavior sentinels**, not live
`bazel query` parity (see below).

**Commits (`p0..p1-engine`):**
- `e790f77` P1.0 — `razel-query` crate + boundary gate
- `abbf28c` P1.1 — §12 expression parser
- `9154846` P1.2 — load-only pattern resolver (moved into `razel-loading`; loaded types `pub`)
- `ab1bd84` P1.3 — query graph adjacency + core evaluator
- `d681653` P1.4 — predicates (kind/filter/attr/labels) + output
- `7829f5c` P1.5 — somepath / allpaths
- `543f19f` P1.6 — `cmd_query` CLI verb
- (this) P1.7 — `qg` harness + q1–q3 sentinels

**Verification.** ~30 razel-query unit tests + the q1/q2/q3 `qg` harness (9 cases) green;
`razel query` works end-to-end through the binary (deps/kind/label_kind + deferred-verb errors);
`cargo xtask gates` green (query stays analysis-free); full `cargo test --workspace` green except
the 2 carve-out reds.

**Rollback.** `git reset --hard razelv3-rust/p0` (Phase 1 is additive — a new crate + a CLI verb;
the only shared-crate change is the `discover_packages` move, behavior-identical).

**Remaining for full Phase 1 (`razelv3-rust/p1`):** LIVE `bazel query` golden capture — the
design's true `qg` (razel vs bazel). It needs `bazel` + a corpus both can query, and rides the
goldens xtask (like the aquery goldens); the `labels()` raw-vs-canonical question settles there.
Also the implicit-deps parity rung (Phase 6).

---

## `razelv3-rust/p2` — Phase 2 complete (`@crates` lock reader + materialization)

**Scope.** The full @crates source-of-truth + materialization (P2.1–P2.7): read `MODULE.bazel.lock`
(version-aware, loud errors), the `recordedInputs` grammar, stale-lock detection (sha256 rehash of
FILE inputs), tree-output capture in the executor cache, root `@crates` materialization (inline
contents), per-crate `RepoFetch` (download + sha256-verify + extract + drop BUILD), and the interim
dev-only bazel-external cache.

**Commits (`p1-engine..p2`):**
- `1a1f5a0` P2.1 — MODULE.bazel.lock reader (serde; reads the real 940KB lock)
- `9f925a8` P2.2 — recordedInputs grammar
- `32d08a8` P2.3 — stale-lock detection (sha2)
- `2c8701e` P2.4 — tree-output capture in razel-exec
- `a3c2cce` P2.5 + P2.6 — root materialization + real RepoFetch
- `355a0a8` P2.7 — interim bazel-external cache

**Verification.** ~18 razel-loading lock/materialize tests + razel-exec tree round-trip; a
network-gated test REALLY downloads + verifies + extracts blake3 1.8.2; `xtask gates` green; full
`cargo test --workspace` green except the 2 carve-out reds. New deps: serde/serde_json (lock) +
sha2 (digests) — gate-clean.

**Rollback.** `git reset --hard razelv3-rust/p1-engine` (Phase 2 is additive modules + the
behavior-preserving razel-exec copy_path refactor).

---

## Phase 3 — rolling per-step (the blake3 build, milestone 1) → `razelv3-rust/p3`

**Correction (do not repeat).** An earlier note here framed Phase 3 as a "too large / too coupled
to checkpoint safely" stop. That was a *manufactured* stop — turn-length dressed up as a roll-build
rule. Phase 3 decomposes into 12 small, independently-checkpointable steps (P3.1–P3.12) exactly
like Phases 0–2; each lands as its own green commit. The integration tag `razelv3-rust/p3` is
earned at the milestone-1 DoD (`razel build @crates//:blake3` → rlib; **aq**+**xp** green). Rolling
continues per-step; a stop fires only on a *real* rule, named explicitly.

P3.1 itself decomposes into a/b/c/d/e (load surface → selects → canonical mapping → alias-follow →
canonicalization wiring) — finer than the plan's single P3.1, each a green commit.

**Commits so far (`p2`..):**
- `993d7a6` P3.1a — `cargo:defs.bzl` load surface (`cargo_build_script`/`cargo_toml_env_vars`
  natives; STUB bodies — compile/run land in P3.6/P3.7)
- `1b53f6c` P3.1b — `crate_universe/private:selects.bzl` `selects` namespace (faithful
  `with_or`/`with_or_dict`; `config_setting_group` loud-deferred). blake3's full three-load surface
  resolves and the per-crate package loads (bare `select()` captured, not resolved at load).
- `81a9478` P3.1c — apparent→canonical `@crates` repo mapping (`CrateLock::canonical_repo`,
  accept-both-forms §11.3); prefix derived from the extension key; verified vs the real lock.
- `7bb2e32` P3.1d — build follows an alias top-label to its terminal `actual`
  (`analyze_workspace_with`); `@crates//:blake3` → `@crates__blake3-1.8.2//:blake3` (apparent).
- (infra, `6f38a03`) tfload uses 6 threads; **not** a gate — `perfgate` (synthetic, ≤20s) is the
  per-step perf gate.
- `0791370` P3.1e — canonicalize `@crates` labels to their `@@rules_rust++crate+…` identity
  (§11.3 accept-both-forms). **P3.1 (a–e) complete.**
- `0e9edf1` P3.2a — `rust_library`/`rust_binary` §5.5 attr verdict table: replace the silent
  `_kw` discard with an enumerated accept-vs-loud-error policy (P2#3). CompileArgv set shaped now
  (`crate_name`/`crate_root` overrides, `crate_features`→`--cfg=feature="x"`, `rustc_flags`);
  CompileEnv (`rustc_env`/`compile_data`/`aliases`/…) accepted **argv-inert** (P3.2b);
  Delegated (`proc_macro_deps`→P4.1, `target_compatible_with`→P3.4, `link_deps`→P4.5);
  Ignored (`data`/`tags`/`visibility`); unknown → loud error. Regression pin: delegated/ignored
  attrs leave the argv byte-identical. (P3.2 split a/b per the plan's "compile set first" hint.)
- `c2aaf1f` P3.2b — `compile_data` → Rustc action **inputs** (each entry via `resolve_dep`:
  source file → path, target → outputs). `compile_attrs` now drives extraction off the verdict;
  the env family (`rustc_env`/`version`/`pkg_name`/`rustc_env_files`) is accepted but **inert
  until P3.5** (it needs an `AnalyzedAction.env` field + the env-file format + precedence — P3.5's
  scope; the executor already carries per-action env). `aliases` (extern rename) deferred to a
  near step alongside the dep-aliasing path. Gate: `compile_data` is an input, not an argv token.
- `e7d291a` P3.3 — synthesize unvendored platform conditions from the host triple:
  `host::platform_condition_matches` resolves `@platforms//{cpu,os}:*` (→ `host_constraint_matches`)
  and `@rules_rust//rust/platform:<triple>` (→ `host_triple`), wired into `condition_matches`'
  config_specs-MISS branch (a declared `config_setting` still wins). `state::host_triple()` added;
  `toolchains.rs` reuses it. So blake3's `target_compatible_with`/cfg `select()` RESOLVES (P3.1b
  only loaded it). Gate: host triple resolves its arm; non-host/foreign-os → default.
- `c90e4fd` P3.4a — evaluate `target_compatible_with`: compatible iff every constraint holds
  (`is_incompatible` via `condition_matches`; `@platforms//:incompatible` never holds). An
  incompatible target gets NO actions + is flagged in `Session.incompatible_targets`
  (`SyncCell<BTreeSet>` — chosen over an `AnalyzedTarget` field: 49 literals across 15 files).
- `9f66a43` P3.4b — `analyze_workspace_with` loud-errors when an EXPLICIT (named top) or a
  transitive DEP target is incompatible (§5.4). Provably inert otherwise (the set is empty unless
  `target_compatible_with` is unsatisfiable). P3.4 split a/b/c (c deferred → P4.4).
- `a863cb9` P3.5a — `cargo_toml_env_vars` emits the `CARGO_PKG_*` env-file (§6.2): reads the
  crate's `Cargo.toml` `[package]` (minimal scan, no `toml` dep; `pkg_file_abs` resolves
  workspace/external like `resolve_dep`) → a `FileWrite` action, content baked at analysis. P3.5
  split a/b: the env PRECEDENCE + consumption (literal `rustc_env`/`version`/`pkg_name` override;
  `rustc_env_files` last-wins) rides the rustc wrapper (P3.9, `--env-file=`) — P3.5b.
- `4b3bd0d` P3.6 — `cargo_build_script` **compile (action 1)** (§5.2): the native (was a P3.1
  stub) now emits the build-script compile — `crate_root`/`srcs[0]` → a HOST `rust_binary`
  (`<name>_`, the §12 `:_bs_` bin) via `rustc`, linking `deps` as `--extern` (build-deps, NOT run
  inputs). Its OWN attr surface (distinct from §5.5's `rust_library`): `BsVerdict` table +
  `bs_compile_attrs` split attrs by phase — compile (`crate_name`/`crate_root`/`crate_features`/
  `rustc_flags`) shapes the argv now; run-phase (`version`/`pkg_name`/`data`/`links`/… → P3.8) and
  deferred-compile (`proc_macro_deps`/`rustc_env`/`aliases`) attrs are accepted-but-argv-inert;
  unknown → loud error. `default_info` stays **EMPTY** (§4.3: a build-script target exposes no
  libs — the bin is intra-target, consumed by the run action P3.8; so `deps=[":build_script_build"]`
  still yields no `--extern`, keeping `p31b` green). Tests `p36_build_script_compiles_to_a_host_bin_with_externs`
  + `p36_build_script_unknown_attr_is_a_loud_error`. NEXT: P3.7 (flags-file parser).
- `0ae893e` P3.7 — build-script **flags-file parser** (§6.1): new `build_script.rs` parses a
  build script's stdout → structured directives (the boundary between the run action P3.8 and the
  rustc wrapper P3.9). Recognized `rustc-*` → `FlagsRecord{kind,args}` in emission order WITH
  duplicates (link order is significant); `rustc-flags` tokenized HERE (whitespace) so the wrapper
  never re-parses/shell-quotes; `flags_file_jsonl` → one `{"kind","args"}` JSON object per line
  (JSON handles tab/space/`=`/quote). Side channels for P3.8: `metadata=K=V` (+ pre-1.77
  non-reserved single-colon `cargo:K=V`) → `dep_metadata` (`DEP_<LINKS>_K`); `warning=`→stderr;
  `error=`→fail; `rerun-if-*` recorded (not narrowing); unknown reserved `cargo::<key>` → one
  deviation line, never fatal. Reserved keys are colon-count-agnostic; non-reserved double-colon →
  deviation, single-colon → metadata. The `kind`→rustc MAPPING is **P3.9**'s (the wrapper's), NOT
  here. 3 goldens.

**P3.8 — SEAM DECISION RESOLVED: wrapper bin, ONE shared crate (option 2), Windows-capable.**
Gianni's steer: a wrapper BIN (not a `razel-exec` mnemonic special-case — that would force the
parser into a razel-exec-reachable crate + an `AnalyzedAction.env` field + teach the generic
executor about cargo). ONE wrapper crate hosts both subcommands so the flags-file schema is defined
once for the WRITER (build-script) and the READER (rustc, P3.9). **AND it must work on Windows.**
- `aa50654` P3.8a — new LIGHT crate **`razel-process-wrapper`** (rules_rust `process_wrapper`
  analogue; serde only, no starlark). The P3.7 flags module RELOCATED here
  (`razel-loading/build_script.rs` → `flags.rs`, now `pub` + a `read_flags_jsonl` reader for P3.9).
  `bs_runner.rs` = the Cargo-AGNOSTIC mechanism: `build-script --flags-out F --out-dir D [--env
  K=V]… [--env-file P]… [--rundir R] -- PROG …` → assemble a default-deny child env (precedence
  baseline < env-files(last wins) < explicit `--env` < `OUT_DIR`, §6.2) → run capturing stdout →
  parse §6.1 → `warning=`/deviation to stderr, fail on `error=`/non-zero exit, else write the JSONL
  flags file. **Windows:** writes via `std::fs` (NO `/bin/sh`, unlike P3.5a's FileWrite — that
  Unix-ism is now a follow-up to revisit), captures via `Command::output()`, seeds a Windows
  system-env baseline (`SystemRoot` etc.) so a default-deny child starts; Unix stays default-deny.
  7 tests. NAME NOTE: plan called the P3.9 crate `razel-rustc-wrapper`; generalized to
  `razel-process-wrapper` (hosts both) — flagged for veto.
- `69c6ba9` P3.8b — `razel-loading`'s `cargo_build_script` emits the RUN action (action 2) beside
  the P3.6 compile: `CargoBuildScriptRun`, argv `[razel-process-wrapper, build-script, --flags-out
  <name>.out, --out-dir <name>.out_dir, <env policy>, --, <bin>]`; run inputs = bin + build-script
  srcs + `data`/`compile_data` (§5.2 slice-1 static keying); outputs = the §6.1 flags-file +
  `OUT_DIR` tree; `default_info` stays EMPTY (§4.3). Env POLICY is razel-loading's (the wrapper is
  the Cargo-agnostic mechanism); this slice = `TARGET`/`HOST` (host==target triple), a default
  `OPT_LEVEL`, one `CARGO_FEATURE_<F>` per feature (uppercased, non-alnum→`_`). `bs_attrs` (was
  `bs_compile_attrs`) also extracts `data`/`compile_data`; new `process_wrapper()` resolver
  (`RAZEL_PROCESS_WRAPPER` override else the bare name). Test `p38b`.
- `5b31d09` P3.8c — the env-file leg (§6.2): `rustc_env_files` are env-file TARGETS
  (`cargo_toml_env_vars`, P3.5a) → resolved to their outputs → `--env-file <path>` (+ staged as run
  inputs); literal `version`/`pkg_name` → `--env CARGO_PKG_VERSION`/`NAME`, emitted AFTER the
  `--env-file`s so the wrapper (files-then-`--env`) lets the literal OVERRIDE the env-file. `bs_attrs`
  extracts `rustc_env_files`/`version`/`pkg_name`; the `analyze` fixture gains a `Cargo.toml`. Test
  `p38c`. **P3.8 run env now covers the statically-derivable §5.2 allowlist.** The remaining
  allowlist legs are tracked as explicit steps (NOT a §10 long-tail — that's only scripts that go
  OUTSIDE the allowlist): **P3.8d** = `CARGO_CFG_*` (triple→cfg derivation) + cc `CC`/`AR`/`CFLAGS`
  (`razel-cc-toolchain`) — IN Phase 3, **required for the P3.12 SIMD-`.o` execution-parity golden**
  (blake3's `build.rs` keys SIMD off `CARGO_CFG_TARGET_*`); **P4.5** = `DEP_<LINKS>_*`
  (cross-build-script links channel — blake3 doesn't exercise it). NEXT: **P3.8d** then **P3.9** —
  the wrapper's `rustc` subcommand (READ the flags file
  → `--cfg`/`-l`/`-L`/`-C link-arg`/env; `read_flags_jsonl` already exists; the `kind`→rustc
  mapping table + the explicit-argv shape `[wrapper, rustc, --flags-file=…, --env-file=…, --, <rustc
  args…>]`; empty flags-file = no-op passthrough). **`AnalyzedAction` still has no `env` field — the
  wrapper carries env (P3.5b env precedence rides P3.9).**
- `0dc04e3` P3.8d **leg 1/2 — `CARGO_CFG_*`** (§5.2): the run action emits the `CARGO_CFG_*` set
  cargo derives from the (host==target) triple via `cargo_cfg_env()` (TARGET_ARCH/OS/FAMILY/VENDOR/
  ENV/POINTER_WIDTH/ENDIAN/FEATURE + UNIX/PANIC); `TARGET_FEATURE` is a per-arch baseline
  (x86_64=`fxsr,sse,sse2`, aarch64=`neon`) refined at P3.12. Tests `p38d`. **Leg 2/2 — cc
  `CC`/`AR`/`CFLAGS` — RE-SEQUENCED to ride P3.12** (not a standalone pre-P3.9 step): `razel-cc-
  toolchain` exposes compile/archive ARGV, not the env-var form a build script's `cc` crate reads,
  so it needs a new accessor + a toolchain-selection decision; and the `cc` crate DEFAULTS to system
  clang absent `CC` (so blake3's `.o`s compile functionally without it). Exact `CC`/`AR`/`CFLAGS` is
  a hermeticity/parity concern best pinned against the P3.12 golden — guessing values now risks
  diverging from both the `cc` crate default and Bazel. (Still in-project, still Phase 3, just at the
  golden.)
- `092c5a8` P3.9 — the process-wrapper's **`rustc` subcommand** (§6.1/§4.1, the flags-file READER;
  P3.8 was the WRITER). `rustc --rustc=PATH [--flags-file=F] [--env-file=F]… [--env=K=V]… -- <rustc
  args…>` → read the §6.1 flags file (`read_flags_jsonl`) → the normative `kind`→rustc map
  (`rustc-cfg`→`--cfg`, `-link-lib`→`-l`, `-link-search`→`-L`, `-link-arg`/`-cdylib-link-arg`→`-C
  link-arg`, `rustc-flags` appended verbatim, `rustc-env`→process env NOT argv) → append to the
  original rustc argv + exec. Empty/absent flags file = no-op passthrough. **P3.5b** env precedence:
  baseline < `--env-file` < build-script `rustc-env` < literal `--env` (highest). Factored a shared
  `env.rs` (`platform_baseline` + `base_env`) used by BOTH subcommands; `bs_runner` delegates to it.
  `razel-process-wrapper` 11/11. **The wrapper crate is now feature-complete for the build-script
  pipeline.** NEXT: **P3.10** — wire the build-script EDGE (§4.3): a `BuildScriptRun` projection so a
  crate's rustc action routes through the wrapper (consuming the flags-file + `OUT_DIR`), and
  `:build_script_build` is never an `--extern`. **Touches `DepInfo`/the provider fold + every
  `rust_library`/`rust_binary` rustc action** — the broad core-path rewire (additive: only
  build-script-dep crates wrap).
- `8e0fbdd` P3.10 — **wire the build-script edge** (§4.3). SEAM resolved (see below) → Option B:
  `BuildScriptRun` is an OWN-only provider (`flags_file`/`out_dir`, `Set`, `dep_fold: None` — modeled
  exactly like `DefaultInfo.files`), so it's read DIRECTLY off the dep and NEVER folds transitively
  (a crate's consumers must not inherit its build-script flags). `resolve_dep` → `DepInfo.build_script`;
  `extern_args` collects build-script edges separately and never `--extern`s them; `apply_build_script_edge`
  rewrites the crate's rustc argv to `[process-wrapper, rustc, --rustc=…, --flags-file=…,
  --env=OUT_DIR=…, --, <orig argv>]` + stages the flags-file + `OUT_DIR` as inputs. **Additive** — a
  plain crate is unchanged (the cc/java carve-outs + the `p3x` argv goldens didn't move). Slice-1 =
  ≤1 build script/crate (loud error on >1). Tests `p310`. **The build-script pipeline is now wired
  end-to-end at the ANALYSIS level (P3.6 compile → P3.7 parse → P3.8 run → P3.9 wrapper → P3.10
  edge).** NEXT: P3.11/P3.12 — blake3 analysis + execution PARITY vs LIVE bazel (needs the bazel
  golden-capture path; the cc/java carve-outs are red here = that capture isn't wired/available in
  this env, so blake3 parity likely faces the same — verify before diving in).

**P3.11 — RESCOPED (Gianni): local build-script parity, not external blake3.** PROBE found razel's
build path can't resolve the real external target — `razel build @crates//:blake3` → `unknown
target: blake3` (and `razel query` defers external `@crates//` to "q4 — §13"). So the full external
`@crates//:blake3` analysis is an unstarted Milestone-1 integration (external repo/alias loading into
the build path + the ~20-crate closure), NOT the plan's "~250-line golden+test". Rescoped to a LOCAL
case proving the same §4.3 edge:
- `6fa3cfd` — `parity/corpus/rust/build_script/` (a `rust_library` `withbs` + its `cargo_build_script`
  `build_script_build`; `build.rs` emits a `rustc-cfg`). **Bazel-verified**: `aquery` → 3 real build
  actions (CargoBuildScriptRun + the build-script bin Rustc + the crate Rustc through
  `process_wrapper --env-file/--arg-file`), matching razel's P3.6/P3.8/P3.10 shape. Golden raw-captured
  to `/tmp/bs_corpus_aquery.txt` (full deps closure at `/tmp/blake3_aquery.txt`, reusable).
- REMAINING (the parity normalizer — substantial, a norm_version 2→3 bump → re-capture rust goldens):
  `razel-parity` already handles the hard parts (`source_inputs` drops the toolchain-input closure;
  hash/cfg/repo normalize; the `diff` `omit` mnemonic-list for the bazel infra actions —
  Symlink/RunfilesTree/RepoMappingManifest/SourceSymlinkManifest/SymlinkTree/ExecutableSymlink). What's
  NEEDED: (a) a **wrapper-prefix + leading-rustc-binary strip** (canonicalize Bazel's `process_wrapper …
  -- <rustc>` AND razel's `razel-process-wrapper rustc --rustc=… --` both to the bare rustc args —
  needed for ALL rust parity, hence rust has no test yet); (b) a **rustc-flag DEVIATION POLICY** —
  razel's lean argv vs Bazel's rich one (`-Cmetadata`/`--extra-filename`/`--remap-path-prefix`/
  `--error-format`/`--cap-lints`/lint flags); the current `diff` is argv-order-strict, so these must be
  allowlisted/normalized. **OPEN: the deviation policy is a parity-faithfulness call (Gianni's, like the
  cc `CppModuleMap` omit).** Then wire a `rust_graph_parity.rs` test (mirrors `graph_parity.rs`).

**P3.10 build-script-edge modeling — RESOLVED: Option B (own-only `BuildScriptRun` provider).** The
edge is INTRA-TARGET, so the transitive projection fold (the `CcInfo.hdrs` pattern) would be a
CORRECTNESS BUG — it'd propagate blake3's build-script flags to every crate that depends on blake3.
`DepInfo.fields` only carries transitively-folded projections, and `Scalar` fields are skipped by the
fold entirely — neither fits. The fit is `dep_fold: None` (own-only) + a direct read in `resolve_dep`,
which `DefaultInfo.files`/`libs` already does. Rejected: A (a new direct-only `FoldPolicy` — redundant
with the own-only pattern), C (no provider; marker + direct `AnalyzedTarget` read — reintroduces the
rule-specific special-casing C3a removed).

**P3.4c — OPEN SEAM DECISION (wildcard-skip; razel-loading → razel-cli).** §5.4's last rung: a
WILDCARD build (`//...`) must SKIP incompatible targets, not error. The wildcard loop is
`cmd_build_many` in `razel-cli` (`expand_pattern` → per-label `build_workspace_with`), but
`build_workspace_with` → `analyze_workspace_with` now ERRORS on an incompatible target (P3.4b). So
the loop can't just build each expanded label. How does it learn "incompatible, skip" without the
error? (A) `cmd_build_many` catches the incompatible error string and skips that label (no new API;
string-fragile, conflates with genuine errors). (B) a `razel-loading` API that analyzes and returns
compatibility WITHOUT erroring — `cmd_build_many` filters, the explicit path keeps P3.4b's error
(clean seam; small new surface). (C) `analyze_workspace_with` gains an explicit/wildcard MODE param
(skip vs error) — one entry, but threads provenance through the loader.
**DECISION (Gianni):** DEFER P3.4c — it's not on blake3's milestone-1 (named) path and pairs with
P4.4's incompatible-target golden (absent-in-wildcard); land it there (approach B preferred). Roll
to P3.5 (env-file) next, on the critical path.

**P3.1e — SEAM DECISION RESOLVED: double-`@` everywhere (option A).** Gianni's steer was
"double-`@` canonical everywhere now" — the design's true identity, faithful, no deferral to a
parity normalizer. Wiring:
- `GlobalFlags.crate_lock: Option<Arc<CrateLock>>` gates canonicalization; `None` for every
  non-`@crates` build, so the central label path is **byte-identical** there (verified: full
  workspace green except the 2 carve-outs; `perfgate` scaling **1.98**, unchanged).
- `canon_label` wraps `canon_label_inner` with `canonicalize_crate_repo`, which maps
  `@crates`/`@crates__*` → `@@rules_rust++crate+…` via `CrateLock::canonical_repo`. Repos the lock
  doesn't define (`@rules_rust`, `@platforms`) keep their apparent single-`@` form.
- `analyze_workspace_with` seeds `crate_lock` from `<root>/MODULE.bazel.lock` (read-if-present;
  a caller/test-seeded lock wins; a malformed lock stays `None`).
- `load_package` made `@@`-tolerant (`trim_start_matches('@')`) so the canonical repo resolves the
  real canonical-named external dir.
- The OTHER single-`@` extraction sites (`glob.rs`, `deps.rs`, `decls.rs`, `engine.rs`) stay
  single-`@` for now; they earn `@@`-tolerance as P3.2+ real-crate tests exercise them
  (verify-first, no speculative edits).

**Remaining for `razelv3-rust/p3`:** P3.6→P3.10 done (build-script pipeline wired end-to-end at
analysis). **P3.11/P3.12 are SUPERSEDED by [`RazelRustParityPlan.md`](RazelRustParityPlan.md)**
(2026-06-16) — the P3.11 probe found razel's rust argv is a lean ORIGINAL (`19321b7`), structurally
far from rules_rust's (syntax + the hashed-output model + ~15 flags), and rust parity was never gated
(cc/java have a parity test, rust never did). So blake3 parity is a faithful-argv rework with a rust
parity gate that LEADS (RazelRustParityPlan Phase A, on a local build-script corpus case) + the
external `@crates//:blake3` integration (Phase B) — not "~250 + a normalizer". NEXT = that plan's **A1**
(wire `rust_graph_parity` RED as the driver). Carried there: P3.8d leg 2 (cc env) → B4; `proc_macro_deps`
→ P4.1 (B2 surfaces it). P3.8d leg 1 `CARGO_CFG_*` done; `DEP_<LINKS>_*` is **P4.5**, Phase 4.

**RazelRustParityPlan Phase A — roll progress:**
- `17d4ab8` **A1** — `rust_graph_parity` wired RED as the driver (the gate leads) + `razel_parity::
  canonicalize_rust_argv` (strips Bazel's `process_wrapper` + razel's `razel-process-wrapper` prefix →
  bare rustc args). razel analyzes the local case cleanly; the diff is the work-list.
- `06f0c20` **A2** — `state::out_path` roots GENERATED outputs at `bazel-out/<cfg>/bin/…` under
  `--bazel_build_compat` (the parity posture); the prefix gap closed. razel-scoped (cc/java/unit-tests
  untouched).
- `1ed7fbb` **(b)** — CargoBuildScriptRun is the DOCUMENTED DEVIATION (RR's call): its output format
  (§6.1 single JSONL `.out` vs Bazel's split `.flags`/`.env`/… files) is intra-target plumbing →
  omit-listed + logged, not a §6.1 rework.
- `e2d8a42` **A3–A6 (the Rustc argv+output faithfulness)** + `422da03` **A6 (--extern + transitive)** —
  **MILESTONE A: rust analysis-parity GREEN** (`rust_graph_parity` 2/2: the build-script edge + the
  transitive baseline; documented deviations only). razel's rust Rustc argv is now rules_rust-faithful:
  crate (rlib) = the ~15-flag argv in Bazel's order (`--crate-name=`/`--crate-type=rlib` joined, the
  `--out-dir`+`--codegen=extra-filename/metadata` hashed-output model → `lib<name>-<hash>.rlib` via
  `metadata_hash`, `--target`/`--emit`/`--error-format`/`--color`/`-Cembed-bitcode`/`--codegen=opt-level/
  debuginfo/strip`); deps = joined `--extern=<n>=<rlib>` + `-Ldependency=<dir>`; build-script bin =
  `crate-type=bin`, exec config (`opt-level=3`/`strip=debuginfo`), `--emit=link=`+`.dSYM`. **Documented
  deviations** (`razel-parity`, logged): `canonicalize_rust_argv` (wrapper strip) +
  `strip_rust_deviation_flags` (`--sysroot`/`-L`/`--remap-path-prefix`/`--codegen=linker`/`link-arg` —
  razel's system rustc + no cc-toolchain rust links) + the ` (TreeArtifact)` annotation strip
  (norm_version 3) + the build-script flags-file inputs (the (b) JSONL-vs-split deviation, flowing into
  the crate's consumption). The `p32`-era unit tests were updated off the lean argv (the entrenchment).
- `2b1dbd5` **A7.1** — `razel-exec` output capture is now **dir-aware + skip-absent** (the shared
  `copy_path` in capture/store/restore): the `OUT_DIR` / extracted-`.crate` TREE outputs round-trip,
  and an unproduced declared output (the macOS `.dSYM` a `debuginfo=0` build omits — declared only to
  match Bazel's graph) is a no-op, not a hard `os error 2`. Symmetric with the build driver digesting
  only inputs that exist; a skipped output that is actually consumed fails loudly downstream.
- `6d31921` **A7.2 — MILESTONE A′: rust EXECUTION parity GREEN** (xp). `razel-process-wrapper/tests/
  exec_parity.rs` drives a full `razel build //corpus/rust/build_script:withbs` with the REAL toolchain
  (system rustc routed through the wrapper, resolved via `CARGO_BIN_EXE_razel-process-wrapper`) into a
  temp workspace and diffs the execution surface vs Bazel's: (1) the `withbs` rlib is produced (valid
  `ar` archive); (2) the build script's `cargo::rustc-cfg=buildscript_ran` reaches the crate's rustc as
  `--cfg buildscript_ran`, matching Bazel's `build_script_build.flags` (`golden.flags`, captured via
  `bazel build`). razel emits ONE §6.1 JSONL `.out`; Bazel splits it into `.flags`/`.env`/… — the (b)
  format deviation — so the diff is at the DIRECTIVE level (the wrapper's `apply_flags`). Cold cache
  runs all 3 actions. Verified empirically: the system rustc shim runs under the sandbox's stripped env
  (no HOME needed); the wrapper's empty-unix-env is fine for the rlib (no link) + the self-contained
  build-script bin. **Phase A COMPLETE** (analysis A1–A6 + execution A7).
**Phase B — external `@crates//:blake3` (Milestone-1) — roll progress:**
- `3e19ed8` **B1.1** — `build_one` routes external-repo labels (`@crates//:blake3`) to the workspace
  build path (was: bare-name branch → "unknown target: blake3"). Probe advanced to "not vendored".
- `3638e8e` **B1.2** — `materialize_crates_world(lock, base)`: from the seeded `CrateLock`, materialize
  the root `@crates` repo + EACH per-crate generated `BUILD.bazel` under its canonical name, inline,
  **NO `.crate` fetch** (analysis is lock-only; the source is B4/`fetch_crate`). Pure + unit-tested.
  KEY FINDING: the lock carries `build_file_content` inline → B1–B3 need NO fetch; the fetch/vendor
  decision (`fetch_crate` vs `read_from_bazel_external`) is real but bites only at **B4** (execution).
- `c4e2cdd` **B1.3 — B1 GATE MET**: `analyze_workspace_with`, once the lock is seeded, materializes the
  world into a workspace-local `.razel-crates` (gitignored) + points `fetched_external_base` at it.
  Guarded by `crate_lock.is_some()` + no caller base → inert for the parity corpus (its lock has no
  `@crates`) + the vendored-dir unit tests. `razel build @crates//:blake3` now gets PAST target
  resolution + INTO analysis (probe: "unknown target" → "not vendored" → evaluating the root `@crates`
  BUILD). `rust_graph_parity` 2/2 + lib 75/0 unaffected.
- **REMAINING:** **B2** — the full closure analyzes. FIRST gap (the probe's current error): the root
  `@crates` BUILD calls `glob()` and razel's `glob()` errs "needs a package on disk" on the materialized
  external package (note the `@@…+crates///BUILD` triple-slash — the root package's empty sub-path).
  Then the ~20-crate closure (proc-macro deps → P4.1, cc SIMD, per-crate `crate_features`/
  `target_compatible_with`). Then **B3** (blake3 aq parity) + **B4** (blake3 xp parity + cc env — the
  fetch/vendor decision lands here). Phase A (local analysis + execution) stays fully gated.
Deferred: **P3.4c** (wildcard-skip) → lands with **P4.4**'s incompatible-target golden.
(`cargo_toml_env_vars` + env-file; makes the `rustc_env`/`version`/`pkg_name`/`rustc_env_files`
env family + `aliases` live) → P3.6–P3.10 (build-script compile/run, flags parser, rustc wrapper,
build-script edge) → P3.11/P3.12 (analysis + execution parity vs live `bazel aquery`/`bazel build`).

**Per-step gate scope (velocity).** The per-step green check is TIERED to the crate changed —
**not** `--workspace` (touching `razel-loading` rebuilds ~24 downstream crates, ~7m; bare
`--workspace` adds doc-tests, ~16m). Loading-layer step = `cargo test -p razel-loading --lib` +
the 2 carve-out sentinels (`--test graph_parity --test java_graph_parity`), ~27s — skips the
network test (`fetch_extract`) + ~28 other integration binaries. **`--lib --tests` is too slow even
as a pre-commit tier** (Gianni, 2026-06-15, P3.6): linking ~30 integration binaries takes minutes,
and for a step whose surface is fully covered by lib tests (e.g. a new `rust_rules` native exercised
by `p3*` lib tests) it adds no signal. Commit on the tight tier when lib tests cover the change;
reserve `-p razel-loading --tests` for steps that actually touch the integration-test surface;
`--workspace --lib --tests` only at a phase tag. (`xtask gates` + `xtask perfgate` unchanged;
`tfload` never a gate.)
(condition source + `target_compatible_with`) → P3.5–P3.10 (env-file, build-script compile/run,
flags parser, rustc wrapper, the build-script edge) → P3.11/P3.12 (analysis + execution parity vs
live `bazel aquery`/`bazel build`). The live-bazel parity capture rides the goldens xtask.
