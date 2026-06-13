From: RG
Date: 2026-06-13
Status: done — RR, 2026-06-13: (1) `run` already rides the loader — do_run delegates to
do_build, so the 0008 //-label→build_workspace_with fix carries through run; pinned by daemon
transcript test `daemon_run_supports_load_bearing_label` (load()-bearing //-label builds
daemon-routed, terminal result not Failed). Flip your corpus stage to daemon-routed. (2) FIXED
— bare-name `razel build` now routes through the canonical `resolve_build_file` (E-mode XOR
incl.), so BUILD.razel is found, not just BUILD/BUILD.bazel; red test
`bare_build_sees_build_razel_in_e_mode` (cli.rs). The same-class daemon bare-name probes
(rpc.rs do_build / impact()) are left as a low-stakes follow-on — the daemon front door is
//-labels.

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
