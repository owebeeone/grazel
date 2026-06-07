"""Razel daemon surface — the authored taut IR (the governed artifact).

The wire contract for the razel build daemon's UDS/WS API. Declarative only:
enums + messages with tags/types, and a service whose methods are
`(name, in, out, shape)` — the same model GripLab uses. This file is the single
source of truth; `../src/generated.rs` is produced from it by `tautc`
(`cargo xtask codegen`) and committed; nothing here is hand-edited downstream.

The contract deliberately stays the build engine's own surface and no further:

  - `build` / `sync_file` / `version` are the core request/response plane
    (faithful to `razel-daemon`'s Request/Response, minus the transport).
  - `affected` is the AI-agent dependency-graph query — "change these files →
    these targets / these tests" — riding the engine's rdep walk
    (`impacted_targets`).
  - `build.subscribe` is the live build-graph state pushed to clients (atom:
    whole-state, latest-wins).

What is *not* here, on purpose: file-*content* sync (SWMR byte ops). That is
GripLab's collaborative-editor surface built atop razel's file watching, not the
build engine's contract — it lives in GripLab's schema. razel surfaces file
*changes* inbound (`sync_file`) and build *staleness* outbound (`build.subscribe`),
never document bytes. Keeping that boundary is the whole point of the in/out/shape
contract: layer the proto where it belongs.

Authoring requires `taut` importable (the build tool: `pip install -e ../taut`,
or the published package). `cargo xtask codegen` sets that up; there is no
sys.path manipulation here on purpose — the env owns tool resolution.

Read whole: 2 enums, 8 messages, 1 service (5 methods).
"""

from taut.ir.dsl import BYTES, INT, STR, Enum, F, List, Msg, Ref, method, schema, service

SCHEMA = schema(
    # --- enums (integer wire values; native side is string-valued) ---
    # Mirrors razel-ir::TargetKind.
    Enum("TargetKind", library=0, binary=1, test=2),
    # Outcome of building a target: served from cache, freshly built, or failed.
    Enum("BuildStatus", cached=0, built=1, failed=2),

    # --- value messages ------------------------------------------------------
    # A produced output file and its content digest (blake3, raw bytes).
    Msg("OutputArtifact",
        F("path", 1, STR),
        F("digest", 2, BYTES),
        next_id=3),

    # A canonical target label + its kind. The unit the dep-graph query returns.
    Msg("TargetRef",
        F("label", 1, STR),                 # e.g. "//pkg/sub:lib"
        F("kind", 2, Ref("TargetKind")),
        next_id=3),

    # --- build (unary) -------------------------------------------------------
    Msg("BuildResult",
        F("target", 1, STR),
        F("status", 2, Ref("BuildStatus")),
        F("recomputes", 3, INT),            # engine nodes recomputed this build
        F("outputs", 4, List(Ref("OutputArtifact"))),
        F("message", 5, STR, optional=True),  # diagnostic / failure detail
        next_id=6),

    # --- sync_file (unary, control plane) ------------------------------------
    Msg("SyncAck",
        F("revision", 1, INT),              # engine revision after applying the edit
        next_id=2),

    # --- version (unary, dx) -------------------------------------------------
    Msg("VersionInfo",
        F("version", 1, STR),               # razel build version
        F("protocol", 2, INT),              # wire protocol revision
        next_id=3),

    # --- affected (unary, query) ---------------------------------------------
    # The AI-agent reverse query: given changed files, the impacted build graph.
    Msg("ImpactSet",
        F("sources", 1, List(STR)),         # the input files queried
        F("targets", 2, List(Ref("TargetRef"))),   # deliverables affected
        F("tests", 3, List(Ref("TargetRef"))),      # tests affected
        next_id=4),

    # --- build.subscribe (atom: whole build-graph state) ---------------------
    Msg("TargetStatus",
        F("label", 1, STR),
        F("kind", 2, Ref("TargetKind")),
        F("status", 3, Ref("BuildStatus")),
        F("output_digest", 4, BYTES),
        next_id=5),
    Msg("BuildState",
        F("revision", 1, INT),              # monotonic cursor; clients dedupe on it
        F("targets", 2, List(Ref("TargetStatus"))),
        next_id=3),

    # --- service: the razel daemon surface -----------------------------------
    service("Razel",
        # core request/response plane
        method("build", role="in",
               params=[("target", STR)], out=Ref("BuildResult")),
        method("sync_file", role="ctl",
               params=[("path", STR), ("digest", BYTES)], out=Ref("SyncAck")),
        method("version", role="dx",
               out=Ref("VersionInfo")),
        # AI-agent dependency-graph query (rdep walk)
        method("affected", role="query",
               params=[("files", List(STR))], out=Ref("ImpactSet")),
        # live build-graph state to clients (atom: latest-wins whole state)
        method("build.subscribe", role="out", shape="atom",
               out=Ref("BuildState")),
    ),
)
