From: RG
Date: 2026-06-13
Status: open

# Two small follow-ons to your 0008 loader fix (found authoring the corpus)

WHAT — hit while wiring `grazel test` + the gryth-examples corpus (your 0010 drops):

1. **`Razel.run` didn't get the 0008 treatment.** `grazel run "//:server"` through
   grazeld streams progress fine (`[load] 0/0 //:server` — nice) but the build half
   FAILS server-side, while `do_build` with the same label succeeds (your fix routed
   do_build's //-labels onto build_workspace_with; Razel.run's internal build still
   rides the old path). Repro: gryth-examples/01-hello-http (on razelv3 after my
   next push), `grazel run "//:server"` vs `grazel build "//:server"`.
2. **Bare names don't see BUILD.razel.** `razel build server -C <E-mode ws>` →
   "no BUILD or BUILD.bazel" — the bare-name single-package path predates E-mode
   and only probes the bazel grammar filenames. //-labels work. Low stakes (labels
   are the norm) but it's a grammar hole E-mode users will trip on.

Meanwhile: corpus stage runs the server via `--no_daemon` (razel-local run takes
the label fine); the daemon-routed run is the ready-made red test for (1).

FYI per 0010: corpus is landing at `clients/gryth-examples/` (hello-http js_binary,
js_test suite, ts_project — ts one is yours/T5's, no host tsc in my stage deps);
`grazel test` stages land with it (verbatim delegation v1 per my 0010 flip).

ACCEPTANCE: (1) daemon-routed `grazel run "//:label"` builds+streams on the corpus —
I flip my corpus stage from --no_daemon to daemon-routed same day; (2) at your
leisure, it's a one-line probe-list fix.
