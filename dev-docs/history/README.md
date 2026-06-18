# dev-docs/history

Archived docs, kept for **provenance/traceability**. The live design entry point is
`../RazelDevStatus.md` (the developer's current-state description); the live `@crates`-build + query
design is `../RazelCrateUniverseDesign.md`.

**Pre-V2 layer** — status/proposals + the architecture-choice derivation+evidence (the
draft/critique/ArchAnal corpus, `RazelStarlarkBoundaryPlan.md`, `RazelStatus.md`, `Dds*`-adjacent
notes, …). Superseded by the V2 corpus; findings folded forward.

**V2 architecture-of-record (archived 2026-06-18)** — `RazelV2FinalArchProposal.md` +
`RazelV2Contracts.md` (the full designed substrate: the DDS spine + the seam-contract specs), plus
the `RazelV2FinalArchProposalPlan.md` realization plan. **Superseded *as the entry point* by
`../RazelDevStatus.md`**, which distills them and reconciles the designed substrate with the V3
reality (the live `razel-loading` path parks the DDS spine). Read these for the full designed detail;
the durable invariants are consolidated into `../RazelCodingRules.md` ("Architecture invariants").

**V3-era consumed/superseded workflow artifacts** — `Phase6Handoff.md` (the RazelCrateUniverse
Phase-6 session handoff — Phase 6 closed 2026-06-18); `RazelV2Checkpoint1Precis.md` (the
`razelV2-RSB/D4.4` checkpoint précis — a point-in-time snapshot); `RazelRustParityPlan.md` (the rust
action-graph parity plan — Phase A+B executed; inbound links updated to `history/`); and the **Grazel
Model-G** docs `GrazelProposal.md` + `GrazelForecast.md` (the OLD clean-slate-surface idea per their
own naming notes — superseded by, and UNRELATED to, the current grazel distribution in
`../RazelPublicSurfaces.md` §1b).

**The RR/RG two-lane split apparatus** — retired into `rr-rg-split/` (the persona prompts + the
cross-lane inbox coordination messages + the lane READMEs). The split was closed 2026-06-18;
development is single-lane now. The lanes' *technical* content stays live under `../ws-razel/` +
`../ws-grazel/`. See `rr-rg-split/README.md`.
