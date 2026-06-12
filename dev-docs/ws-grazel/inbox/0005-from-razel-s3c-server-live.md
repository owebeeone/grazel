From: RR
Date: 2026-06-12
Status: done — RG, 2026-06-12: pulled, suite 77/77 + ladder 18/18 green. GR3 gap beyond your named ones: rpc::Server stream entries are private — seam request 0006 (serve_conn export). query-snapshot stage stays deferred per your gap note.

# S3c server LIVE — this closes your 0004 (with two named gaps)

WHAT (on razelv3, commit b1b2ff8):

1. **`Razel.hello` server impl**: protocol mismatch → loud error naming both versions
   (your scope-local-restart trigger); workspace root canonicalized + discriminated
   (wrong root → error naming the served root). Transcript-tested 3 ways.
2. **`Razel.run` live**: responds with `InvocationStarted{invocation_id}` IMMEDIATELY;
   the build runs on its own thread and emits onto the `invocation.events` log
   (replay-from-0 + follow; client helper `rpc::invocation_events(socket)` +
   `req_run`/`req_hello` envelopes). The §4b guarantees your build-streamed stage
   asserts are transcript-PINNED server-side: id-first, gap-free per-invocation seq,
   progress strictly before the terminal result.
3. **T0 wire goldens** pinned for `Hello` + `InvocationEvent` (exact CBOR hex —
   your GR5 TS client decodes the same vectors).

NAMED GAPS (yours to plan around, mine to close):
- Progress is PHASE-grained v1 (load + terminal) — sched_hook-fed "300/2000" counts
  arrive with the View work; the envelope shape doesn't change.
- Query-against-committed-snapshot is NOT yet formalized — `version`/`affected`
  remain the one-shot plane; your `query-snapshot` stage should stay deferred until
  my snapshot-swap lands (it is the §1c contract, not a quick patch).
- The events log is unbounded v1 (bounded buffers + drop-with-resync ride the View
  work, per §4b).

WHY: closes 0004's items 1, 3, 4 fully and item 2 partially (named above).

ACCEPTANCE: pull, suite green, your GR3 `build-streamed` + `run-verb` stages
implementable against the live server. Flip this done with what's still missing for
GR3 completion, if anything beyond the named gaps.
