From: RG (this session, 2026-06-13)
Date: 2026-06-13
Status: open — flip when a fresh RG session has oriented.

# RG → RG: session handoff — orient here, then take the queue

You are RG, the grazel-lane agent. Repo-root `AGENTS.md` is the contract (identity
by tree, inbox protocol with RR, sync ritual, the razel→grazel crate/labor arrow —
you NEVER edit razel-*/xtask/parity). Tree: `razel-grazel/` on `razelv3`, share =
local bare `../razel.git` (remote `origin`). Gianni is the human; verify before
asserting, design docs BEFORE code, stages red-first, don't pause on allocated work.

## Orientation ritual

1. `cd razel-grazel` FIRST — the shell resets cwd to glial-dev between commands.
2. `git pull --rebase origin razelv3`; read BOTH inboxes for `Status: open`.
3. `dev-docs/ws-grazel/`: GrazelWorkstream.md (the GR plan), GrazelDebts.md (D1–D15,
   several retired in place), the design docs (Crates/Scopes/Verbs/HttpEdge/ViewSeam,
   GrazelOptionsSurvey — every LATER item has a named trigger; don't add surface
   without one).
4. Health: `cargo test --workspace` green + `./target/debug/grazel ws test`
   (31 stages; needs host cc + node ≥22 + the dev tree for parity/js/corpus stages).
   `python3 scripts/build-bins.py --verify` for release bits.

## State (as of this handoff)

GR0–GR4 COMPLETE (tags grazel/GR0..GR4) + GR5a (js client, clients/grazel-js) —
the gryth-bootstrap bar is closed both sides. View seam round 1 in (views.rs
KeyedFanout; GR5b = producer swap when RR's View service lands). Options NOW set
landed; gryth-examples corpus at clients/gryth-examples (triple duty: our stages,
RR's js goldens, T5). Flag seam: my inbox 0008 flip — lane 1 ours, lane 2 batched
to RR, no shared files.

## Queue (order), and what each waits on

1. `grazel locks` verb (backlog #2 remainder) — read-only workspace.lock scan; free.
2. `grazel doctor` (backlog #6) — env preflight; free.
3. js-client hardening (backlog #3 / ViewSeam round 2) — resubscribe/resync against
   the fan-out; free.
4. Corpus run stage → daemon-routed when RR fixes Razel.run labels (my 0011, open).
5. GR5b view-delta + query-snapshot stages — WAIT on RR's View service /
   snapshot-swap (announced via inbox).
6. Real TS client — WAITS on taut's TS runtime (Gianni's repo, debt D15).
7. @npm shim review seat — REACTIVE when RR's design note arrives.

## Gotchas that cost this session time

Push rejected = RR pushed first → `git pull --rebase` (AGENTS.md). Cargo.lock
conflict → take either side, rebuild. Cold razel-local builds need an ABSOLUTE
`-C` (razel bug, my 0010 — stages carry the workaround). A warm cache hit can MASK
that bug — cold-check before claiming a fix. macOS kills binaries overwritten in
place (build-bins.py unlinks first). Stage scratch paths must stay SHORT (104-byte
UDS cap; numeric stage dirs). `kill -0` output must be captured or it leaks into
verb stdout.
