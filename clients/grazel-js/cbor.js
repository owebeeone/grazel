"use strict";
// Minimal deterministic CBOR — the JavaScript binding of the frozen wire
// substrate. Same tiny subset (int, bytes, text, array, int-keyed map, bool,
// null), core-deterministic (definite length, shortest-form ints, ascending map
// keys). Hand-rolled, no dependencies. Integers are JS numbers (safe to 2^53,
// like the TS codec); bytes are Uint8Array.

const INT = 0, BYTES = 1, TEXT = 2, ARR = 3, MAP = 4, BOOL = 5, NULL = 6;

const CInt = (n) => ({ kind: INT, i: n });
const CText = (s) => ({ kind: TEXT, s });
const CBytes = (b) => ({ kind: BYTES, b });
const CBool = (x) => ({ kind: BOOL, i: x ? 1 : 0 });
const CArr = (a) => ({ kind: ARR, arr: a });
const CMap = (m) => ({ kind: MAP, map: m }); // m: array of [key, Cbor]
const CNull = () => ({ kind: NULL });

function cget(c, key) {
  for (const [k, v] of c.map) if (k === key) return v;
  throw new Error("no map key " + key);
}
const cmapEntries = (c) => (c.kind === MAP ? c.map : []); // forward-compat residual
const isNull = (c) => c.kind === NULL;

function head(out, major, n) {
  const mt = major << 5;
  if (n < 24) out.push(mt | n);
  else if (n < 0x100) out.push(mt | 24, n & 0xff);
  else if (n < 0x10000) out.push(mt | 25, (n >> 8) & 0xff, n & 0xff);
  else if (n < 0x100000000) out.push(mt | 26, (n >>> 24) & 0xff, (n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff);
  else {
    const hi = Math.floor(n / 0x100000000), lo = n >>> 0;
    out.push(mt | 27, (hi >> 24) & 0xff, (hi >> 16) & 0xff, (hi >> 8) & 0xff, hi & 0xff,
      (lo >>> 24) & 0xff, (lo >> 16) & 0xff, (lo >> 8) & 0xff, lo & 0xff);
  }
}

function encode(c) {
  const out = [];
  enc(c, out);
  return Uint8Array.from(out);
}

function enc(c, out) {
  switch (c.kind) {
    case INT: if (c.i >= 0) head(out, 0, c.i); else head(out, 1, -1 - c.i); break;
    case BYTES: head(out, 2, c.b.length); for (const x of c.b) out.push(x); break;
    case TEXT: { const bb = new TextEncoder().encode(c.s); head(out, 3, bb.length); for (const x of bb) out.push(x); break; }
    case ARR: head(out, 4, c.arr.length); for (const x of c.arr) enc(x, out); break;
    case MAP: {
      const m = [...c.map].sort((a, b) => a[0] - b[0]); // ascending keys
      head(out, 5, m.length);
      for (const [k, v] of m) { head(out, 0, k); enc(v, out); }
      break;
    }
    case BOOL: out.push(c.i ? 0xf5 : 0xf4); break;
    case NULL: out.push(0xf6); break;
  }
}

function readArg(data, off, info) {
  if (info < 24) return [info, off];
  if (info === 24) return [data[off], off + 1];
  if (info === 25) return [(data[off] << 8) | data[off + 1], off + 2];
  if (info === 26) { let v = 0; for (let j = 0; j < 4; j++) v = v * 256 + data[off + j]; return [v, off + 4]; }
  let v = 0; for (let j = 0; j < 8; j++) v = v * 256 + data[off + j]; return [v, off + 8];
}

function decode(data) {
  const [v, off] = dec(data, 0);
  if (off !== data.length) throw new Error("trailing bytes after top-level CBOR item");
  return v;
}

function dec(data, off0) {
  const initial = data[off0], major = initial >> 5, info = initial & 0x1f;
  let off = off0 + 1;
  switch (major) {
    case 0: { const [n, o] = readArg(data, off, info); return [CInt(n), o]; }
    case 1: { const [n, o] = readArg(data, off, info); return [CInt(-1 - n), o]; }
    case 2: { const [n, o] = readArg(data, off, info); return [CBytes(data.slice(o, o + n)), o + n]; }
    case 3: { const [n, o] = readArg(data, off, info); return [CText(new TextDecoder().decode(data.slice(o, o + n))), o + n]; }
    case 4: { let [n, o] = readArg(data, off, info); const a = []; for (let i = 0; i < n; i++) { const [v, o2] = dec(data, o); a.push(v); o = o2; } return [CArr(a), o]; }
    case 5: { let [n, o] = readArg(data, off, info); const m = []; for (let i = 0; i < n; i++) { const [k, o2] = dec(data, o); const [v, o3] = dec(data, o2); m.push([k.i, v]); o = o3; } return [CMap(m), o]; }
    case 7:
      if (info === 20) return [CBool(false), off];
      if (info === 21) return [CBool(true), off];
      if (info === 22) return [CNull(), off];
  }
  throw new Error("unsupported CBOR item");
}

module.exports = { CInt, CText, CBytes, CBool, CArr, CMap, CNull, cget, cmapEntries, isNull, encode, decode };
