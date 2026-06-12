# GrazelViewSeam — mock→real subscription machinery (backlog #1, GR5b prep)

*2026-06-13, RG. Design BEFORE code (house rule). Goal per inbox 0009: build
gryth's subscription machinery NOW so RR's View service lands as a producer SWAP
(GR5b = a day, not a sprint). Constraints: §4b (stream-first, bounded buffers,
drop-with-resync), §0 taut-everywhere (NO invented wire messages), the
mock→real seam pattern (consumers never rewritten).*

## The key design move: the "mock" is a REAL stream

Inventing a fake View feed would force fake WIRE messages — a shadow protocol §0
forbids. Not needed: **`build.subscribe` is already view-shaped** (an ATOM: full
`BuildState` snapshot on connect, fresh snapshot per revision) and
`invocation.events` is already log-shaped. Both are live, taut, and served
through grazeld since GR3b/GR4b. So the seam's day-1 producers are these REAL
streams; the View-era producer is the same trait over `View.subscribe` when its
IR lands. Nothing fake ships, and the swap is a producer registration.

## The seam (grazel-cli-lib `views` module)

```rust
/// A view key names an upstream subscription: today "build" | "invocations";
/// View-era keys arrive with the View IR (same trait, new producer).
pub trait ViewProducer: Send + Sync {
    fn open(&self, key: &str) -> Result<Box<dyn Conn>, String>; // the upstream stream
}
```

- **KeyedFanout** — `key → one upstream connection + N subscriber buffers`.
  Subscribers to the same key SHARE the upstream (dedup); refcounted; the
  upstream closes at zero subscribers (the §1 idle pattern, member-style).
- **Bounded buffers (§4b)** — per-subscriber ring of raw envelope frames, bound
  configurable (default 256 frames). A slow consumer overflows → its buffer is
  CLEARED and a RESYNC marker delivered; on an atom stream the next upstream
  frame IS the fresh snapshot (atoms self-heal — the §4b property that makes
  drop-with-resync cheap); on a log stream the marker carries the last delivered
  seq so the client can re-request replay. The marker is the existing error
  envelope (`{1:false, 3:"resync:<n>"}`) — in-band, no new wire type.
- **The WS edge bridges per key**: `GET /events` carries the request envelope as
  today (first WS message names the stream); the fan-out sits between the edge
  and the member server instead of each WS connection opening its own upstream.
  UDS subscribers keep their 1:1 connections (razel clients are local and few;
  fan-out is for the gryth edge where N browsers share a daemon).

## What this buys at View-swap time

RR's View service ships `View.subscribe` + delta messages + server-side bounded
buffers (his named gap list). At that point: register the View producer for view
keys; gryth's client machinery (resubscribe loops, resync handling — js client
hardening, backlog #3) is already exercised against the same fan-out semantics.
`view-delta-roundtrip` (GR5, debt D14) becomes implementable the same day.

## Stages (red-first, all against REAL daemons)

- `view-fanout-dedup` — two WS clients subscribe the same key; assert both see
  the same byte sequence AND the daemon holds ONE upstream (observable: member
  connection accounting via `daemon status` line `fanout=<key>:<n>`).
- `view-resync-after-drop` — tiny bound (`--view-buffer=4` test knob), a stalled
  reader; assert the resync marker arrives, then a coherent fresh snapshot, and
  the FAST subscriber saw no gap.
- `view-idle-closes-upstream` — both clients close; assert the upstream count
  drops to zero (status line gone).

## Sizing & debts

Round 1: `views` module + fan-out + bounds + status line + the three stages.
Round 2: js-client resubscribe/resync loops (backlog #3) against it. Opens
D16: resync semantics for LOG streams pin "replay from seq" — needs the View-era
IR to confirm; the marker shape is forward-compatible (in-band error envelope).
