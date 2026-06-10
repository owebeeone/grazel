# RazelV3Plan — the plan of record

Status: **ACTIVE** (2026-06-10). Supersedes `history/RazelStarlarkBoundaryPlan.md` (V2) and
`history/RazelV2FinalArchProposalPlan.md`. Born from `RazelV2Checkpoint1Precis.md` (the grounded
state + distance-to-TF calibration); the *architecture* of record is unchanged
(`RazelV2FinalArchProposal.md` + `RazelV2Contracts.md`) — V3 re-plans the *route*, it does not
re-architect the destination. V2 diverged from the build (Phase D executed out of its written
order via the probe loop); rather than patch it, this restates the plan with the checkpoint's
corrections.

**Primary reader: an agent.** This doc is the supervisor's contract (§5); humans steer via the
escalation list. Normative words: MUST/NEVER are gate-enforced or supervisor-enforced; SHOULD is
default-unless-ticket-says-otherwise.

## §0 Invariants (carried from V2, unchanged, non-negotiable)

1. **Roll-build.** Every change lands green (`cargo test --workspace` + `cargo run -q -p xtask
   -- gates`), committed, tagged. V3 tags: `razelV3/<step>`. Tags are the recovery mechanism.
2. **TDD** per `AGENTS.md`: failing test first; the test states the contract.
3. **The gates are the objective function.** AD2 no-ambient-state · razel-dds boundary ·
   C3c no-language-in-core. No agent edits a test, gate, or allowlist except where a ticket
   explicitly scopes it; gate *weakening* requires Gianni.
4. **Gate ratchet (new):** the C3c per-language allowlist may only shrink. A new language
   appearing as Rust in `crates/` MUST fail the gate. Languages are `.bzl` + registrations.
5. Commit messages ≤3 lines, terse, no Co-Authored-By. Code style per `RazelCodingRules.md`.

## §1 State at adoption

