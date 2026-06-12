From: RG
Date: 2026-06-12
Status: open

# Gap (user-hit): the daemon build path can't load() AT ALL — no Evaluator loader

WHAT: Gianni ran `grazel build :all` in
`third-party/examples/rust-examples/01-hello-world` and got, THROUGH the daemon:

    daemon error: error: No imports are available, you tried
    `@rules_rust//rust:defs.bzl` (no call to `Evaluator.set_loader`)

rules_rust being unsupported is roadmap (track B, after java/go — fine). The part
that's a real seam gap: `rpc::Server::do_build` still runs the Phase-0
single-BUILD `analyze_build` with NO loader wired, while razel-LOCAL builds go
through `build_workspace_with`/`build_bazel_with` (the loader-capable paths). So
through razeld/grazeld, EVERY `load()` fails — including things razel-local
supports today. Since grazel routes build/affected through the scope daemon by
default (GR3, §1e posture), the daemon path is now the FRONT DOOR; its evaluator
lagging the local one breaks the §1d "byte-identical, daemon and local" claim the
moment a workspace has a load().

ASK: lift the daemon's do_build/run onto the same loading pipeline as the local
verbs (workspace loader + set_loader), or expose the loader-capable build entry
from razel-build so the daemon can ride it. My build-parity stage currently uses
a loader-free fixture — I'll extend it to a load()-bearing one as the red test
the moment you have the entry, if useful.

WHY: first real-workspace use through grazel hit it immediately; gryth's BUILD
files will load() from day one (@aspect shims), so this gates gryth-on-grazel.

ACCEPTANCE: a note naming the landed entry; my parity stage grows the
load()-bearing fixture and stays green.
