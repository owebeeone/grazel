From: RG
Date: 2026-06-12
Status: done — RR, 2026-06-12: exported as requested — pub fn serve_conn<C: Read + Write>(&self, conn: &mut C) -> io::Result<()> on rpc::Server (reads the first frame, routes unary OR stream); transcript-tested over a socketpair with no listener. On razelv3.

# Seam request: export a per-connection serve entry on rpc::Server

WHAT: `rpc::Server::handle_conn` (and the stream methods it routes —
`stream_invocation_events`, `stream_build_state`) are PRIVATE. grazeld is a
multi-workspace daemon holding N in-process `rpc::Server`s (one per member
workspace, GR2); it accepts the connection itself, routes by scope/membership,
then needs to hand STREAM methods (`invocation.events`, `build.subscribe`) to the
right member's server. Unary is covered (`dispatch` is pub); streams are not.

Ask: ONE pub entry, e.g.
`pub fn serve_conn<C: Read + Write>(&self, conn: &mut C) -> io::Result<()>`
(handle_conn made pub under a contract-shaped name) — or pub
`stream_invocation_events`/`stream_build_state` if you'd rather keep the
read-first-frame part private; either works, I adapt.

WHY: GR3 (`grazel run` + `build-streamed`/`run-verb` stages) follows the
invocation log THROUGH the scope socket; without a stream entry the events log is
unreachable behind grazeld. This is the last blocker on GR3 completion — your S3c
covers everything else (0005, flipping it done in the same push as this note).

ACCEPTANCE: the export on razelv3 + one line in my inbox naming the signature;
my GR3b lands against it.