See `RazelV2Checkpoint1Precis.md` §1–2 (do not restate; it is the snapshot). Summary: generic
engine proven at toy scale (languages as registrations, one provider algebra, three gates, 53
green test bins at `razelV2-RSB/D4.4`); real upstream Starlark *loads* (skylib evaluates;
rules_rust's `rust.bzl` compiles, blocked on `rules_cc` vendoring); nothing real *built* through
a real upstream ruleset yet; remaining semantic surface ≈10× current code. Five architecture
debts identified (précis §7); two are foundations (E0 below).

## §2 E0 — foundations first (serialized; precedes all ticket rounds)

The two debts that everything else would otherwise be built on twice. Engine-core rework:
**supervisor-grade, never farmed to ticket workers.**

- **E0a** — failing test: a BUILD with a forward reference (`filegroup(name="a", srcs=[":b"])`
  before `:b` is declared). Fails today by construction (`dialect.rs` "not analyzed yet").
- **E0b** — loading/analysis phase split, `rule()` path: BUILD eval *records* declarations
  (rule object + raw attr values); analysis is a separate demand-driven pass over the recorded
  package graph, dependency-ordered, cycle-detected. Parity suite stays green throughout.
- **E0c** — native rules (cc/py/sh/java shims) onto the same record→analyze shape; E0a's test
  goes green. New declaration structures are **born typed** (`razel-core::Label`, not `String`)
  — no wholesale migration, but no new stringly code.
- **E0d** — the DDS becomes the store: `Session` owns one `Dds`, facts asserted at record/
  analyze time; the per-dep `to_dds` rebuild (`deps.rs`, `dialect.rs`) is deleted and analysis
  cost drops from O(n²) to O(n). Collapses the `AnalyzedTarget.providers`/DDS duality.
- **Exit:** parity + full suite green; forward references work; no `to_dds` call in a dep path;
  tag `razelV3/E0-exit`.

**E0 DONE** (`razelV3/E0b`/`E0c`/`E0d`, 2026-06-10): all exit criteria green — forward refs
resolve (Starlark + native, cycle-guarded), both dep paths fold over the Session-owned live
store, 54 test bins + the three gates pass. **One honest deviation:** the new declaration
structures kept `String` labels (born-typed `Label` was not done) — debt item 4 in the précis §7
stands unchanged; retire it with the L4-scale work at the latest.

## §3 The corpus ladder (the route to TF-class)

Inherited from précis §3, normative here. Every rung exit is a **build-and-run golden**
(load-only green is explicitly not a rung), deletes at least one shim or stub, and is declared
only by the supervisor with Gianni's ack.

| Rung | Golden | Unlocks / lands | Deletes |
|---|---|---|---|
| **L2** | real `rules_rust` `rust_library` hello-world builds+runs | provider capture-from-return, `dep[P]`, `ctx` member expansion, rules_rust load graph | both rust backends (`rust_rules.rs` + template) |
| **L3** | real toolchain resolution (`rule(toolchains=)` → `ctx.toolchains[type]`) | generic `Toolchain` type; **registration-as-data** (provider schemas + toolchains declarable from Starlark — retires `registry.rs`/`toolchains.rs` rows into `.bzl`) | hardcoded cc resolver row |
| **L4** | abseil-cpp builds | `select()`/`config_setting` resolution, platforms, native-cc fidelity, runfiles | first-branch `select` stub |
| **L5** | protobuf builds | proto rules, genrule fidelity, generated-code dep chains | — |
| **L6** | a TF subtarget (tsl / `//tensorflow/core/platform`) | `tf_*` macro layer; aspects/transitions as exercised; **L6a first**: vendored pre-configured repos in lieu of the repo-rule subsystem | namespace stubs as they become real |
| **L7** | TF at large | repository rules proper; execution at scale (action-graph size, caching, scheduling) | — |

Resource gates (Gianni): vendoring (`rules_cc` — currently blocking L2; later protobuf, abseil).

## §4 Language tracks — `.bzl`-only (the V3 correction)

There will be **no `rules_go.rs`, no `rules_js.rs` — ever** (§0.4 ratchet). A language track is:

1. **Shim rung:** `rules_shims_<lang>.bzl` over the generic API (`rule()` + `razel_build.*`) —
   the `java_defs.bzl` pattern; registrations are its only Rust footprint, and after L3's
   registration-as-data, not even that.
2. **Real rung:** the real upstream ruleset loads and builds (the D4/run-it path); the shim dies.
3. Build-and-run golden per rung; engine gaps found by a track become **core tickets** (§5),
   never in-track hacks — the supervisor dedups across tracks (go and js will both want
   runfiles: one ticket).

Tracks fan out **after L2** (the Starlark core proven on a real ruleset). Candidate order: go
(non-cc-shaped compile+link model — the best generality probe; needs `ctx.actions.write` for
importcfg, a small generic engine move), then js/ts. Existing `.rs` language modules retire per
the ladder's Deletes column; `native_cc.rs`'s compiler half becomes a *toolchain* (where naming
cc is legitimate), not a language rule.

## §5 Operating model — supervisor + workers

Prompts and the communication protocol live in `dev-docs/v3-prompts/` (the README there is the
protocol spec). Summary of the model:

- **Supervisor** (frontier model; this doc is its contract): owns the ladder; runs the probe
  harness; writes tickets; spawns builders/reviewers; integrates **serially** (rebase → full
  suite + gates → commit + tag); maintains the debt register (`RazelGaps.md`); appends a
  checkpoint delta to the précis each round. NEVER implements engine-core changes via workers'
  tickets it hasn't ruled on; NEVER weakens gates (Gianni only).
- **Builders** (cheaper model, one ticket each, isolated git worktrees): test-first, smallest
  fix, full suite + gates green, structured handoff. Narrow context by design — the ticket MUST
  be self-contained.
- **Reviewers** (cheaper model, **fresh instance — never the builder's context**): adversarial
  review of ticket+diff. Independence is the point: a context that produced a diff cannot be
  trusted to judge it. Same model is fine; same conversation is not.
- **Probe harness** (`xtask probe`, to be built early in L2): deterministic ladder runner that
  emits first-failure per corpus, classified (missing-global / missing-member / semantic /
  resource). Agents do not search; the harness does.

**Parallelization rules (how time is actually saved):**
1. **Parallelize across the gate boundary, never within the engine core.** Language tracks,
   corpus goldens, probe/diagnosis (read-only), and doc work fan out freely — the C3c gate makes
   cross-track conflicts structurally impossible.
2. **Engine core: shard by file, else pipeline.** Tickets touching disjoint files
   (`engine.rs` namespace vs `values.rs` vs `registry.rs`) may fan out; otherwise run the
   pipeline — builder(N+1) ∥ reviewer(N) ∥ integrate(N−1).
3. **Parallelize the expensive, serialize the cheap.** Building/probing in parallel worktrees;
   integration (seconds) strictly serial through the supervisor. No merge trains.
4. **Escalate, don't block:** semantic rulings (stub vs real), seam designs, gate/test changes,
   vendoring, rung exits → Gianni. Mechanical stubs auto-proceed *into the debt register*.

## §6 Doc map

- **Plan of record:** this doc. **State snapshots:** `RazelV3Checkpoint3Precis.md` (current —
  per-round deltas append there from round 28); `RazelV2Checkpoint1Precis.md` (checkpoint 1 +
  the round 1–27 delta stream, closed).
- **Architecture of record (unchanged):** `RazelV2FinalArchProposal.md` + `RazelV2Contracts.md`;
  DDS model: `DdsQuerySystem.md`, `DdsCoveringSet.md`, `DdsWorkedTransformation.md`,
  `BzlToDdsValidation.md` — E0d implements what these specify.
- **Live specs:** `BazelCcCommandLine.md` (§8c/Constrain), `RazelParityHarness.md`,
  `RazelHookSeam.md` (C3 seam; its §10-references point at the retired V2 plan — historical),
  `RazelCodingRules.md` (binding on workers), `ArchFundamentals.md`/`ArchBazelConstraints.md`.
- **Backlog/debt register:** `RazelGaps.md`.
- **Retired to `history/`:** `RazelStarlarkBoundaryPlan.md` (V2 plan),
  `RazelV2FinalArchProposalPlan.md`, `RazelCcRules.md` (redirect stub).
