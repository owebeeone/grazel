#!/usr/bin/env python3
"""Generate the razel-wire golden corpus → `../src/vectors.rs`.

The corpus is the cross-language contract: reference values encoded by taut's
*Python* codec, committed as hex, that razel's *Rust* codec must reproduce
byte-for-byte (parity == correctness — taut's whole conformance thesis). This is
the producer side; the Rust consumer is the `corpus_byte_parity` test in lib.rs.

Run via `cargo xtask corpus [--check]` (which sets up `taut` on PYTHONPATH), or
directly: `PYTHONPATH=../../../../taut/src python3 corpus.py [--check]`.

Native value form (what the codec takes): enums as their member-name *string*,
messages as field-name dicts, bytes as `bytes`, optional-absent as `None`.
"""

from __future__ import annotations

import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent          # crates/razel-wire/wire
SCHEMA_PATH = HERE / "razel.taut.py"
OUT_PATH = HERE.parent / "src" / "vectors.rs"

from taut.ir.load import load_schema             # noqa: E402  (after sys.path setup by caller)
from taut.wire import codec                      # noqa: E402

# (name, message, native-value) — exercises enums, lists, nested messages,
# bytes, and optional present/absent across every message in the contract.
REFERENCE: list[tuple[str, str, dict]] = [
    ("output/lib", "OutputArtifact",
     {"path": "out/libmath.a", "digest": bytes([0xde, 0xad, 0xbe, 0xef])}),
    ("targetref/binary", "TargetRef",
     {"label": "//app:bin", "kind": "binary"}),
    ("build/success", "BuildResult",
     {"target": "//pkg:lib", "status": "built", "recomputes": 7,
      "outputs": [{"path": "libpkg.a", "digest": bytes([1, 2, 3])}],
      "message": None}),
    ("build/failure", "BuildResult",
     {"target": "//x:y", "status": "failed", "recomputes": 0,
      "outputs": [], "message": "link error: undefined symbol `add`"}),
    ("sync/ack", "SyncAck", {"revision": 42}),
    ("version/info", "VersionInfo", {"version": "0.1.0", "protocol": 1}),
    ("impact/set", "ImpactSet",
     {"sources": ["src/a.cc", "src/b.h"],
      "targets": [{"label": "//app:bin", "kind": "binary"}],
      "tests": [{"label": "//app:unit_test", "kind": "test"},
                {"label": "//lib:lib_test", "kind": "test"}]}),
    ("targetstatus/cached", "TargetStatus",
     {"label": "//l:lib", "kind": "library", "status": "cached",
      "output_digest": bytes([0x00, 0xff])}),
    ("buildstate/one", "BuildState",
     {"revision": 3,
      "targets": [{"label": "//l:lib", "kind": "library", "status": "built",
                   "output_digest": bytes()}]}),
]


def emit() -> str:
    schema = load_schema(SCHEMA_PATH)
    lines = [
        "// GENERATED golden corpus (taut Python codec) — do not edit.",
        "// Regenerate with `cargo xtask corpus`. Each entry is (name, message,",
        "// cbor-hex): the bytes razel's Rust codec must reproduce exactly.",
        "pub static VECTORS: &[(&str, &str, &str)] = &[",
    ]
    for name, message, value in REFERENCE:
        # names/messages/hex contain no quotes or backslashes — plain literals.
        hexbytes = codec.encode(schema, message, value).hex()
        lines.append(f'    ("{name}", "{message}", "{hexbytes}"),')
    lines.append("];")
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    fresh = emit()
    if "--stdout" in argv:
        # Raw (un-rustfmt'd) source to stdout — `cargo xtask corpus` formats +
        # writes/drift-checks it (keeping committed vectors.rs fmt-stable).
        sys.stdout.write(fresh)
        return 0
    OUT_PATH.write_text(fresh)
    print(f"corpus: wrote {OUT_PATH} ({len(REFERENCE)} vectors)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
