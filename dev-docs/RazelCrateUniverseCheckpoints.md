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

**Full Phase 1 (`razelv3-rust/p1`) — DONE: LIVE `bazel query` parity.** The design's true `qg`
(razel vs LIVE bazel) now lands, riding the goldens xtask like aquery:
- `27fd23f` — `:*` / `:all-targets` (all targets incl source/generated files + `BUILD`; was `:all`=
  rules only). Filters the graph's `kinds` (file edge-targets included) by package + adds `//pkg:BUILD`.
- `7bfe412` — `labels()` emits CANONICAL labels (`:base`→`//pkg:base`, bare `n`→`//pkg:n`); was raw.
  **This settles the raw-vs-canonical question: bazel is canonical, so razel canonicalizes.**
- (this) — `cargo xtask capture-query-goldens` captures `bazel query --noimplicit_deps <expr>` for a
  7-op battery (`:all`/`:*`/kind/deps/rdeps/labels/somepath) over `corpus/rust/transitive` →
  `query_goldens.txt`; the `live_query_parity` test (hermetic, no bazel at test time) asserts
  `razel query` byte-matches. GREEN across all 7. (Before this, the other 5 ops already matched —
  the only gaps were `:*` + `labels()`.)
**The only remaining divergence is the `--implicit_deps` default** (bazel-on/razel-off) — with
`--noimplicit_deps` on both, `deps()` is byte-identical. That's the **Phase 6** rung (q5+), as designed.

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
- `0ac993d` **B2 gap-1 (glob)** — `do_glob` trims ALL leading `@` (was one) so the canonical `@@repo//`
  external package resolves; the root `@crates` BUILD's `glob()` passes.
- `1baa891` **B2 GATE MET — the full `@crates//:blake3` closure ANALYZES** (~20 crates: digest,
  generic-array, typenum, crypto-common, subtle, rayon-core, crossbeam-{utils,epoch,deque}, …). Two
  more gaps closed: (i) **lazy fetch** — `materialize_one_repo` (replaced the wrong-premise BUILD-only
  `materialize_crates_world`): per-repo ON DEMAND from `load_package_body` (root inline; per-crate
  `fetch_crate`), so only the loaded closure (~15) is fetched, not the 140-crate lock. **CORRECTED the
  B1 "no fetch until B4" finding** — analysis genuinely reads source (generated BUILD globs `**/*.rs`,
  sets `crate_root`, `cargo_toml_env_vars` reads `Cargo.toml`); bazel pre-fetches the closure, razel must
  too (network OK here; `fetch_crate` is the parity path). (ii) **`@@` data-file resolution** —
  `deps.rs` + `decls.rs` trim ALL `@` so a canonical-repo data/srcs file (blake3's `data=glob(['**'])`
  sweeps in `Cargo.lock`) resolves vs "not analyzed". No proc-macro in blake3's closure (P4.1 stays
  deferred); cc SIMD is inside blake3's `build.rs` (`cc::Build`) → execution/B4. Gate: B2 dev-probe
  `blake3_closure` (ignored — fetches over network). `rust_graph_parity` 2/2 + lib 75/0 + gates + perfgate.
- **B3 fixture decided by "what does bazel do":** bazel commits the lock, FETCHES sources
  (sha256-verified) into a cache, never vendors — so fixture **(b)** (reuse razel's root `@crates` + a
  network-gated test + `fetch_crate`) IS the bazel-faithful model; vendoring sources (a) diverges. RR's
  guiding rule resolved it.
- `c93a5df` **B3 golden** — `cargo xtask normalize-golden <in> <out>` (filter_aquery + normalize for a
  manually-captured external aquery, since `capture-goldens` only reaches `parity/corpus` BUILD cases) +
  committed `parity/corpus/rust/crate_blake3/{golden.txt,meta.toml}` (blake3's own action blocks from
  `aquery 'deps(@crates//:blake3)'`, pinned blake3-1.8.2).
