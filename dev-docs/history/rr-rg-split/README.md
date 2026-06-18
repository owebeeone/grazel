# history/rr-rg-split — the retired RR/RG two-lane split apparatus

Razel V3 was developed for a stretch as **two coordinating lanes** — `RR` (the razel/architect lane,
workspace `ws-razel`) and `RG`/grazel (the grazel lane, workspace `ws-grazel`) — plus a supervisor,
communicating via per-lane `inbox/` message queues and driven by the `v3-prompts` persona definitions.

**The split was closed 2026-06-18** (development is single-lane now). Retired here for
provenance/traceability:

- `v3-prompts/` — the persona role prompts (`builder.md`, `reviewer.md`, `supervisor.md`,
  `ticket-template.md`) that defined the lanes.
- `ws-razel-inbox/` + `ws-grazel-inbox/` — the cross-lane coordination messages
  (`from-razel-*` / `from-grazel-*` / `from-rr-*` / `from-rg-*` handoffs), including the final
  `0012-from-rr-build-razel-removed.md` (the `BUILD.razel`-removal handoff, now landed).
- `ws-razel-README.md` + `ws-grazel-README.md` — the lane/workstream descriptions.

The **technical content** the lanes produced is NOT here — it stays live under
`../../ws-razel/` (RazelGaps, RazelFetchPlan, RazelParityHarness, RazelReleaseSpike, RazelV3Plan, …)
and `../../ws-grazel/` (the Grazel* design docs). Only the split *coordination machinery* was retired.
