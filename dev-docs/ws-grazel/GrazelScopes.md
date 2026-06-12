# GrazelScopes — GR2 design: service scopes for real

*2026-06-12, RG. The decisions GR2 implements, recorded before code (constraints:
PublicSurfaces §1b/§1e, GrazelWorkstream GR2). GrazelCrates.md is the crate-level
design; this is the scope-daemon behavior level.*

## Multi-workspace grazeld (the §1 handle model, scope-sized)

A scope daemon serves a workspace COLLECTION. grazeld holds a **member map**
`root → handle` where a handle owns a `razel_daemon::rpc::Server` bound to that root.
Members arrive two ways (§1e "use both"):

- **Pinned** — `~/.grazel/scopes/<scope>/scope.rc`, lines `pinned=<abs path>`
  (rc syntax, grazel-owned). Opened at daemon start; failure to claim a pinned
  workspace is a loud STARTUP error. Pinned members never idle out.
- **Dynamic** — a hello whose `workspace_root` isn't a member opens one (refcount =
  liveness via `last_used`; a sweep closes dynamic members idle past
  `--member-idle-timeout`, default 30min). The DAEMON does not idle out (Gianni:
  grazeld runs indefinitely); only memberships do.

**Member open order:** hello protocol check FIRST (a mismatched client must not cause
lock churn), then the output-base lock, then the handle. Lock failure → taut error
frame naming the holder.

## Output bases & the cross-daemon lock (§1b made concrete)

- Each member's engine state lives in **the workspace's own** `.razel-cache/` — the
  same dir razel-local uses, BECAUSE §1b: output bases are not keyed by distribution
  or scope; switching a workspace razel↔grazel must not rebuild the world.
- **The lock:** `<workspace>/.razel-cache/workspace.lock`, created `create_new`,
  content one JSON line `{"pid":N,"daemon":"grazeld","scope":"<name>"}`. Conflict →
  if the recorded pid is dead, reap and retake; alive → error
  `workspace <root> is held by grazeld scope="a" (pid N)`. Released on member close
  and on daemon cleanup (shutdown/idle paths share one exit).
- This file IS the §1b cross-daemon single-writer contract as proposed to the razel
  lane (inbox note; razeld mirrors it when daemon mode lands with S3). Until razeld
  honors it, the lock arbitrates grazeld↔grazeld only — stated, not pretended.

## Routing (interim, until GR3)

The wire's one-request-per-connection shape stays. Non-hello requests route to the
daemon's SOLE member; zero or many members → taut error ("hello first / ambiguous").
GR3's invocation envelope (S3 service messages) carries workspace identity properly —
this interim rule is debt D9, retired there.

## Observability: membership as files

`~/.grazel/scopes/<scope>/members/<digest16-of-root>` containing `<kind> <root>`
(kind = pinned|dynamic). Daemon-owned filesystem artifacts (like daemon.json), NOT
protocol — ws-test stages assert membership without inventing wire messages ahead of
the razel-side IR (taut-everywhere holds).

## `.razelrc` policing (§1e, the config arrow)

Grazel keys in `.razelrc` are an ERROR (razel must never become grazel-aware). The
grazel key set today: `service_scope`. Enforced at scope resolution: a `.razelrc`
in the workspace containing `service_scope=` fails loud, pointing at `.grazelrc`.

## ws-test stages added (GR2 ladder rungs)

`grazel-key-in-razelrc-errors` · `scope-routing` (rc-bound workspace's hello lands its
member in the RIGHT scope's state) · `pinned-membership` · `dynamic-membership-idle-out`
(member closes, daemon survives) · `double-claim-fails-loud` (second scope dialing the
same workspace is refused, holder named) · `two-scopes-concurrent` (two fixture
workspaces, two daemons, real `build` wire calls racing — both Built; the GR2 exit).
`two-scopes-hello` reworked: one scope per workspace means scopes a/b get DISTINCT
workspace dirs (the §1e rule, now enforced by the lock).
