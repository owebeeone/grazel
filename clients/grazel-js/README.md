# grazel-js — the IR-generated client (GR5a, the gryth attach point)

Generated from the SAME taut IR the daemons speak (`crates/razel-wire/wire/razel.taut.py`)
— the §4 promise: one IR, every language's client. Committed like
`razel-wire/src/generated.rs` (codegen out of the build graph). Regenerate:

```
PYTHONPATH=<taut checkout>/src python3 -m taut.cli gen -l js --with-runtime \
  -o /tmp/grazel-js-gen crates/razel-wire/wire/razel.taut.py
cp /tmp/grazel-js-gen/js/{api,cbor,client,server}.js clients/grazel-js/
```

- `api.js` + `cbor.js` — generated types/codec + vendored deterministic-CBOR
  runtime. `client.js`/`server.js` — generated stubs (unused by the smoke).
- `smoke.js` — HANDWRITTEN node client (node ≥22, built-ins only): hello over
  `POST /rpc`, `run` for an invocation id, follows `invocation.events` over WS,
  decoding every frame with the generated codec. Driven by the
  `js-client-roundtrip` ws-test stage (host-node posture: non-hermetic,
  version digest-logged by the stage).

## Why js, not ts (the GR5 wording)

taut's TYPESCRIPT backend currently emits types-only `api.ts` plus a client stub
importing `trial/ts/src/client.ts` — a runtime that does not exist in the taut
checkout (no TS cbor/codec emission; the `js` backend, by contrast, vendors
`cbor.js` via `--with-runtime`). Gryth's real TS client needs that runtime
completed taut-side (taut is Gianni's project — flagged to him directly, not an
RR seam). The wire contract proven here is identical either way: deterministic
CBOR from the same IR. Debt D15 in GrazelDebts.md.