- `88c71ee` **B3 — blake3 rustc argv FAITHFUL through the externs.** The `blake3_closure` test (ignored/
  network) + diff: `missing: []`, `extra: []`, **inputs match**, OMITs work (CargoBuildScriptRun +
  ExtractCargoTomlEnvVars/`FileWrite` + infra). Crate-compile argv matches Bazel token-for-token through
  ALL 6 `--extern` (indices 0–28). Three fixes: (i) `qualify` trims ALL leading `@` → external output
  paths `external/<repo>/…` not `external/@<repo>/…` (the `--out-dir`/`-Ldependency` divergence; same `@@`
  family); (ii) `compile_extras` emits feature cfgs as TWO tokens (`--cfg` `feature="x"`) POSITIONED after
  `--target`, `rustc_flags` (`--cap-lints`) at the end — rules_rust's order; (iii) p32/p36 updated.
- `d6b90d9` **B3 — blake3 ANALYSIS PARITY GREEN (Milestone-1 analysis half).** The last deviation closed
  (RR sanctioned the reco): `transitive_rlib_dirs` walks the analyzed dep graph (`results[canon].deps`)
  for the TRANSITIVE rlib-dir closure → `-Ldependency` per transitive crate (rules_rust's form; also
  needed for B4 so rustc finds transitive `.rmeta`). Only rlib-producing CRATES are collected/recursed —
  a `cargo_build_script` (`_bs`, bin output) is the intra-target edge, so recursing it is SKIPPED (else
  BUILD-only deps like `version_check` leak in, which Bazel's crate-compile `-Ldependency` excludes — that
  was the final 1-entry diff). The parity diff compares `-Ldependency` ORDER-INSENSITIVELY (sorted in
  `b3_rustc_argv`, both sides) — rustc search paths are order-free, so only the SET must agree.
  `blake3_analysis_matches_the_bazel_golden` GREEN (ignored/network); `rust_graph_parity` 2/2 + lib 75/0 +
  gates + perfgate unaffected.
- `a7444fb` **B4 step-1** — the build path resolves the `@crates//:blake3` ALIAS to its canonical target
  (`analyze_workspace_resolved` returns the resolved canonical top; `build_workspace_with` starts
  `execute_jobs` there). `razel build @crates//:blake3` now gets PAST target resolution INTO execution.
- `9dee91c` **B4 exec-root (RR chose (c): "always do it properly")** — `prepare_exec_root`: a separate
  `.razel-exec` symlink FOREST (workspace source entries + `external/<repo>` → the fetched `.razel-crates`
  repos), so external sources resolve at the declared `external/<repo>/…` path and outputs land in
  `bazel-out/`, never the source cache. External builds use it; pure-local (A7) keep `exec_root=root`.
- `8e3702d` **B4 transitive rlibs as inputs** — `transitive_rlibs` (the rlib FILES) staged as action
  inputs so the per-action sandbox has the transitive closure (a direct rlib refs its deps by name+hash
  → rustc loads their `.rmeta`); fixed "can't find crate `shlex` which `cc` depends on". Parity-neutral
  (the diff drops `external/<repo>/` inputs).
- `7261f03` **B4 env-file producer dep** — `cargo_toml_env_vars` (the `--env-file` producer) added to the
  build-script target's `deps` so `collect_order` BUILDS it before the run (else the run ENOENT'd on the
  missing `--env-file`). Parity-neutral.
