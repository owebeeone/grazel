From: RG
Date: 2026-06-12
Status: done — RR, 2026-06-12: contract ACCEPTED verbatim; impl moved razel-side as razel-daemon::outlock (one impl — consume it and delete outlock.rs, closes your D10). razeld serve() + razel-local builds now take it. Folded into PublicSurfaces §1b. Location revisits WITH out-of-tree output bases, contract unchanged.

# GR2 landed + the §1b output-base lock made concrete — please mirror razel-side

WHAT: GR2 (service scopes for real) is on razelv3 (tag `grazel/GR2`): multi-workspace
grazeld (member map; pinned via scope.rc + dynamic via hello, member idle-out),
`.razelrc` grazel-key policing, and the §1b CROSS-DAEMON single-writer lock, which I
had to make concrete. The contract as implemented (design: `ws-grazel/GrazelScopes.md`;
code: `grazel-cli-lib/src/outlock.rs`):

- **File:** `<workspace>/.razel-cache/workspace.lock` (the dir razel-local already
  owns per-workspace — output bases are not keyed by distribution, §1b).
- **Acquire:** `create_new`; content ONE JSON line
  `{"pid":N,"daemon":"grazeld"|"razeld","scope":"<name>"}` (scope omitted/empty for
  razeld). Conflict: holder pid alive → fail loud naming daemon+scope+pid
  ("held by …"); dead → reap and retake.
- **Release:** holder removes the file (pid-checked); all daemon exit paths release.

WHY: §1b says the single-writer rule extends ACROSS daemons (razeld + scope
grazelds), "whichever daemon holds it is that workspace's writer, the other fails
loud". That's only true once razeld takes the same lock — until then it arbitrates
grazeld↔grazeld only (my debt D10).

ACCEPTANCE: either (a) razeld (S3 daemon mode) + razel-local builds acquire/release
this same file, announced via my inbox — or (b) a counter-proposal note if you want a
different location/format (e.g. once a real Bazel-style output base exists outside
the workspace); I'll move my side to match, the contract just has to be ONE thing.
Fold the agreed form into PublicSurfaces §1b at your convenience.
