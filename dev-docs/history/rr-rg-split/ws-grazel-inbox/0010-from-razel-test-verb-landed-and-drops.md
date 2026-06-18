From: RR
Date: 2026-06-13
Status: done — RG, 2026-06-13: taking all three. (1) grazel test ROUTING DECIDED:
verbatim delegation v1 (test = build+exec local; the workspace's .razel-cache is
shared by construction §1b, so a prior daemon-routed build warms it anyway — zero
loss, recorded in GrazelVerbs.md next round) + stages; (2) gryth-examples corpus —
I'll author after reading your js_test exec semantics + survey js rows; (3) review
seat accepted, flag me on the @npm note. Order: 1 → 2; 3 reactive. FYI meanwhile:
View seam round 1 landed (KeyedFanout, ladder 29) — GR5b is producer-swap-ready.

# `razel test` landed + three drops for your queue (Gianni: keep you fed)

WHAT (razelv3, c6268f1 + 1f5caf5): `razel test` is live — build → exec → bazel's
protocol (exit 0 pass / 3 tests-failed / 1 build-failed; testlogs/<pkg>/<name>/
test.log; `//t:x PASSED in 0.3s` lines). `js_test` + `js_library` are on the aspect
surface; `cc_test` on @rules_cc. Drops, in my suggested order:

1. **`grazel test` (+ `grazel run` routing review):** wire the new verb into your
   surface — delegation is verbatim via razel_cli::run, but YOU own the
   daemon-routing decision (test execs locally; does grazel ensure-daemon for the
   build half then exec, or pure --no_daemon delegation v1?). ws-test stages:
   pass/fail exit codes through grazel, test.log lands, summary line renders.
2. **A gryth-examples fixture corpus (you own it):** a small tree of razel-native
   E-mode workspaces under ws-grazel (or clients/) — hello http server (js_binary),
   a js_test suite, a ts_project — used three ways: your ws-test stages, my
   examples-verb goldens for js, and the T5 gryth-acceptance fixture later. You
   know what shapes gryth will actually use; better you author them than I guess.
3. **Review seat on the @npm shim design:** the `frontend` example's next wall is
   `@npm//:defs.bzl` (aspect's npm_link_all_packages machinery). The shim is
   razel-side (mine), but it sits on fetch-npm's materialization and YOUR npm
   instincts — when I write the design note I'll flag you for a review flip before
   implementation.

WHY: Gianni asked what else can flow your way while protocol work blocks GR5b/c;
these are all in your file set (1, 2) or review-only (3).

ACCEPTANCE: flip with what you take. FYI: survey burn-down is live
(parity/examples/SURVEY.md, 17/44 fully green) — js rows are yours to read for
gryth-relevant signal.
