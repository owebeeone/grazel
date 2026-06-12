# GrazelVerbs — GR3 design: razel verbs through the scope daemon

*2026-06-12, RG. GR3 lands in two slices: GR3a (this round — unary verbs routed,
parity proven) and GR3b (streams: `grazel run` + invocation events, gated on the
serve_conn export, inbox 0006). Constraints: §1d (razel's CLI surface verbatim, a
razel flag never behaves differently), §1e (every grazel command dials its scope),
GR3 (end-to-end via the shared razel-cli lib over the scope socket).*

## Routing rule (the §1d/§1e reconciliation)

- **Default routing differs; flag semantics never do.** Under razel, `build` is
  local unless `--daemon`. Under grazel, `build`/`affected` go THROUGH the scope
  daemon (that's §1e's posture — grazel CLI everywhere, grazeld a strict
  superset): grazel resolves the scope, ensures the daemon (hello → membership →
  output lock), then delegates to `razel_cli::run` with `--daemon --socket
  <scope socket>` injected. Same parser, same rendering — byte-identity by
  construction, re-proven over the daemon path by the `build-parity` stage.
- **Explicit routing wins verbatim:** if the user passed `--daemon` or `--socket`,
  grazel injects NOTHING and delegates untouched — razel flags keep razel
  semantics exactly (pointing razel/grazel at any socket stays a user right).
- **The one peeled flag is `--scope`** (grazel-namespaced; razel never sees it).
  Workspace for scope resolution honors razel's own `-C`/`--workspace` by a
  non-destructive scan — the flag itself still reaches razel's parser untouched.
- `version`/`subscribe` stay as-is this round: `subscribe` is a stream (GR3b);
  grazel `version` identifies the distribution (deliberate shadow, GrazelCrates).

## `grazel test` (2026-06-13, inbox 0010 drop 1)

**Verbatim delegation v1** — `test` reaches razel's verb untouched (build → exec →
bazel's exit protocol: 0 pass / 3 tests-failed / 1 build-failed; testlogs under
`.razel-cache/testlogs/`). The routing question RR flagged resolves to "no daemon
involvement loses nothing": the workspace's `.razel-cache` is shared across daemons
and local builds BY CONSTRUCTION (§1b — outputs aren't keyed by distribution), so a
daemon-warmed cache benefits a local `test` build automatically. Daemon-routed test
(id + streamed progress for the build half) arrives when razel's test verb itself
rides the invocation plane — not a grazel-side fork. Stages: `test-verb-protocol`
(exit 0/3, PASSED/FAILED lines, test.log content, zero daemon artifacts),
`gryth-examples-corpus` (the real shapes end-to-end).

## Streams (GR3b, after inbox 0006's serve_conn export)

`grazel run <target> [-- args]`: hello → `Razel.run` over the scope socket →
`InvocationStarted{id}` immediately → follow `invocation.events` (replay+follow
log), render Progress to STDERR (`[phase] done/total detail`), exec the built
output locally on a Built terminal (same exec contract as razel's local run).
Stages `build-streamed` (id-first, gap-free seq, progress-before-terminal — the
§4b guarantees, asserted through grazeld) and `run-verb` land here, completing
GR3's ladder. `query-snapshot` stays deferred until razel's snapshot-swap (RR's
named gap, inbox 0005).
