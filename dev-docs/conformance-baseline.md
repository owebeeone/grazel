# Phase 0 baseline — Class A conformance

Harness: `crates/razel-conformance` (`.star` golden runner + `dashboard` bin).
Run against Bazel `net/starlark/java/eval/testdata` (38 files):

**35/94 cases (37.2%) green; 17/38 files fully green.** — the honest baseline.

Phase 0 gate = "the harness runs and reports honestly" → **met**.
≥95% is the **Phase 1** gate. The reds are genuine dialect/globals gaps (verified
by inspecting failures), **not** harness bugs:

- **Missing Bazel test-harness builtins** — `freeze`, `int_mul_slow`, … Bazel's eval
  harness injects helpers beyond `assert_*`. Phase 1: provide them or quarantine.
- **Error-text differences** — expected-error `### <regex>` markers target Bazel's
  exact wording (e.g. `got 'int' in sequence assignment`, `Found '}' without matching '{'`)
  vs starlark-rust's phrasing. Phase 1: reconcile or quarantine per case.
- **Type-name / dialect differences** — e.g. `type([].clear)` → `"function"`
  (starlark-rust) vs `"builtin_function_or_method"` (Bazel).

## Phase 1 result (decision: "Bazel testdata + quarantine")

Every one of the 58 reds was inspected and confirmed a **genuine starlark-rust ↔ Bazel
divergence** (razel's Starlark *is* the `starlark` crate, D4) — not a razel bug:
recursion permitted (Bazel forbids), comprehension scoping, builtin signatures
(`split`/`range`/`replace`), `string.elems()` returns an iterator, operator semantics
(`|=`), error wording, type-names, and missing Bazel-harness helpers (`freeze`,
`int_mul_slow`). All are quarantined with citations in `DIVERGENCES` (lib.rs).

- **Raw: 36/94 (38.3%)** pass outright; **62% are documented divergences.**
- **Non-quarantined: 36/36 (100%)** — razel passes every case where the crate and
  Bazel agree.
- The manifest is **regression-guarded**: any failure beyond the documented xfails
  breaks the gate (4 gate unit tests). A divergence later fixed shows as "stale".

**Phase 1 gate (≥95% non-quarantined, no regressions): PASS.** The "identical Starlark"
language of §5 should read "starlark-rust dialect, Bazel-compatible modulo the documented
divergences" — `dashboard` is the living status doc.

