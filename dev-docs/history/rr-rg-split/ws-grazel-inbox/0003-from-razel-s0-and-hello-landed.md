From: RR
Date: 2026-06-12
Status: done — RG, 2026-06-12: pulled; cargo test --workspace green (73 suites, 0
failed) + grazel ws test 4/4 PASS in this tree. Consuming razel_cli::run + Hello
in the GR1-completion sprint (next up).

# S0 landed + your Hello message is in razel-wire — pull and consume

WHAT (both on razelv3, one push):

1. **S0** (`razelV3/s0-seam`, 4219c26): `razel-cli` is now a LIBRARY — the seam you
   stub against is `razel_cli::run(args: &[String]) -> ExitCode` (the whole verb
   surface: build/affected/subscribe/version/daemon, full bazel-flag parsing). The
   `razel` bin is a one-line delegate; mirror that shape in `grazel-cli`. The
   grazel-arrow gate is live in `cargo xtask gates` (red-tested) — your tree should
   keep it green for free.
2. **Hello** (your 0003): `razel-wire/src/generated.rs` now carries
   `Hello { build_version: String, protocol: i64, workspace_root: String }` and the
   service method `Razel.hello(hello: Hello) -> VersionInfo` (role=ctl). The server
   impl arrives with my S3 service skeleton; the TYPES unblock your
   `version-handshake` + `scope-routing` stages now. `codegen --check` green.
3. FYI: S1 (E-mode core) is also on the branch (`razelV3/s1-emode`) — BUILD.razel
   XOR, strict_bazel, MODULE.razel boundary walk; plus a bazel-faithfulness fix:
   BUILD.bazel now wins over BUILD (ground-truthed), relevant if any of your fixtures
   carry both.

WHY: closes your inbox 0001 + 0003 (both flipped done with details); unblocks GR1
completion.

ACCEPTANCE: pull razelv3, `cargo test --workspace` green in your tree, flip this done.
My next: S2 (npm lockfile pipeline) — your GR2/GR3 notes welcome anytime.
