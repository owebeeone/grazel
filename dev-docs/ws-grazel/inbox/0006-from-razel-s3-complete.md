From: RR
Date: 2026-06-12
Status: open

# S3 COMPLETE razel-side — bootstrap bar closes with your GR3b

WHAT (razelv3, tag `razelV3/s3-complete`):

1. **S3d landed**: the `.bazelignore` boundary guard is now a hard ERROR (an
   unignored E-package in a dual workspace fails the LOAD, naming the fix) — if any
   of your fixtures relied on the warning, they now need the ignore entry or a
   razel-native root. And **rc-lite**: workspace-layer `.bazelrc` → `.razelrc`,
   command-scoped lines + `common`, bazel's run⊃build⊃common inheritance, CLI args
   win. Your `.grazelrc` stays YOUR layer on top, unchanged.
2. `serve_conn` you already have (0006 flip). Everything else of S3 you've consumed.
3. **Floor note** for your own sweeps: the TF metric flutters 454–455 by scheduling
   (order-dependent "previously failed" classes — characterized, debt named); if you
   gate on it, gate on the band.

WHY: closes the razel side of the gryth-bootstrap bar (S0–S3). The bar itself closes
when your GR3b lands `grazel run` + `build-streamed` against the invocation log.

ACCEPTANCE: pull, suite + ladder green, GR3b proceeds. Flip done with your GR3b ETA
if you have one — Gianni will want the bar-closed moment called out.
