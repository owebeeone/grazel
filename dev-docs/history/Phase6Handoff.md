# Phase 6 — session handoff (2026-06-18)

Handoff because the previous session's conversation log grew too large for the viewer. **The work is
not blocked** — everything is committed green; this is a clean continuation point. A fresh session
should keep rolling Phase 6 autonomously (the user's standing instruction across this work was
literally "go phase 6" → "continue" → "keep rolling" — pick the next rung, gate it, commit it, don't
ask permission per step).

## Current state (verify with `git`)

- Repo: `/Users/owebeeone/limbo/glial-dev/razel`. Branch **`razelv3`**, tag prefix `razelv3-rust/`.
- HEAD: **`faf1ed8`**, working tree **clean**. Tag **`razelv3-rust/p5`** marks the start of Phase 6.
- **Phase 5: DONE + tagged** (`razelv3-rust/p5` at `82be6e0`). razel self-hosts; `@crates` query 4/4
  at the tag. See `RazelCrateUniverseCheckpoints.md` "Phase 5".
- **Phase 6: IN PROGRESS.** Done so far (15 commits since the tag; full per-rung detail with commit
  hashes is in `RazelCrateUniverseCheckpoints.md` — read it, don't re-derive):
  - **Q1** (a–d) — external glob / source-file query fidelity. `labels(compile_data, @crates//:blake3)`
    is byte-identical to `bazel query`; the `@crates` battery is **5/5**.
  - **B** (1–2) — output-tree separation. `out_path`/`out_dir` now honor `bin_tree_layout`; razel
    self-hosts into a clean `razel-out` tree (source pristine). Retired the Q1.d product-clean band-aid.
  - **Q2** `siblings(x)`, **Q3** `same_pkg_direct_rdeps(x)`, **Q4** `--output=package`, **Q5** `tests(x)`
    (with `test_suite` expansion) — query surface expansion.

Query surface now: verbs `deps`/`rdeps`/`kind`/`filter`/`attr`/`labels`/`somepath`/`allpaths`/
`siblings`/`same_pkg_direct_rdeps`/`tests`; `--output=` `label`/`label_kind`/`package`.

## How to work here (the roll-build discipline — obey it)

- **One green step per commit. NO `Co-Authored-By` trailer** (overrides the default tool instruction).
  Commit each rung as you complete it. (The user authorized continuous per-rung commits this session.)
- **`bazel` parity LEADS.** Gate every behavior change against real `bazel query`/`aquery` (the
  goldens), not just razel's own shape. bazel 9.1.1 is on PATH; `.razel-crates` is already materialized
  at the repo root (so the `#[ignore]` `@crates` drivers run without re-fetch).
- **Always prefix shell commands with `cd /Users/owebeeone/limbo/glial-dev/razel &&`** — the cwd resets
  to the parent `glial-dev` between some calls and `cargo` then fails to find `Cargo.toml`.
- **`cmd | tail`/`| grep` masks the exit code** (you get tail/grep's 0) AND truncates. Redirect to a
  file, capture `$?` on its own statement, then grep the file.
- **Per-step gate, tiered to the changed crate:**
  - razel-**query** change → `cargo test -p razel-query --lib` + `cargo xtask gates`.
    (Query is downstream of loading; the loading carve-out can't be affected — but `xtask gates`
    confirms the "razel-query reads-only, no analysis dep" boundary still holds.)
  - razel-**loading** change → `cargo test -p razel-loading --lib` + the carve-out sentinel
    `cargo test -p razel-loading --test rust_graph_parity` (**2/0**) + `cargo xtask gates` +
    `cargo xtask perfgate`. For `@crates`/build changes also run the `#[ignore]` drivers that touch it
    (`blake3_closure::blake3_analysis_matches_the_bazel_golden`, `dogfood_selfhost`).
  - The full `cargo test --workspace --no-fail-fast` (~16–20 min) is a **phase-tag-only** gate.
- **Known carve-out reds** (NOT your regressions — a separate cc/java parity track):
  `live_cc_graph_matches_the_golden_in_adopt_bazel_mode`, `java_graph_matches_the_golden_structurally`.
  `rust_graph_parity` is the green sentinel.
- **perfgate is thermally noisy.** A budget trip right after a heavy compile+test batch is usually
  variance — re-run it clean 1–2× before believing a regression (a real one shows in the scaling
  ratio, cap 2.3, not just absolute ms). Baseline ~2560ms.
- **Style** (from `CLAUDE.md`): verify before asserting; stay in scope; concise; don't manufacture
  work; concede cleanly when wrong. Declarative-first; boilerplate is a smell.

## Recipe — adding a query verb (the warm path; Q2/Q3/Q5 followed it)

1. `crates/razel-query/src/parse.rs`: add the `Expr::Foo(...)` variant; in the dispatch `match` (the
   `"word" =>` arms ~L200) move the verb out of the `"…" => Err("not supported … deferred §12")` list
   into a real parser (`parse_unary` for `f(x)`; `parse_filter2`/`parse_path2`/`parse_attr` exist for
   other shapes).
2. `crates/razel-query/src/run.rs`: add the variant to **both** tree-walkers (`collect_patterns` +
   `canonicalize_patterns`) or the match is non-exhaustive → compile error.
3. `crates/razel-query/src/eval.rs`: add the `Expr::Foo` arm in `Eval::go`. Reuse helpers:
   `pkg_prefix(label)` (repo+package prefix), `canonical_label(raw, prefix)`, `attr_labels(raw, out)`.
   Graph access: `self.graph.match_pattern("//pkg:*")` (rules + source/generated + BUILD),
   `.deps`/`.rdeps`/`.target(label)` (→ `LoadedTarget` with `.rule_class`/`.attrs`/`.package`),
   `.same_pkg_direct_rdeps`. Non-trivial graph ops belong as `QueryGraph` methods (see graph.rs).
4. **Unit-gate** in the eval/output test module (synthetic `QueryGraph::new(BTreeMap)`; `target()` helper).
5. **Parity-gate** against bazel (do this whenever the corpus supports it):
   - LOCAL (preferred): add the expr (using the `{P}` package template) to `QUERY_BATTERY` in
     `xtask/src/main.rs`, run `cargo xtask capture-query-goldens` (bazel, over
     `parity/corpus/rust/transitive`), then `cargo test -p razel-query --test live_query_parity`
     (hermetic, **non-`#[ignore]`** — it's part of the normal gate; uses `cases >= N` so no count bump).
   - `@crates`: `CRATES_QUERY_BATTERY` + `cargo xtask capture-crates-query-goldens` +
     `crates_query_parity` (`#[ignore]`; normalizes the canonical `@@rules_rust++crate+…` → apparent).
6. Watch for the pre-existing `parse::tests::errors_are_human_readable` test — it asserts some verb is
   still "deferred"; if you enable that verb, swap the assertion to one that's still deferred.

## What's next — the fork (cheap rungs are exhausted; all of these need real infra)

Recommendation if continuing the **query** track: **extend the parity harness to gate Q4/Q5 against
bazel** (capture `--output=package` over the local battery; add a small test-bearing corpus — needs
checking whether the loader registers `test_suite` — to gate `tests()`). Lowest-risk; consolidates the
unit-only gates onto real parity.

Otherwise, the open Phase 6 backlog (see `RazelCrateUniversePlan.md` "Phase 6"):
- **Query verbs needing infra:** `buildfiles`/`loadfiles`/`rbuildfiles` (load-phase `.bzl` tracking —
  not in the query graph today), `visible(x, y)` (a visibility model).
- **More `--output=` renderers:** `build`/`proto`/`xml`/`graph` — each its own renderer + the golden
  harness extended to capture that output mode.
- **Build track (meatiest):** `rerun-if` narrowing (dynamic-dep model), cross-compilation (exec/target
  split + transitions), the build-script long tail (sys-crates/system-libs/link-ordering — never bit
  razel's own closure, real for heavier graphs), richer `DEP_*` beyond P4.5.
- **Lock:** `cargo-bazel splice+generate` offline fallback — only if the lock format destabilizes.

## Deferred / do-NOT-touch-yet

- **Perf review is deferred to AFTER Phase 6** (the user's explicit call). The full `--workspace`
  sweep ran ~20 min vs a ~16 min baseline — UNMEASURED whether that's a real load/eval regression or
  just recompilation. **Do NOT run `cargo xtask tfload`** (loads the whole TF corpus) to chase it until
  Phase 6 closes. When pulled forward: re-baseline with `tfload` + the `PL` scaling gate, then
  stop-and-correct per §Performance. (See `RazelCrateUniversePlan.md` Phase 6 "Perf".)
- **Named follow-ons** already recorded in the plan/checkpoints: implicit-all-tests `test_suite`
  expansion (a suite with no `tests` attr); a bazel-battery gate for `--output=package` and `tests()`.

## Authoritative docs (read these first in a fresh session)

- `dev-docs/RazelCrateUniverseCheckpoints.md` — the per-rung record (Phase 5 + every P6 rung with
  commit hashes, gates, and the reasoning). **The primary state-of-the-work doc.**
- `dev-docs/RazelCrateUniversePlan.md` — the plan; "Phase 6" is the backlog (each item its own rung).
- `CLAUDE.md` (working memory / how Gianni works) + `AGENTS.md` (TDD + workflow rules).
- `dev-docs/DecisionLog.md` — open design questions.
- `git log --oneline razelv3-rust/p5..HEAD` — this session's commits, each message self-contained.
