# AGENTS.md — common instructions for the razel & grazel workstream agents

Two agents work this codebase in two checkouts of the SAME repo, sharing branch
`razelv3` through the local bare `../razel.git`:

- **Tree `razel/` → you are the RAZEL agent, designated "RR".** Lane:
  `dev-docs/ws-razel/` (`RazelReleaseSpike.md` — bazel-compat + working subset).
  You own `razel-*` crates.
- **Tree `razel-grazel/` → you are the GRAZEL agent.** Lane: `dev-docs/ws-grazel/`
  (`GrazelWorkstream.md` — gryth backend, GR0–GR5). You own `grazel-*` crates.

Agent designations (Gianni assigns them): sign your inbox notes' `From:` line with
your designation; the filename sender token stays the lane name (`from-razel` /
`from-grazel`) so paths stay stable if designations evolve.

Shared DESIGN lives at the `dev-docs/` root — `RazelPublicSurfaces.md` is the contract
both lanes answer to. **Ownership is absolute:** never edit the other lane's crates.
The dependency arrow (grazel-* may depend on razel-*, never the reverse) is also the
labor arrow, enforced by the CI deny gate.

## How to communicate with your colleague agent

You have a colleague agent working the other lane. When you need something folded into
THEIR work — a seam change, an announcement, a question — write a note DIRECTLY INTO
THE COLLEAGUE'S inbox:

- razel agent writes into `dev-docs/ws-grazel/inbox/`
- grazel agent writes into `dev-docs/ws-razel/inbox/`

**Filename:** `NNNN-from-<who-am-I>-<short-slug>.md` — NNNN is the next number in that
folder; `<who-am-I>` is `razel` or `grazel`. Example:
`dev-docs/ws-razel/inbox/0002-from-grazel-need-view-msgs-in-wire-ir.md`.

**Content:** header lines `From:` / `Date:` / `Status: open`, then plainly: WHAT is
needed, WHY (one line), and the ACCEPTANCE — how your colleague knows it's done.
Self-contained: your colleague has none of your session context; name files, crates,
and doc sections explicitly.

**Delivery is the push:** commit the note and push `razelv3` to the share immediately —
an unpushed note does not exist.

**Your own inbox:** check it at EVERY sync (pull). Act on open notes, then flip
`Status: open → done` in place, adding one line on where the work landed. Notes are
never deleted — the folder is the log.

## Sync ritual

`git pull` razelv3 from the share (`share` remote in razel/, `origin` in razel-grazel/)
→ read your inbox → rebuild → full suite green (razel-grazel additionally:
`grazel ws test`) → work → commit → push. Both lanes commit to the same branch; the
ownership rule keeps merges trivial — a conflict outside the declared seams means
someone broke the ownership rule, stop and say so.

## Process (both lanes)

TDD first — the failing test precedes the implementation (the grazel lane's acceptance
granularity is e2e `grazel ws test` stages, per `GrazelWorkstream.md` §0). Every step
lands green, is committed with a terse message (≤3 lines, no Co-Authored-By trailer),
and records debts in its OWN lane's docs. All protocol payloads are taut — never
introduce a JSON shadow protocol.
