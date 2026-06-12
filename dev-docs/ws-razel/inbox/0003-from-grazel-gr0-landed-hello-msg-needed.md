From: RG
Date: 2026-06-12
Status: open

# GR0 landed (FYI) + seam request: a Hello message in razel-wire

WHAT (two items):

1. **FYI, no action:** GR0 + the GR1 UDS slice are on razelv3 (tag `grazel/GR0`,
   553fdd3). Crate design: `dev-docs/ws-grazel/GrazelCrates.md`. Crates:
   `grazel-cli` (thin bin) / `grazel-cli-lib` (scope chain `--scope` > `GRAZEL_SCOPE`
   > `.grazelrc service_scope` > `default`; sockets `~/.grazel/.uds/<scope>`, state
   `~/.grazel/scopes/<scope>/daemon.json`) / `grazel-node` (placeholder). grazeld
   rides the razel-daemon LIB (allowed direction, zero razel-* diffs); `grazel ws test`
   runs a 4-stage e2e ladder incl. two concurrent scope daemons answering hello.
   New shared-file note: Cargo.lock gained the three crates — regenerate on conflict
   per AGENTS.md.

2. **Seam request (blocks GR1 completion, not GR1 start):** the §1e hello carries
   the WORKSPACE ROOT plus build + wire versions; today I ride the existing `version`
   method (VersionInfo: version, protocol) as hello v0. Please grow the taut IR
   (`wire/razel.taut.py`) with a Hello message — suggested shape:
   `Hello { build_version: str, protocol: int, workspace_root: str }` request +
   response carrying the daemon's versions (server impl can land with your S3 service
   skeleton; the message alone unblocks me earlier if cheap to emit).

WHY: GR1's `version-handshake` and `scope-routing` (GR2) stages need the workspace
root in the hello — the daemon must discriminate workspace handles (§1e dial
procedure).

ACCEPTANCE: a razelv3 note in ws-grazel/inbox/ saying the Hello message is in
razel-wire's generated types (name + fields as landed); `cargo xtask codegen --check`
green. S0 remains separately open per inbox 0001.
