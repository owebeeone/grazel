# 0012 — from RR — `BUILD.razel` removed (one wstest fixture is yours to flip)

Status: open

## Decision (Gianni, 2026-06-18)
`BUILD.razel` is removed. The argument: one `BUILD.razel` pollutes the **target space** and,
via the old boundary guard, forces its whole module razel-native / bazel-invisible — all-or-
nothing fragmentation that defeats the bazel-compliance goal. Spike §3c design E is reverted to
**C + D**. `MODULE.razel` and `--strict_bazel` are KEPT but are now dormant hooks (not on the
live path); only the package-level `.razel` grammar is gone.

## What I did (RR lane — loader + razel-* + the corpus data files)
- `razel-loading`: `resolve_build_file` no longer recognizes `BUILD.razel` (just `BUILD.bazel`
  over `BUILD`); `e_mode_guard` + `root_is_dual` deleted; the e_mode `pkg.rs` guard block gone;
  `e_mode_guard` export dropped. `find_workspace_root` (MODULE.razel walk) kept, dormant.
- tests: `tests/e_mode.rs` → `tests/workspace.rs` (kept the BUILD-precedence + MODULE.razel
  boundary-walk tests; dropped the BUILD.razel/XOR/guard ones).
- `razel-cli` + `razel-build`: flipped fixtures and user-facing strings off `BUILD.razel`.
- corpus: `clients/gryth-examples/{01-hello-http,02-js-tests,03-ts-project}/BUILD.razel` →
  `BUILD.bazel` (content was already `load("@aspect_rules_js…")`, so a pure rename). The empty
  `MODULE.razel` markers are LEFT in place (per Gianni — MODULE.razel stays).
- doc: spike §3c amended (E reverted → C+D).

## What's yours (one line — it's in `grazel-cli-lib`, your crate)
`crates/grazel-cli-lib/src/wstest.rs:1129` writes `ws.join("t/BUILD.razel")`. Flip it to
`BUILD.bazel` (the content is already aspect `load()` grammar). Once `BUILD.razel` recognition
is gone, that fixture writes a file the loader won't see, so the wstest step fails until flipped.

## Sequencing
This is on my working tree, **not pushed** — I'm holding the push so your wstest flip and my
loader removal can land together (no red intermediate on `razelv3`). Flip wstest, tell me, and
I'll push both. Or if you'd rather I push first, say so and flip right after.
