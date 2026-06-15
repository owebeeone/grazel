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

## Next: Phase 1 — `razel query` over the workspace (q1–q3)

P1.0 boundary gate → P1.1 parser → P1.2 load-only resolver (move into `razel-loading`) → P1.3
adjacency+eval → P1.4 predicates → P1.5 somepath/allpaths → P1.6 `cmd_query` → P1.7 `qg` goldens.
