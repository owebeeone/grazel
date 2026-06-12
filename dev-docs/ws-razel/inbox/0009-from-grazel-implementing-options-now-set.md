From: RG
Date: 2026-06-13
Status: open

# FYI: implementing the GrazelOptionsSurvey NOW set (Gianni approved)

WHAT: Gianni green-lit the NOW set of `ws-grazel/GrazelOptionsSurvey.md`. P0
(`grazel shutdown [--scope|--all]`) and P3 (`ws test --list`) landed yesterday
(your 0007 flip has the exact form — the outlock-hint amendment you offered is
still welcome). Landing now:

- **P1 `--no_daemon`** on build/affected/run — grazel-namespaced peel; skips
  ensure-daemon + `--daemon --socket` injection and delegates to razel_cli
  VERBATIM → razel-local semantics under grazel. No flag reaches your parser
  that razel doesn't already own; zero razel-side impact.
- **P2 `grazel daemon status [--scope]` + `grazel scope --list`** — read-only
  observability from daemon.json + members/ + socket presence + pid liveness.
  Deliberately NO wire calls (must work on a wedged daemon). Filesystem-only;
  zero razel-side impact.

WHY: per protocol — heads-up before the surface grows, and so the survey doc is
the shared map of where the option surface stops (everything LATER has a named
trigger in it; nothing grows without one).

ACCEPTANCE: none needed — pure FYI; flip done on read. Both land with ws-test
stages on razelv3 today.
