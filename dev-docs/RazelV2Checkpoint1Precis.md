# RazelV2 Checkpoint 1 — Précis

*2026-06-10, at `razelV2-RSB/D4.4` (133 green tags). Companion to `RazelStarlarkBoundaryPlan.md`
(the phase plan) and `RazelHookSeam.md` (the C3 seam). This doc answers: how far is razel from
building a complex real codebase (TensorFlow as the yardstick), and how do we drive there.*

## 1. Where we are (grounded inventory)

**Code:** ~19.8k LOC Rust across 17 crates (+1.6k test LOC), 53 green test binaries, three
enforced gates (AD2 no-ambient-state, razel-dds dependency boundary, C3c no-language-in-core).

**Proven (phases A–C, all green-tagged):**
- A generic analysis engine where **languages are data**: one provider value algebra (razel-dds
  `FieldValue`/`FieldKind`), one registry-driven transitive fold shared by both dep-resolution
  paths, one generic provider constructor (`razel_build.info`), a toolchain registration seam.
  Adding a language is a registration, and a gate enforces it.
- Five rulesets ride that engine (cc, java, py, rust, sh — razel's own `.bzl` + native rules),
  with **build-and-run** parity on toy corpora (hello-world-scale packages; the parity corpus is
  3 BUILD files). Bazel-parity goldens exist for the AdoptBazel cc path and the java spike.
- Per-analysis `Session` (multi-instance capable), workspace mode with cross-package deps,
  `.bzl` loading with freeze/load.

**Phase D so far:** the attrs schema is real (defaults/`mandatory`/schema-driven label
resolution — D1a–c); **real upstream Starlark loads from `third-party/`** (D4.1–4.4): real-file
loads override razel's shims, real `bazel_skylib` `paths.bzl` + `common_settings.bzl` load and
evaluate, `provider()` exists, and real `rules_rust/rust/private/rust.bzl` (1781 lines) now
**compiles end-to-end** — every free variable resolves — blocked only on `@rules_cc` not being
vendored (`cc_info.bzl` load, rust.bzl:19).

**The honest capability statement:** razel today is a *validated architecture spike*. It
analyzes and executes toy packages over its own rulesets, and has just crossed the threshold of
*loading* (not yet instantiating) real upstream rules. Nothing real has been *built* through a
real upstream ruleset yet.

## 2. The distance to TensorFlow (grounded)

TF's Bazel surface (`third-party/tensorflow`): **1,324 BUILD files, 359 `.bzl` files (53k LOC of
Starlark), 51 external archives** (`tf_http_archive`), WORKSPACE+MODULE hybrid, **53
`repository_rule`s** (the cuda/python/etc. configure system — arbitrary discovery logic run at
fetch time), 220 `select()`s, 129 genrules, 10 aspects, 3 transitions, plus Bazel's *native*
cc/py/proto rules and the full C++ feature-configured toolchain underneath all of it.

Gap by subsystem (✅ have · 🟡 partial/stub · ❌ not started):

| Subsystem | State | Notes |
|---|---|---|
| Loading: `load()`/freeze, real external repos | ✅/🟡 | Real files load; 42-file rules_rust graph not yet walked; no bzlmod/WORKSPACE eval |
| `rule()`/attrs schema | 🟡 | defaults/mandatory/label-resolution real; types, `cfg=`, `providers=`, implicit attrs absorbed-not-honored |
| `ctx` surface | 🟡 | razel has 6 members; rules_rust's 3 core files alone use **16** (`bin_dir`, `runfiles`, `expand_location`, `toolchains`, `configuration`, …) |
| Providers: define/construct | ✅ | D4.2; capture-from-return + `dep[P]` indexing ❌ |
| Configuration: `select()`/`config_setting`/platforms | ❌ | `select()` takes first branch; no config resolution at all |
| Transitions / aspects / exec groups | ❌ | stubs absorb, nothing applied |
| Toolchain resolution (`rule(toolchains=)` → `ctx.toolchains`) | ❌ | one hardcoded cc config behind the registry seam |
| Repository rules / fetch / configure | ❌ | vendored-only; TF's 53 repo rules are a whole subsystem |
| Native rule fidelity (cc/py/proto at Bazel semantics) | 🟡 | toy cc/py; no proto, no genrule, no runfiles/data |
| Execution at scale (10⁵–10⁶ actions, remote cache) | ❌ | toy executor proven on hello-worlds |

**Order-of-magnitude calibration:** the remaining semantic surface is roughly **10× the code
that exists today** — and that is the *expected* shape, not a surprise: the assessed risk was
always integration surface, never algorithmic novelty. TF is the far end of a ladder, not the
next step. Loading-phase semantics are maybe 5–10% done; analysis-phase fidelity less;
configuration/toolchain/repo-rule subsystems are at zero. No calendar estimate is honest at this
distance; the ladder below makes progress *measurable* instead.

## 3. The corpus ladder (how we proceed)

Each rung is a **build-and-run golden** (load-only green is a trap), and names its prerequisite
subsystems. Don't skip rungs; every rung retires a shim or stub.