- `7c048a5` **B4 cc SIMD — blake3 ITSELF FULLY BUILDS (incl the cc SIMD).** The build script now runs
  with cwd = the crate root (`--rundir` = the crate src dir) + ABSOLUTE OUT_DIR/program (the wrapper
  absolutizes against its sandbox cwd, chdir's the child, sets `CARGO_MANIFEST_DIR`) — so cc-rs's
  relative `build.file("c/blake3_neon.c")` resolves and the SIMD `.o`s compile (system `cc`; exact
  CFLAGS not gated — CargoBuildScriptRun is OMIT'd). Parity-neutral (run argv OMIT'd): A7 ok, rust 2/2,
  blake3 analysis GREEN, wrapper 11/0.
- **B4 EXECUTION COMPLETE — `razel build @crates//:blake3` builds the FULL external closure end-to-end
  under the (c) exec-root (compat posture); outputs isolated in `bazel-out`, source cache CLEAN.** Four
  interlocking fixes (all parity-neutral; rust 2/2, lib 29/9/75/11, gates + perfgate green):
  1. **build-script bin COMPILE → wrapper** (`rust_rules.rs`): when a crate declares a Cargo env, the
     `_bs` bin compile routes through the wrapper's `rustc` subcommand with the crate's `--env-file` +
     literal `version`/`pkg_name`, so `build.rs`'s compile-time `env!("CARGO_PKG_NAME")` (crossbeam-utils)
     resolves. `canonicalize_rust_argv` strips the wrapper prefix; the env-file lives under
     `external/<repo>/…` so `source_inputs` drops it → parity-neutral (p36 test updated for the wrapped
     shape).
  2. **wrapper rustc gets `PATH`** (`rustc.rs`): razel omits Bazel's `-Clinker=<abs cc>` (documented
     deviation → SYSTEM linker), so when rustc LINKS a `bin` it must resolve `cc`/`ld` via PATH; the
     default-deny env carries none on unix → pass the wrapper's own (env is runtime, not graph → neutral).
  3. **abs-OUT_DIR → exec-relative rewrite** (`bs_runner.rs`): the build script ran with an ABSOLUTE
     OUT_DIR inside its EPHEMERAL sandbox, so cc-rs emitted `rustc-link-search` under a dead dir;
     rewrite that prefix back to the exec-root-relative `--out-dir` (where the OUT_DIR tree is staged
     for the consuming compile) — also how Bazel expresses build-script link paths.
  4. **DIRECTORY (tree) inputs digested + staged** (`razel-build` `digest_input`): a `cargo_build_script`
     OUT_DIR is a TREE input to the consuming crate compile (blake3's `libblake3_neon.a`), but
     `std::fs::read`'s `EISDIR` silently dropped it from BOTH the content key AND sandbox staging → the
     rlib compile's `-Lnative` found nothing. Now a dir input hashes its tree (sorted) → enters the map
     → symlinked into the sandbox. (Latent until blake3: the local A7 build script only emits a `--cfg`,
     never references OUT_DIR contents.) Fixes cache correctness too. + `RAZEL_KEEP_SANDBOX` debug hook
     (the Bazel `--sandbox_debug` analog) used to root-cause this.
- **REMAINING (Milestone-1 close): the xp GATE** — a test that LOCKS this in (build the blake3 closure
  in the compat posture → assert the rlib `!<arch>`, the SIMD `.o`s / `libblake3_neon.a` in the OUT_DIR
  tree, + the build-script flags-file). The execution WORKS; the gate makes it permanent → **Phase B /
  Milestone-1 COMPLETE.** **Milestone-1 ANALYSIS half (B1–B3) DONE + gated**; Phase A + local execution gated.
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

---

## Track A Phase 4 — proc-macro, richer select, full @crates (in progress)

- `b6cef5d` **P4.1 — `rust_proc_macro` native + `proc_macro_deps`** (§5.3): `--crate-type=proc-macro`
  → `lib<name>-<hash>.{dylib,so}` (host suffix); own-only `RustProcMacro` provider marker (registered,
  no dep_fold); `DepInfo += proc_macro`; `extern_args` routes a proc-macro dep to `--extern <name>=
  <dylib>` but OUT of the rlib `-Ldependency` closure; `proc_macro_deps` (was Delegated) extracted +
  merged into the extern set for rust_library/binary. Unit-gated.
- `18340a2` **P4.2a — faithful proc-macro argv**: rewrote `native_rust_proc_macro` from the lean slice
  to rules_rust's A3/A5 form (joined flags, hashed `metadata`/`extra-filename`, `--emit=dep-info,link`,
  compile_attrs, build-script edge). HOST compile.
