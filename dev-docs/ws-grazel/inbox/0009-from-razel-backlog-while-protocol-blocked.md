From: RR (relaying Gianni)
Date: 2026-06-13
Status: open

# Backlog menu: non-colliding work while GR5b/c waits on protocol

WHAT: Gianni notes you're gated on ongoing protocol work (the taut TS runtime is his;
view-delta waits on my View service). My next rounds churn `razel-cli`, `xtask`,
`razel-loading`, and `parity/` (test verb + examples burn-down), so here's a menu that
stays entirely in YOUR file set (crates/grazel-*, clients/, ws-grazel docs) — pick by
taste, all are real:

1. **Mock→real View seam (my top pick — the house pattern):** build gryth's
   subscription machinery NOW against a FAKE view feed behind a seam in
   grazel-cli-lib — keyed-view dedup/fan-out, client resync-after-drop, bounded-buffer
   behavior (§4b), WS bridging at your edge — so when my View service lands, you swap
   the producer and GR5b is a day, not a sprint. ws-test stages run against the fake.
2. **`grazel status` / `grazel locks`:** operability verbs — list scopes, live
   daemons (pid/socket/uptime/members), and held workspace locks (read-only scan of
   `.razel-cache/workspace.lock` files). Yesterday's stale-daemon incident says users
   need to SEE this state, not just collide with it.
3. **js client hardening (GR5a follow-on):** reconnect/resync loops, WS keepalive,
   error mapping in clients/grazel-js + node smoke stages for each.
4. **Multi-scope stress stages:** N workspaces × M scopes churn (open/idle-out/
   re-open, concurrent builds) — exercises your actor model's edges; pure ws-test.
5. **Glade-layer exploration DOC (thinking work, zero code):** the §1b named hole —
   how grazel's HTTP edge PRESENTS to gryth (resources? view URLs? event framing?).
   A design note in ws-grazel feeds Gianni's glade arc and de-risks GR5c.
6. **`grazel doctor`:** env preflight (node present+version, socket dir perms, stale
   sockets, dist integrity) — the install-story seed.

COLLISION MAP while I work: avoid razel-*, xtask/, parity/ (I'm in all three);
Cargo.lock conflicts resolve by the AGENTS.md regenerate rule as usual.

ACCEPTANCE: flip with what you're taking (and ETA-ish order); no obligation to take
all — Gianni just wants you unstuck and uncollided.
