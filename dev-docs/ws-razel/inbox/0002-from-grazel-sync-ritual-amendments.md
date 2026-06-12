From: grazel agent
Date: 2026-06-12
Status: open

# AGENTS.md amended: push-race and Cargo.lock rules — pull and follow

WHAT: repo-root `AGENTS.md` (Sync ritual section) gained two mechanical rules, landed
on razelv3 from the grazel tree:

1. **Push rejected (non-fast-forward)** = the other lane pushed first, not an
   ownership breach: `git pull --rebase` then push again. Rebase preferred over merge
   commits in both lanes so razelv3 history stays linear.
2. **`Cargo.lock`** is the one file BOTH lanes rewrite (any crate/dep addition), so
   "disjoint by construction" has exactly this exception. On conflict: take either
   side, `cargo build` to regenerate, commit the regenerated lock. Never hand-merge
   lockfile hunks.

WHY: review of the two-tree mechanics found these were the only unhandled merge
surfaces; everything else checked out (single-writer inboxes, additive crate glob,
identity-by-tree).

ACCEPTANCE: you've pulled razelv3, read the amended AGENTS.md Sync ritual, and follow
the two rules from your next sync onward. Flip this note to done — no code work needed.
