From: RG
Date: 2026-06-12
Status: open

# Heads-up: GR3 blocks on your S3 — requirements so the wire grows right

WHAT: I'm starting GR1 completion now (autostart/handshake/stale-recovery/
shutdown/idle-out, consuming your S0 lib + Hello), GR2 (scopes for real) right
after. Then GR3 — `grazel build/query/run` through grazeld — which BLOCKS on S3
(GrazelWorkstream §3). Gianni asked me to flag it: I expect to need S3 within ~two
sprints. What GR3 needs on the wire (PublicSurfaces S-B, §4b), so S3's IR lands
consumable:

1. **Command service**: build/run/test — invocation id returned IMMEDIATELY;
   results, diagnostics and PROGRESS (sched_hook: "300/2000 BUILD files loaded",
   "action k/n") arrive as EVENTS on the invocation's stream; per-invocation
   ordering guaranteed (my `build-streamed` stage asserts id-before-events and
   progress-strictly-before-completion).
2. **Query** as a one-shot against the last COMMITTED snapshot (my `query-snapshot`
   stage runs a query DURING a build and must get the previous snapshot, not a
   torn one).
3. All messages in the taut IR (`wire/razel.taut.py`) — GR5 generates the TS client
   from the same IR, so nothing rust-private in the shapes.
4. The `Razel.hello` SERVER impl (you said it rides S3) — GR1 ships client-side
   hello against my own grazeld dispatch meanwhile.

WHY: seam-request latency is the split's named tax (GR §3) — batching GR3's needs
now so razel-wire grows once, not per-discovery.

ACCEPTANCE: when S3 lands, a note in ws-grazel/inbox/ naming the service messages +
stream envelope as landed (method names, event ordering guarantees). No reply needed
before then unless a shape above looks wrong to you.
