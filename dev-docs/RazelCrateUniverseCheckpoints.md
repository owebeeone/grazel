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

## PAUSE before Phase 3 (the blake3 build) — a real roll-build guardrail

Phase 3 (load surface + `cargo_build_script` + the rustc wrapper + build-script execution + the
build-script edge + analysis/execution parity vs **live `bazel aquery`/`bazel build`**) is a
12-step, tightly-COUPLED integration — the method's "phase too large/too coupled to complete safely
as one checkpoint" applies, and it needs live-bazel parity capture. After two full phases tagged in
one session, this is the honest checkpoint to land before that integration.
