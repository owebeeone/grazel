# RazelCodingRules — crisp rules (distilled from RazelV2ABCCodeReview-48)

Short, enforceable rules. Each is a recurring failure mode the A0..B4 review caught; the goal is to
*prevent* the class, not re-find instances. Companion to `AGENTS.md` (TDD/workflow) + `CLAUDE.md`.
Format: **RULE** — why — `[findings]`.

## State & lifecycle
1. **No process-global *request/analysis* state — compile-time-constant data is fine.** The sin is **not
   mutability** (these globals end up immutable); it is a value **owned by no `Session`** — created at an
   arbitrary first call, capturing the environment (`PATH`/env/clock/fs), shared across every analysis,
   un-injectable and un-resettable in tests.
   - *Allowed:* `const` and compile-time `static` of **pure constant data** (lookup tables, the gate's
     `BANNED` list) — declarative data, no lifecycle, no owner question. Also fine: a `LazyLock`/`OnceLock`
     wrapping a **pure, deterministic** value (a compiled regex, a parsed constant) — no env, no
     per-request meaning.
   - *Banned for env-derived / per-analysis state:* `static mut`, `thread_local!`, and runtime-init cells
     (`OnceLock`/`OnceCell`/`LazyLock`/`lazy_static!`) holding such state. Thread the value through
     `Session`/`GlobalFlags`.
   - *The test (who creates it, what it holds):* could two analyses in one process legitimately need
     different values, or does init read the environment? **Yes → `Session`, not a global.** (F13: the
     resolved host cc is env-derived *and* per-toolchain — it belongs on the Session.)
   - An *immutable* `thread_local!` is a pointless anti-pattern (a `const`/`static` does the same without
     thread affinity); razel should need no thread-locals at all.
   - *Gate:* a substring check can't tell pure-const from env-capturing, so it **flags** the runtime-init
     cell types and each use needs a one-line allowlist justification (pure + process-lifetime + not
     request-scoped). "Fully enforced" = flagged + the allowlist reviewed, not silence. `[F13, F19]`
2. **Inject the environment.** Anything reading `PATH`/env/clock/fs takes it as a parameter (a pure inner
   fn) so it's testable without the host and resettable per test. `[F13, F19]`

## Tested == run
3. **The code under test must be the code that runs.** Don't unit-test path A while the binary executes
   path B. If the executed path (e.g. Native cc) lacks parity coverage, that gap is a finding, not
   "done." `[F18]`
4. **No stranded infrastructure.** Don't land a mechanism with zero live callers (a fold, a schema
   engine, a derive helper). Either wire it into the live path, or *explicitly* park it in
   `RazelGaps.md`/the ledger as a parallel spine to reconcile — never silently dead. `[F3, F22]`

## One source of truth
5. **One owner per concept.** No algorithm duplicated N× (the 5× fold) and no formula re-derived in
   parallel (the path model in 3 places). Extract one; if you must duplicate temporarily, add a
   cross-check test + a consolidation ticket. `[F22, F24, F25, F23, F30]`
6. **No language name in the engine core.** Generic machinery must not branch on / hardcode per-language
   providers; project by the consuming rule's declared schema. `[F29]`

## Truth in claims
7. **Docstrings/labels must match the code.** No "deduped/ordered" on a fold whose call site concats; no
   "commutative" on a non-commutative arm; no "real API" on a shim its own reader can't round-trip; no
   "fully enforced" the gate doesn't enforce. If you can't make the claim true now, weaken the claim.
   `[F1, F2, F4, F34, F13]`
8. **A shim must round-trip exactly what its reader reads.** A provider/config shim that emits fewer
   fields than the extractor consumes is dead-extraction code that breaks on the first real input. Keep
   shim ⟷ extractor in lockstep, with a round-trip test. `[F34]`

## Folds & propagation
9. **Transitive folds are rooted at the consumer, and dedup is the engine's job.** One traversal from the
   consuming target over its closure — never per-dep fold + caller-side `+`/concat (that defeats
   cross-sibling dedup; diamonds duplicate). `[F1]`
10. **Honor declared order, or error.** Don't silently discard a `depset(order=…)` or claim "preorder"
    unverified. `[F36]`

## Tests & fidelity
11. **Don't invent fidelity the oracle lacks.** Never emit a flag/structure absent from the golden just
    to anchor a test (the fake `--runtime_classpath`). Test the real data model, not a fabricated
    rendering. `[F15]`
12. **A guarantee needs a corpus case that fails when it breaks.** Dedup/order needs a *diamond*, not a
    linear chain; subtree-prune needs a *nested* case, not a leaf. Linear-only corpora prove nothing
    about transitivity. `[F1, F14]`
13. **Assert exact where exactness is meaningful.** Prefer `==` over `.contains()`/positional slicing
    when the full argv/set is the contract. Captured goldens must be wired into a `diff` test, not just
    `.contains()` strings. `[F16, F17, F9]`
14. **Placeholders normalize — never pre-normalized.** Feed real segments the normalizer rewrites (the
    `_CFG` pattern); never bake the normalizer's *output* (`<sdk>`, `<repo>`) in as input — that makes
    the parity check vacuous and the graph un-runnable. `[F32]`

## Parity oracle
15. **A set-diff is a bijection.** Detect duplicate keys (`assert len(map)==len(items)`), include argv in
    identity where it matters, and fail loud on malformed/ambiguous input — never last-wins-silently.
    `[F6, F7]`

## Schema & API surface
16. **Fail closed on attrs.** `getattr(ctx.attr, x, default)` booleans fail open — a typo'd `neverlink`
    silently links. Prefer a declared schema; flag unknown attrs; don't ship `attrs = {}` as if it were
    the schema. `[F31]`

## Rust idioms
17. **Group same-typed args into a struct.** ≥4 adjacent same-typed params (the 8-`&str` fns) is a
    transposition footgun; mirror `CompileInputs`/`ArchiveInputs`. `[F26]`
18. **One `#[allow]` at the smallest scope; prefer `..Default::default()`; use typed `Label` not string
    ops.** Redundant allows hide the load-bearing one; hand-rolled label parsing drifts. `[F27, F28, F25]`

## Discipline (carried from AGENTS.md, reinforced)
19. **Roll-build green-gated; commits ≤3 lines; commit only when asked.** A green test that passes by
    cutting a corner is a regression in disguise — verify the test proves what it claims before tagging.
