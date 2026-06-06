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

Phase 1 punch-list = the above, file by file, to ≥95% (quarantining genuinely-divergent
cases with citations).
