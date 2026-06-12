From: razel agent (seeded by Gianni, 2026-06-12)
Date: 2026-06-12
Status: done — both deliverables landed (RG, 2026-06-12): design at
dev-docs/ws-grazel/GrazelCrates.md (c50c273); crates grazel-cli / grazel-cli-lib /
grazel-node + `grazel ws test` 4-stage ladder green (553fdd3, tag grazel/GR0).
razel-cli touchpoint stays stubbed per this note — awaiting the S0 announcement.

# First instruction: THINK the grazel crate structure, THEN create it

Gianni's direction: the grazel structure needs some thinking first — which crates,
how they play together, naming — before any crate is created. So this note is two
deliverables, strictly in order.

## Deliverable 1 — `dev-docs/ws-grazel/GrazelCrates.md` (the design)

A short design doc deciding the crate set. Hard constraints already decided
(do not relitigate; sources: `RazelPublicSurfaces.md` §1b/§1d/§1e, `GrazelWorkstream.md` §0):

- **Naming: `grazel-*` for ALL grazel-specific crates.** No exceptions.
- **`grazel-cli`** — the bin crate (binary named `grazel`), SMALL: rust/OS mechanics
  only (argv/env intake, exit codes, signals, process entry); it just invokes the lib.
- **`grazel-cli-lib`** — ALL business logic, linking the razel-cli LIB (post-S0) plus
  the other grazel crates.
- **`grazel-node`** — placeholder only. Iroh is coming; do not design it; nothing may
  contradict per-scope identity (§1e).
- Depend on `razel-*` crates freely; NEVER modify one (seam requests go to
  `ws-razel/inbox/`). All payloads taut (§4) — no JSON shadow protocol.

Questions the doc must answer (your call, with reasoning):
- Where does the scope/daemon machinery live — inside `grazel-cli-lib`, or a separate
  `grazel-daemon` crate riding the razel-daemon lib? (Consider: the thin-bin rule, and
  that `grazel ws test` stages must run in-process under `cargo test`.)
- Where does the `grazel ws test` stage registry live, and what is a stage's type shape?
- Is the GR4 HTTP/WS edge a separate crate (`grazel-http`?) from day one or carved out
  later? (Bias: don't pre-create empty crates without a consumer — boilerplate smell.)
- Crate-level dependency diagram: arrows among grazel-* crates and into razel-*.

## Deliverable 2 — the crate skeletons (after Deliverable 1 is committed)

Create the crates per your design. ADDITIVE ONLY: new `crates/grazel-*` dirs auto-join
the workspace via the `members = ["crates/*"]` glob — touch no existing file, so the
merge back to razelv3 stays trivial. Each crate lands with a compiling skeleton + at
least one test (TDD applies from the first file; the `grazel ws test` verb with its
`harness-selftest` stage is the GR0 exit).

Note: the S0 seam (razel-cli `[lib]` split + CI deny gate) lands razel-side and reaches
you via `git pull` on razelv3 — until it arrives, don't consume razel-cli internals;
stub the touchpoint. Watch this inbox for the "S0 landed" note.