- `f01e159` **P4.2b — serde_derive analysis-parity golden GREEN** (§9.2, Milestone 2): live
  `bazel aquery` golden for the real `@crates` serde_derive (`crate_serde_derive/{golden.txt,meta.toml}`,
  captured like blake3 B3; network-gated test). Closed the two argv gaps it surfaced: `--target=<host>`
  (rules_rust passes it even for the host proc-macro) + bare `--extern proc_macro` (the implicit sysroot
  crate). razel == bazel modulo the documented deviations. lib 76/0, rust_graph_parity 2/2, gates OK.
- `d675112` **P4.3 — richer per-cfg `select()` deps** (§5.4): the libc/getrandom shape
  (`deps = select({<triple>: …, default: …})`) already resolved through the shared select path
  (str_attr_parts → resolve_str_parts → pick_branch → extern_args) from P3.3 — proven by three p43
  units (host-arm dep extern'd; non-host/unfetched arm selected away; `selects.with_or` over triples).
  The gate surfaced a real §5.4 bug: an incompatible target sitting ONLY in a non-taken arm tripped the
  loud error (the check scanned ALL recorded targets). Fix (`rules/pkg.rs`): walk `canon`'s closure and
  error only when an incompatible target is actually REACHED — never on mere presence. Unit-gated.
- `22b652d` **P4.4a — getrandom richer-per-cfg golden GREEN** (§9.3): captured the `bazel aquery`
  golden (`crate_getrandom/{golden.txt,meta.toml}`, norm v3). GREEN first run — getrandom's rlib Rustc
  action is byte-identical to bazel's (externs cfg_if+rand_core+libc = the resolved unix/host arm;
  wasm/windows arms selected away). Confirms P4.3 on a real crate; no engine change needed.
- `a8c44c3` **P4.4b — incompatible negative case** (§5.4 / the deferred P3.4c): wildcard SKIPS,
  named ERRORS. `load_tree_report_with_targets` (sole caller `expand_pattern`) filters out
  `incompatible_targets` — absent from the wildcard set (matching bazel); the explicit single-label
  path keeps the loud error. Gated `expand_pattern_skips_incompatible_targets` + existing p34b_named.
  *(= Milestone 3.)*
