From: RG
Date: 2026-06-12
Status: done — RR, 2026-06-12: (1) lock was degitted + .razel-cache/ gitignored in the S3d bank (f22c187) — your phantom-pid analysis is the better WHY, recorded; offending test now uses a tempdir workspace. (2) IR comment pinned: "gap-free per invocation STARTING AT 1"; regenerated.

# Hygiene: a runtime workspace.lock is COMMITTED in razel-daemon (+ one nit)

WHAT (two small things, both razel-side):

1. **`crates/razel-daemon/.razel-cache/workspace.lock` is tracked in git** —
   committed in a6ab5d0 (a test that uses the crate dir as its workspace, then a
   broad `git add`). Beyond churn (every local test run dirties the tree — it
   blocked my `pull --rebase` today), a COMMITTED lock is a real hazard: it ships
   a pid, and on a fresh checkout the acquire path treats that pid as the holder —
   if an UNRELATED process happens to be alive at that pid, builds in that
   workspace fail loud with a phantom "held by razeld". Fix:
   `git rm --cached` it, add `.razel-cache/` to .gitignore, and ideally point the
   offending test at a tempdir.
2. **Nit:** the IR comment for `invocation.events` says "seq is gap-free per
   invocation" but doesn't pin the START. The server emits 1-based; my
   build-streamed stage asserts gap-free-from-1 to match. One word in the IR
   comment ("starting at 1") makes the contract explicit for the GR5 TS client.

WHY: (1) bit me mid-sync today and will bite any fresh clone with pid luck;
(2) GR5 generates the TS client from the IR — comments are its spec.

ACCEPTANCE: lock file untracked + ignored (announce; I'll drop my local copy on
pull); IR comment pinned. FYI alongside: GR3 is COMPLETE on razelv3 (tag
`grazel/GR3`) — serve_conn consumed (ReplayConn delegation), `grazel run`
streams the invocation log end-to-end, ladder 21 stages green.
