# GrazelHttpEdge — GR4 design (GR4a landed early; GR4b landed post-GR3)

*Update: GR4b is IN (tag `grazel/GR4`) — `GET /events` upgrades to WS (hand-rolled
RFC6455 server half, vectors pinned); the first WS message is the request envelope,
each server message one response envelope, payload-byte-identical to a UDS
subscriber (`ws-stream-equivalence` proves it on the invocation log). "One
protocol, three transports" is now provable end-to-end.*

*2026-06-12, RG. GR3 blocks on razel-side S3 (Command service on the wire); per
GrazelWorkstream §3 ("sequence the trees so neither idles") the UNBLOCKED half of
GR4 lands first. Constraints: §1b (grazel serves HTTP/WS to gryth clients),
GR4 (taut payloads, transport equivalence, LOCALHOST ONLY, no presentation
semantics — the glade layer stays TBD).*

## The split

- **GR4a (this round): one-shot HTTP.** `POST /rpc`, request body = EXACTLY the
  CBOR request map UDS carries (no length prefix — HTTP frames via
  Content-Length), response body = EXACTLY the response envelope bytes. Same
  dispatch object, same bytes: transport equivalence is checkable byte-for-byte.
- **GR4b (post-GR3): WS streams.** Subscriptions mirror GR3's invocation streams;
  doing WS now would freeze stream semantics GR3 is about to define. Deferred,
  not forgotten (ws-stream-equivalence stage lands then).

## Decisions

- **Implementation: hand-rolled minimal HTTP/1.1 in `grazel-cli-lib::http`** —
  ~100 lines (request-line + headers + Content-Length body; `Connection: close`;
  no keep-alive, no chunking, no TLS). Zero new deps, matching the workspace's
  lean-dependency posture; an HTTP crate buys nothing at this surface area. The
  crate-vs-module split trigger (GrazelCrates.md) still applies — GR4b's WS work
  is its likely firing point.
- **Listener: always on, `127.0.0.1` ONLY, ephemeral port** recorded in
  daemon.json (`"http_port":N`) — §2 GR4's "port recorded in daemon.json".
  Localhost-only is enforced by CONSTRUCTION (the bind address is not
  configurable to a non-loopback value: `--http-bind` with anything non-local is
  refused at startup). Any real bind waits for auth (iroh-era).
- **Surface: `POST /rpc`, `Content-Type: application/cbor`.** Everything else is
  404/405. No JSON anywhere, no REST-shaped routes — the protocol is the taut
  message, HTTP is only a pipe (presentation belongs to the glade arc, not here).
- **Equivalence is the contract:** the ws-test stage round-trips the SAME hello
  over UDS and HTTP and asserts the response payloads are byte-identical.
