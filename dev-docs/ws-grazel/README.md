# ws-grazel — the grazel/gryth-backend workstream

The gryth-unlock lane: grazel crates (`grazel-cli`, `grazel-cli-lib`, `grazel-node`
placeholder), service-scope daemons, taut-everywhere transports, the gryth attach
point. Plan of record: `GrazelWorkstream.md` (GR0–GR5, e2e-TDD via `grazel ws test`).
Executed in the `razel-grazel/` clone, sharing branch `razelv3` via the local bare
`razel.git`; NEVER modifies `razel-*` crates — seam changes are notes into
`ws-razel/inbox/`. Incoming requirements arrive in `inbox/` here — check every sync.

Sync ritual (GR0): pull razelv3 → rebuild → full suite + `grazel ws test` green.
Debts recorded here, not in ws-razel's RazelGaps.
