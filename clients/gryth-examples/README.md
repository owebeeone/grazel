# gryth-examples — the razel-native fixture corpus (inbox 0010, RG-owned)

Small E-mode workspaces shaped like the things gryth will actually build. Triple
duty: grazel's ws-test stages drive them (`test-verb-protocol`,
`gryth-examples-corpus`), razel's examples burn-down uses them as js goldens, and
T5 (gryth acceptance) grows from them.

| workspace | shape | exercised by |
|---|---|---|
| `01-hello-http` | `js_binary` node http server (built-ins only; `GRYTH_EXAMPLE_ONESHOT=1` serves one request and exits — the stage knob) | grazel build + run stages |
| `02-js-tests` | `js_test` suite (plain node scripts, exit code = verdict, `node:assert`) | grazel test stage |
| `03-ts-project` | `ts_project`-lite (one tsc action) | razel-side goldens + T5 — NOT a grazel stage: host `tsc` is not a ws-test prerequisite (node is; tsc follows razel's host-toolchain posture) |

Rules ride the aspect surface (`@aspect_rules_js//js:defs.bzl`,
`@aspect_rules_ts//ts:defs.bzl` — the no-competing-surfaces rule, spike §3c).
Every workspace is a razel-native MODULE: empty `MODULE.razel` boundary marker,
`BUILD.razel` sole grammar (E-mode XOR). Stages COPY a workspace to scratch
before building — the corpus stays pristine (no `.razel-cache` churn in-tree).
The corpus is all-green by definition; failure-path fixtures (exit-3 protocol)
live inside the stages, not here.
