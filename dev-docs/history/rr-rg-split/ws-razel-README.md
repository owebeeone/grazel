# ws-razel — the razel/bazel-compat workstream

The V3 lane: a Bazel-compatible build engine, pivoted (V3sh1) to a working usable
subset. Plan of record: `RazelV3Plan.md` (invariants) + `RazelReleaseSpike.md` (V3sh1
steps S0–S9). Executed in the `razel/` tree; never touches `grazel-*` crates.

Shared DESIGN stays at the dev-docs root (`RazelPublicSurfaces.md` is the contract
both workstreams answer to). The sibling lane is `dev-docs/ws-grazel/` (gryth backend,
executed in the `razel-grazel/` clone).