- **L2 — real `rust_library` hello-world** *(current frontier)*. Needs: `rules_cc` vendored
  (resource), the 42-file rules_rust load graph walked, instantiation semantics (provider
  capture-from-return, `dep[P]`, the missing `ctx` members rust.bzl's impl actually touches, a
  rust `ctx.toolchains` stand-in). Exit: A0's rust corpus green via REAL rules_rust; **both**
  razel rust backends deleted (the D4 exit).
- **L3 — real toolchain resolution.** `rule(toolchains=…)` + registered toolchains →
  `ctx.toolchains[type]`; the generic `Toolchain` type the C3 doc deferred becomes honest here.
- **L4 — a real cc codebase (abseil-cpp).** Needs: `select()`/`config_setting` resolution against
  a real configuration, platforms, native-cc fidelity, runfiles. This rung is where the biggest
  not-started subsystem (configuration) lands.
- **L5 — protobuf.** TF's most load-bearing dep: proto rules, genrule fidelity, generated-code
  dep chains.
- **L6 — a TF subtarget** (e.g. `//tensorflow/core/platform` or tsl). Needs: repository-rule
  subsystem (or vendored pre-configured repos as a cheat rung L6a), the `tf_*` macro layer,
  aspects/transitions as actually exercised.
- **L7 — TF at large.** Adds execution scale (action-graph size, caching, scheduling) on top of
  full analysis fidelity.

## 4. Near-term (the next rolls, in order)

1. **Vendor `rules_cc`** (resource gate — Gianni). Unblocks rust.bzl:19.
2. Walk the rust.bzl load graph (probe loop: each missing builtin/member, test-first, tagged).
3. **`xtask probe`** — automate the loop I've run by hand: run ladder corpora, emit
   first-failure per corpus, classified (missing-global / missing-member / semantic / resource).
   This is the ticket generator for §5.
4. Instantiate `rust_library`: provider capture-from-return, `dep[P]`, `ctx` member expansion.
5. L2 exit: rust corpus green on real rules_rust; delete both rust shims; update plan docs.

## 5. Multi-agent plan (drive-to-completion machinery)

The D4 probe loop is mechanical and parallelizable: *probe → first error → classify → failing
test → smallest fix → workspace green + gates → commit + tag*. Automate it with one supervisor
and N cheap implementers.

**Roles**
- **Supervisor (1, frontier model).** Owns the ladder + a `State.md`; runs `xtask probe`;
  triages failures into *tickets* (one gap each, with repro, files, and a stub-vs-semantic
  ruling); reviews every implementer diff against AGENTS.md/TDD + the gates; serializes
  commits+tags (roll-build preserved); maintains the **stub debt register**; escalates to
  Gianni. Never implements.
- **Implementers (2–4, cheaper model, isolated worktrees).** One ticket each: write the failing
  test first, smallest fix, full workspace + gates green, hand back a diff. Narrow context:
  the ticket, the contract docs (`RazelHookSeam.md`, this précis), and the 2–3 files involved —
  not the whole history. Parallel only across independent gaps (distinct builtins/members);
  anything touching the engine core or a registry is serialized.
- **Verifier (optional, cheapest model).** Re-runs `cargo test --workspace` + `xtask gates` on
  each handoff before supervisor review; bounces fast on red.

**Policy (the guardrails that make cheap agents safe)**
- Tests + the three gates are the objective function. **No agent edits a test, gate, or
  allowlist without supervisor sign-off; the supervisor doesn't weaken a gate without Gianni.**
- Stub-vs-semantic is a *supervisor* ruling, recorded per ticket: stubs are allowed to keep the
  loop moving but every stub lands in the debt register with the rung that must retire it
  (absorbed kwargs, first-branch `select`, namespace stubs are already debt).
- Every green step commits + tags (`razelV2-RSB/...`) — the existing roll-build discipline is
  the recovery mechanism when an agent goes sideways.
- Escalate to Gianni: design seams (new registries, value-algebra changes), gate changes,
  vendoring/resources, any rung-exit declaration.
- **Do not automate:** the C3-style seam designs, doc-of-record updates, ladder re-ordering.

**Cadence.** Rounds of ~5–10 tickets; each round ends with the supervisor re-running the full
ladder probe and appending a delta to this précis (which rung moved, debt added/retired). That
delta — not ticket count — is the progress metric.

## 6. Risks

- **Stub debt becomes load-bearing.** `rule()` absorbs `cfg`/`toolchains`/`providers` silently;
  a corpus can go green-at-load while semantically wrong. Mitigation: debt register + rungs are
  build-and-run goldens + (cheap win) an instantiation-time counter of stub hits surfaced in
  probe output.
- **Shim/real divergence.** Until a rung deletes a shim, both live; real-file-first only applies
  when `external_base` is set. Mitigation: each rung's exit *deletes* the corresponding shim.
- **Local maximum on loading.** Walking load graphs is satisfying and endless; instantiation
  (L2 exit) is the only thing that proves anything. Keep probes pointed at build-and-run.
- **Repo-rule subsystem (L6) is qualitatively different** (running configure logic, hermeticity)
  — treat L6a (pre-configured vendored repos) as the honest first cut.
- **Agent-cost blowout.** Implementers must run on narrow context; the supervisor batches; the
  probe harness, not an agent, does the searching.
