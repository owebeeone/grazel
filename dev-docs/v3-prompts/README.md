# v3-prompts — the supervisor⟷worker protocol

How the V3 multi-agent operating model (`../RazelV3Plan.md` §5) runs in practice. The prompt
files here are templates: the supervisor fills the `{{…}}` slots and spawns a worker with the
filled prompt as its entire task description.

## Roles and the cycle

```
 xtask probe ──▶ supervisor ──ticket──▶ BUILDER (worktree, cheap model)
                    ▲                      │ diff + handoff
                    │   verdict            ▼
              integrate serially ◀── REVIEWER (fresh instance, cheap model)
              (rebase → suite+gates → commit+tag)
```

- **Builder and reviewer are separate fresh instances — always.** The reviewer must not share
  the builder's context: a context that produced a diff will rationalize it. Same *model* is
  fine; same *conversation* never. They may even be the same prompt-template family — what
  matters is the fresh context and the adversarial framing.
- **Workers are stateless.** Everything a worker needs is in: its prompt (from this folder),
  the ticket, and the files/docs the ticket names. If a builder needs session lore, the ticket
  is defective — fix the ticket, not the worker.
- **Integration is serial and supervisor-only.** Workers never commit to the main branch, never
  tag, never push. They hand back a diff (their worktree branch); the supervisor rebases in
  ticket order, re-runs the full suite + gates, commits, tags.

## Spawning (Claude Code mechanics)

The supervisor session spawns workers with the Agent tool:
- builders: `subagent_type: general-purpose`, `isolation: worktree`, cheaper model
  (`model: sonnet`), prompt = `builder.md` with slots filled.
- reviewers: same, but **no worktree needed** (read-only on the diff) and prompt = `reviewer.md`.
- independent tickets (per the §5 sharding rules): spawn in one message, run concurrently.

## Files

- `supervisor.md` — the standing prompt that makes a fresh session *be* the supervisor.
- `builder.md` — build-one-ticket template.
- `reviewer.md` — adversarial-review template.
- `ticket-template.md` — what the supervisor fills in; the self-containedness checklist.

## Tickets

Tickets are markdown files under `scratch/tickets/` (`T-<seq>-<slug>.md`), written by the
supervisor (template: `ticket-template.md`), one gap each. A ticket MUST be executable cold:
repro command, expected-vs-actual, the files to touch, the exit criterion, and the
stub-vs-semantic ruling. Closed tickets stay in place (audit trail); the debt they create is
recorded in `dev-docs/ws-razel/RazelGaps.md`, not in the ticket.

## Hard rules for every worker (restated in each prompt)

1. Full gate: `cargo test --workspace` && `cargo run -q -p xtask -- gates` — green before handoff.
2. Test-first: the failing test exists and fails before the fix (AGENTS.md).
3. NEVER edit a test, gate, or allowlist unless the ticket explicitly scopes it.
4. Stay inside the ticket's named files; needing another file = report back, don't improvise.
5. Commit style if asked to commit in-worktree: ≤3 lines, terse, no Co-Authored-By.
