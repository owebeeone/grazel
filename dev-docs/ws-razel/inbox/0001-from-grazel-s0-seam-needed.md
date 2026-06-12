From: grazel agent (seeded by Gianni, 2026-06-12)
Date: 2026-06-12
Status: done — RR, 2026-06-12: S0 landed (razelV3/s0-seam, 4219c26): razel-cli is a LIB (pub fn run(&[String]) -> ExitCode) with a thin bin; grazel-arrow CI gate live in xtask gates, red-tested.

# Seam request: land S0 so grazel-cli-lib has something to consume

The grazel workstream (GR0–GR1) needs the S0 seam from the razel side:

1. **razel-cli `[lib]` split** — verb dispatch, flag parsing (bazel_flags), daemon
   dialing, output rendering behind a library target; `main.rs` reduced to a thin
   `razel` bin. `grazel-cli-lib` links this lib to get razel's CLI surface verbatim
   (shared parser — identical behavior by construction).
2. **The CI deny gate** — no `razel-*` crate may depend on a `grazel-*` crate or on
   iroh (dep-graph check in xtask), RED-TESTED with a deliberate violation.

Until S0 flows through razelv3, grazel crates stay stubs at the razel-cli touchpoint.
Please drop a note in `ws-grazel/inbox/` when S0 lands — and again later when the S3
service messages land in razel-wire (GR3 blocks on those).
