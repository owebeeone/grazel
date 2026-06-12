# GrazelDebts — debts recorded per GrazelWorkstream §0 (this lane's, not RazelGaps)

| # | debt | opened | retires |
|---|---|---|---|
| D1 | razel-cli verb-dispatch STUB in grazel-cli-lib | GR0 | **RETIRED at GR1** — `razel_cli::run` consumed |
| D2 | `build.subscribe` through grazeld answers a polite error — streaming over the scope socket is GR3's invocation-stream work | GR1 | GR3 |
| D3 | Hello's `workspace_root` is decoded but not ROUTED (single workspace handle per daemon); per-workspace handles + scope-routing land GR2 | GR1 | GR2 |
| D4 | `GRAZEL_FAKE_WIRE_PROTOCOL` env: documented TEST SEAM (ws-test `version-handshake` plants a mismatched daemon); autostart scrubs it from spawned daemons. Replace with a real cross-version harness when two wire protocols actually exist | GR1 | first real protocol bump |
| D5 | `grazel --help`/unknown-verb shows RAZEL usage (delegation is verbatim); a combined usage page is cosmetic, deferred | GR1 | when it annoys someone |
| D6 | razel verbs under grazel run in-process (razel's own behavior) — scope routing of build/query/run through grazeld is GR3's whole point | GR1 | GR3 |
| D7 | launch lock is create_new + timeout surfacing a crashed launcher's stale lock in the error message; no auto-reap of dead-holder locks | GR1 | GR2 (per-scope daemons for real) |
| D8 | grazeld runs INDEFINITELY by default (decision: Gianni 2026-06-12 — it's the long-lived scope service; razeld keeps idle-out, razel's lane). Real home is OS service management (launchd/systemd); `--idle-timeout` remains opt-in | GR1 | service-ification arc (post-iroh) |
