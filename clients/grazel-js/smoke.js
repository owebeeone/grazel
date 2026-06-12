// grazel-js smoke client (GR5a) — the gryth attach point, proven from node:
// the IR-GENERATED codec decodes the LIVE grazeld over the HTTP/WS edge.
//
// usage: node smoke.js <http_port> <workspace_root>
// prints `hello-ok …`, `run-ok …`, `events-ok …`, `smoke-ok` — the ws-test
// stage asserts these lines. Exits nonzero on any contract violation.
//
// Host-node posture (GrazelWorkstream GR5): non-hermetic, digest-logged by the
// stage. Uses only node built-ins (fetch, WebSocket — node ≥22).

"use strict";
const api = require("./api.js");
const { encode, decode, CText, CMap, CArr, cget } = require("./cbor.js");

const [port, workspaceRoot] = [Number(process.argv[2]), process.argv[3]];
if (!port || !workspaceRoot) {
  console.error("usage: node smoke.js <http_port> <workspace_root>");
  process.exit(64);
}

// The razel-daemon envelope: request {1: method, 2: args}, response
// {1: ok, 2: payload, 3: error} — same bytes on every transport (GR4).
function req(method, args) {
  const m = [[1, CText(method)]];
  if (args !== undefined) m.push([2, args]);
  return CMap(m);
}

function payload(respBytes) {
  const c = decode(new Uint8Array(respBytes));
  if (cget(c, 1).i !== 1) throw new Error(`daemon error: ${cget(c, 3).s}`); // CBool carries .i
  return cget(c, 2);
}

async function rpc(method, args) {
  const r = await fetch(`http://127.0.0.1:${port}/rpc`, {
    method: "POST",
    headers: { "Content-Type": "application/cbor" },
    body: encode(req(method, args)),
  });
  if (r.status !== 200) throw new Error(`HTTP ${r.status} from /rpc`);
  return payload(await r.arrayBuffer());
}

async function main() {
  // 1. hello — version + protocol from the generated VersionInfo decoder.
  const hello = new api.Hello({ build_version: "smoke", protocol: 1, workspace_root: workspaceRoot });
  const v = api.VersionInfo.fromCbor(await rpc("hello", hello.toCbor()));
  if (v.protocol !== 1) throw new Error(`wire protocol ${v.protocol}, want 1`);
  console.log(`hello-ok version=${v.version} protocol=${v.protocol}`);

  // 2. run — invocation id arrives IMMEDIATELY (§4b id-first).
  const started = api.InvocationStarted.fromCbor(
    await rpc("run", CMap([[1, CText("hello")], [2, CArr([])]]))
  );
  if (!started.invocation_id) throw new Error("no invocation id");
  console.log(`run-ok id=${started.invocation_id}`);

  // 3. follow invocation.events over WS until OUR terminal event; decode every
  //    frame with the generated InvocationEvent codec (gap-free, 1-based seq).
  const ws = new WebSocket(`ws://127.0.0.1:${port}/events`);
  ws.binaryType = "arraybuffer";
  const events = await new Promise((resolve, reject) => {
    let count = 0;
    let nextSeq = 1;
    ws.onopen = () => ws.send(encode(req("invocation.events")));
    ws.onerror = (e) => reject(new Error(`ws: ${e.message || "error"}`));
    ws.onmessage = (m) => {
      try {
        const ev = api.InvocationEvent.fromCbor(payload(m.data));
        if (ev.invocation_id !== started.invocation_id) return;
        if (ev.seq !== nextSeq) throw new Error(`seq gap: got ${ev.seq} want ${nextSeq}`);
        nextSeq += 1;
        count += 1;
        if (ev.result !== null && ev.result !== undefined) {
          ws.close();
          resolve(count);
        }
      } catch (e) {
        reject(e);
      }
    };
  });
  console.log(`events-ok n=${events}`);
  console.log("smoke-ok");
  process.exit(0);
}

main().catch((e) => {
  console.error(`smoke-FAIL: ${e.message}`);
  process.exit(1);
});