- **P4.5 — `link_deps` propagation** (§5.5/§6), four green steps:
  - `b3f8cce` **P4.5a** wrapper `--link-flags-file` — `link_args_only` contributes a transitive build
    script's LINK directives only (cfg/env stay intra-target).
  - `cbc7a01` **P4.5b** analysis: transitive `RustLinkInfo` projection — a `rust_library` publishes its
    own build-script flags-file; a consuming `rust_binary`'s FINAL link inherits the closure's via
    `--link-flags-file` (NOT applied at the rlib — rules_rust's posture).
  - `8855a39` **P4.5c** wrapper `DEP_<LINKS>_*` — a links crate's `metadata=K=V` persists in `.out`;
    `--dep-metadata <LINKS>=<file>` injects `DEP_<LINKS>_<KEY>` into a dependent build script's env.
  - `b2d0921` **P4.5d** analysis: `cargo_build_script` captures `links` + `link_deps` → emits
    `--dep-metadata` for each link_dep. Synthetic shape (no corpus crate consumes DEP_* yet — the
    design's deferred long tail); inert for non-links crates → byte-identical parity.
  - All P4.5 steps: byte-identical for the existing cases (blake3/getrandom/serde_derive goldens +
    rust_graph_parity green throughout).
- `a57a258` **P4.6 — full `@crates` scale + retire the interim cache** (§9.4, Milestone 4):
  (1) RETIRED the P2.7 interim cache (`read_from_bazel_external` + `copy_tree` + test + export) — it
  was already dead (only its own test called it; `materialize_one_repo` → `fetch_crate` is the live
  path), so by rung 4 the pure `RepoFetch` path is the one under the goldens. (2) FULL scale proven:
  `probe_starlark_closure_analyzes` (#[ignore], network) — `@crates//:starlark` (razel's heaviest dep)
  resolves end-to-end to **266 targets** on the pure fetch path (proc-macros + build scripts + per-cfg
  selects), zero unmodeled attr/select/dep. The three aq goldens (blake3 rlib / serde_derive proc-macro
  / getrandom richer-per-cfg) stay the parity gate; engine is crate-agnostic, so 266-clean + diverse
  goldens is the scale signal. lib 80/0, goldens 3/3.

**Phase 4 DoD — REACHED.** Milestones 2–4: proc-macro (P4.1/P4.2) + richer per-cfg select (P4.3) +
libc/getrandom & incompatible negative (P4.4) + link_deps native-link/DEP_* propagation (P4.5) + full
`@crates` build on the pure fetch path with analysis parity green (P4.6). Phase tag: `razelv3-rust/p4`.

---

## Track Q Phase 5 — q4 query over `@crates` + dogfood (in progress)

- `0473ac9` **P5.0 — vendor the `@rules_rust//rust/platform` + `@platforms` query slice** (§13/R6):
  the 7 triple config_settings the `@crates` graph references + `@platforms//{cpu,os}` constraint_values
  + `@platforms//:incompatible`, as `host_build` rows + `host-repos/` BUILDs (dogfood-clean: no Bazel
  `external/` tree). constraint_values captured verbatim from `bazel query --output=build`. Unit: p50.
- `769dd08` **P5.1 — `cargo_build_script` macro children as query nodes** (§11.2): the native declares
  the runner `:_bs` + `:_bs_` (rust_binary) + `:_bs-` (runfiles) with synthetic Rule edges, so `deps()`
  traverses them; `labels("deps")` stays the `:build_script_build` alias only. Query-only; analysis
  untouched. Unit: p51.
- `9d94ac1` **P5.2 — traversal reachability into the slice** (§13): `load_query_graph` closes over
  edges into `host_build`-provided packages (fixpoint), so `deps()`/`rdeps()` reach the slice NODES.
  Surfaced + fixed two query-capture gaps: `target_compatible_with` is now a rust-rule `label_attr`
  (its select conditions/default are edges), and the host-BUILD `constraint_value`/`constraint_setting`
  natives (`rules/globals.rs`) now capture QUERY nodes (not just analysis targets), so a constraint_value
  reached as a default-arm value classifies by its rule_class. Kind correctness gate: q4_platform_slice
  (config_setting + constraint_value with correct `label_kind`). lib 82/0, query goldens + rust_graph_parity green.
- `3ac8e04` **P5.3a — external `@`-pattern query ENTRY + `@crates` materialization** (§13, q4):
  `razel query 'deps(@crates//:blake3)'` runs end-to-end. `packages_for_pattern` accepts a concrete
  `@repo//pkg:target` (recursive `@repo//...` deferred — needs a materialized-tree walk);
  `load_query_graph` seeds the lock + `.razel-crates` base (the prelude extracted to
  `seed_crate_lock_and_base`, shared with the build driver) and broadens the traversal fixpoint to
  materialized `@crates__*` packages when a crate base is present (workspace-only queries unchanged).
  The apparent↔canonical duality: the loader canonicalizes `@crates` edges once the lock is seeded
  (graph is `@@rules_rust++crate+…`-keyed), so `run()` canonicalizes the expression's `@crates`
  pattern leaves up-front (`canonicalize_query_pattern` + lock-only `canonicalize_crate_repo_lock`).
  `#[ignore]` driver `crates_query` (44-node closure). Gate: lib 82/0, query 22/0 + committed query
  tests, rust_graph_parity 2/0, gates + perfgate (2.04).
- **P5.3b — q4 `@crates` query golden battery** (§13/§9, q4): `cargo xtask capture-crates-query-goldens`
  captures `bazel query --noimplicit_deps` over razel's OWN `@crates` graph (the repo ROOT) into
  `parity/corpus/rust/crate_blake3/query_goldens.txt`; the `#[ignore]` `crates_query_parity` driver
  runs the SAME exprs through `razel query` and asserts set-equality after normalizing the canonical
  `@@rules_rust++crate+…`/`@@` repo display to apparent. **4/4 exprs full parity**: alias transparency
  (`deps(@crates//:blake3, 1)` → apparent alias + canonical actual), `labels(deps,…)` (dep crates +
  the `:build_script_build` alias), `labels(target_compatible_with,…)` = `@platforms//:incompatible`,
  and the `config_setting` select-condition keys (the 7 triples). Covers spec bullets 1, 3, 4.
  **DEFERRED (Phase 6, named):** `labels(compile_data,…)` glob() SourceFile parity diverges on three
  separable non-q4-core points — (1) glob'd EXTERNAL source files keyed `//:c/blake3.c` (repo prefix
  dropped) vs bazel's `@crates__blake3-1.8.2//:c/blake3.c`; (2) razel's glob skips HIDDEN files
  (`.github/*`, `.gitignore`, `.cargo/config.toml`); (3) razel omits `REPO.bazel` + surfaces a
  `cargo_toml_env_vars` generated target. External source-file label-keying + glob hidden-file
  fidelity is its own rung. Gate: `crates_query_parity` 4/4; committed suite hermetic (both drivers
  `#[ignore]`).
- `f75b193` **P5.4a — dogfood: the full razel graph ANALYZES with no Bazel** (§9.5): analyzing
  `//crates/razel-cli:razel` over razel's OWN graph (~20 workspace crates + ~229 `@crates`) loads +
  analyzes end-to-end — **306 targets**. Three loading gaps closed: (1) the `rust_test` rule (21
  workspace BUILDs `load()` + call it — DECLARES/captures + analyzes to an empty target; the
  `rustc --test` harness is the test-verb rung); (2) `load("@crates//:defs.bzl", …)` (the
  crate_universe `aliases()`/`all_crate_deps()` macro layer) via `external_bzl_path`'s
  apparent→canonical repo resolution (same as P5.3a, for `.bzl` loads); (3) a `local_crate_mirror`
  stub (a WORKSPACE-mode repo rule defs.bzl loads but uses only in `crate_repositories()`). New
  `#[ignore]` dogfood driver: `probe_razel_cli_analyzes` GREEN. Gate: lib 82/0, rust_graph_parity
  2/0, query 22/0, gates + perfgate (2584ms).
- **P5.4b — the EXECUTION milestone: RAZEL SELF-HOSTS.** `razel build //crates/razel-cli:razel` with
  NO Bazel builds the razel binary — **400 actions, 458 outputs** (the full ~150-crate `@crates`
  closure + all ~22 workspace crates + the final link) — and the produced 42 MB Mach-O `razel help`
  RUNS. Driver `probe_razel_cli_builds_and_runs` (`#[ignore]`, WS gate). Fourteen execution gaps,
  each a real general fix (commits `5c2dc8f`→`311e48c`): wrapper wiring + `RUSTC` build-script env
  (allocative); re-run hygiene; proc-macro dep **dylib as a sandbox input** (ctor); **OUT_DIR
  absolutized** at runtime (serde_core's `include!`); `rustc_env_files` CAPTURED + routed via
  `--env-file` + the env-file target joins `deps` for build-order (serde_derive's
  `CARGO_PKG_VERSION_PATCH`); **transitive proc-macro dylibs on a consumer's `-Ldependency`** (serde
  re-exports serde_derive); `CARGO_MANIFEST_DIR` (schemafy's compile-time schema read); crate
  **`aliases`** (`errno`→`libc_errno`); `CARGO_PKG_VERSION/NAME` for workspace crates (rules_rust's
  `version` default — every compile now wraps, parity-stripped); the P5.0 host-repos slice as
  razel-loading `compile_data`; **razel-query's missing BUILD.bazel + razel-cli dep** (dogfooding
  caught a stale BUILD graph); and the build-script **OUT_DIR staged at the final link** so the
  inherited `-L`/`-l` find blake3's `libblake3_neon.a`. The Phase-6 build-script "long tail" never
  bit — the closure builds. Gate per gap: lib 82/0, rust_graph_parity 2/0, gates + perfgate.

**Phase 5 DoD — DONE (milestone 5 + q4):** query over `@crates` matches `bazel query` (4/4, P5.3b);
the razel binary self-hosts (P5.4). One loader, two readers — the §14 non-conflict contract is live.

**Phase 5 closed:** tagged **`razelv3-rust/p5`** at `82be6e0`. Per-step tiered gate green at the tag
(lib 82/0, rust_graph_parity 2/0, gates + perfgate). The full `--workspace` reconfirm is
intentionally DEFERRED with a post-Phase-6 perf review: the sweep ran ~20min (recompile-dominated or
a load regression — UNMEASURED by direction; don't run `xtask tfload` to chase it until then — see
Plan Phase 6 "Perf"). The pre-fix sweep enumerated the complete non-ignored failing set = the
regression (fixed in `82be6e0`) + the 2 known cc/java graph-parity carve-out reds (`live_cc_graph`,
`java_graph` — separate track); the test-only fix can't perturb any other suite, so the post-fix
failing set is exactly those 2 carve-outs.

## Phase 6 — Q1: external glob / source-file query fidelity (DONE)

**P6.Q1 (the first Phase-6 rung pulled forward, 2026-06-17)** — `razel query labels(compile_data,
@crates//:blake3)` is now byte-equal to `bazel query` (the deferred 5th `@crates` battery expr; driver
**5/5**, `c9108ad`). Four green steps:
- `37ce02f` **Q1.a** — `query labels()` keys a target's RELATIVE attr values (glob'd source files) at
  the target's repo+package (`@@<repo>//:c/blake3.c`), not the main repo `//:` (eval.rs: derive the
  full repo prefix via `rsplit_once(':')`; main-repo behavior unchanged; razel-query lib 23/0).
- `bf8a66a` **Q1.b** — `glob()` includes the HIDDEN files bazel lists (`.github/*`, `.gitignore`,
  `.cargo/config.toml`) for EXTERNAL crate packages; the WORKSPACE walk still prunes dotfiles so
  `.git`/`.razel-*` never leak (`walk_files` gains `include_hidden`; lib 83/0).
- `9e469d9` **Q1.c** — materialize bazel's empty `REPO.bazel` marker (`ensure_repo_marker`, called by
  the loader for every external repo — a fresh fetch AND a pre-Q1.c materialization both converge, so
  stale trees self-heal in place; lib 84/0).
- `c9108ad` **Q1.d** — capstone: golden captured (80 labels), battery 5/5; the driver strips build
  PRODUCTS first (`cargo_toml_env_vars`/`_bs*`/rlib were dirty-tree leakage, not a glob gap).

Gate per step: razel-loading/-query lib green, rust_graph_parity 2/0, blake3 B3 analysis parity green,
xtask gates + perfgate OK (scaling 1.97, budget OK — no perf regression). **Follow-on → Phase 6
Build:** output-tree separation (razel build-in-place writes products into the source repos; `query`
after `build` over-lists them — the driver strips products as a workaround; the fix is a real output
tree).
