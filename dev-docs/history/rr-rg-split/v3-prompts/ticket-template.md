# T-<seq>-<slug> — <one-line gap statement>

- **Rung:** <L2 | L3 | … | E-epic | lang-track:<lang>>
- **Class:** <missing-global | missing-member | semantic | core | lang-track>
- **Ruling:** <stub — debt recorded in RazelGaps.md | semantic — full behavior required>
- **Shard key:** <files this will touch — the supervisor's parallel-scheduling input>

## Repro (must fail before the fix, verbatim)

```
<command, e.g. cargo test -p razel-loading --test probe_x -- --nocapture>
```
Expected failure: `<the exact error line>`

## Exit criterion

<One sentence a test can state. The builder's new test asserts THIS, nothing else.>

## Files

- `<path>` — <why / what changes here>
- (test) `crates/<crate>/tests/<file>.rs` — new or extended

## Context the builder needs (self-containedness checklist)

<Everything a cold agent needs: the relevant contract line from RazelV3Plan/RazelHookSeam,
the registry row to add, the upstream .bzl line being satisfied, etc. If this section needs
session history to write, the ticket is not ready.>

## Out of scope

<Explicitly name the adjacent things NOT to touch — the reviewer enforces this.>
