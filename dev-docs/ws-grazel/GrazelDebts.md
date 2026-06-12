# GrazelDebts — debts recorded per GrazelWorkstream §0 (this lane's, not RazelGaps)

| # | debt | opened | retires |
|---|---|---|---|
| D1 | razel-cli verb-dispatch STUB in grazel-cli-lib | GR0 | **RETIRED at GR1** — `razel_cli::run` consumed |
| D2 | `build.subscribe` through grazeld answers a polite error — streaming over the scope socket is GR3's invocation-stream work | GR1 | **RETIRED at GR3b** — serve_conn delegation (ReplayConn) |
| D3 | Hello's `workspace_root` is decoded but not ROUTED (single workspace handle per daemon); per-workspace handles + scope-routing land GR2 | GR1 | **RETIRED at GR2** — member map, hello opens/touches members |
| D4 | `GRAZEL_FAKE_WIRE_PROTOCOL` env: documented TEST SEAM (ws-test `version-handshake` plants a mismatched daemon); autostart scrubs it from spawned daemons. Replace with a real cross-version harness when two wire protocols actually exist | GR1 | first real protocol bump |
| D5 | `grazel --help`/unknown-verb shows RAZEL usage (delegation is verbatim); a combined usage page is cosmetic, deferred | GR1 | when it annoys someone |
| D6 | razel verbs under grazel run in-process (razel's own behavior) — scope routing of build/query/run through grazeld is GR3's whole point | GR1 | **RETIRED at GR3** — build/affected routed (GR3a), run streamed (GR3b) |
| D7 | launch lock is create_new + timeout surfacing a crashed launcher's stale lock in the error message; no auto-reap of dead-holder locks (the OUTPUT lock reaps; the launch lock doesn't yet) | GR1 | GR3 |
| D9 | Interim wire routing: non-hello requests go to the scope's SOLE member; >1 members → refused. GR3's invocation envelope carries workspace identity properly | GR2 | GR3 |
| D10 | The workspace.lock contract is grazeld↔grazeld only until razeld mirrors it (proposed to razel lane, inbox 0005) — razel-local builds don't take the lock yet | GR2 | razel-side S3+ |
| D11 | GR4b (WS streams over the HTTP edge) deferred: stream semantics belong to GR3's invocation streams — freezing them now would pre-empt S3. `ws-stream-equivalence` stage lands with GR4b | GR4a | **RETIRED at GR4b** — RFC6455 server half, byte-identical event payloads proven |
| D12 | `query-snapshot` stage deferred: Query-against-committed-snapshot isn't formalized razel-side yet (RR's named gap, inbox 0005 — the §1c contract). Lands when razel's snapshot-swap does | GR3 | razel snapshot-swap |
| D13 | `grazel run` flag surface is narrow (target, --scope, -C, `-- args`) until razel-cli grows a daemon-routed run to delegate to — then grazel delegates verbatim like build | GR3b | razel-cli daemon-routed run |
| D8 | grazeld runs INDEFINITELY by default (decision: Gianni 2026-06-12 — it's the long-lived scope service; razeld keeps idle-out, razel's lane). Real home is OS service management (launchd/systemd); `--idle-timeout` remains opt-in | GR1 | service-ification arc (post-iroh) |
