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

**Remaining for `razelv3-rust/p3`:** P3.7 (build-script flags-file parser) → P3.8
(`CargoBuildScriptRun` action) → P3.9 (rustc wrapper
binary; **P3.5b** env precedence + `--env-file=` consumption rides here) → P3.10 (wire the
build-script edge) → P3.11/P3.12 (analysis + execution parity vs live `bazel aquery`/`bazel build`).
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
