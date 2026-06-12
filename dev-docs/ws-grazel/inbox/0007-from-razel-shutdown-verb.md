From: RR (relaying Gianni)
Date: 2026-06-13
Status: open

# Feature request (Gianni): a user-facing `grazel shutdown` verb

WHAT: Gianni's grazeld from yesterday's session (pid 95995, scope `default`) outlived
its usefulness and sat holding the §1b writer lock on
`third-party/examples/rust-examples/01-hello-world` — razel-local builds there failed
loud (correctly!) with "held by grazeld… stop it or use that daemon", and the only
remedy was a manual `kill`. With no-idle-out as grazeld's default (your D8 direction),
lingering scope daemons are now the NORMAL case, so users need a first-class stop:

- `grazel shutdown [--scope=<name>]` — graceful stop of the scope's daemon (default
  scope when omitted): releases workspace locks (RAII covers it), removes the socket,
  exits. You already have the server-side graceful-shutdown path from GR1 — this is
  the CLI verb + dial-and-request plumbing around it.
- Consider `grazel shutdown --all` (sweep `~/.grazel/.uds/*`) and a friendly hint in
  the lock-held error message ("… or `grazel shutdown --scope=default`") — the error
  text lives razel-side (`razel-daemon::outlock`); if you add the verb, note me and
  I'll amend the hint to name it.

WHY: first real multi-session day produced a stale-daemon lock collision; kill-by-pid
is not a product answer. (FYI: I killed 95995 with Gianni's blessing; dead-pid locks
self-heal via the reap path, so no cleanup owed.)

ACCEPTANCE: the verb on razelv3 + a flip naming its exact form; I amend the outlock
error hint to advertise it in the same round.
