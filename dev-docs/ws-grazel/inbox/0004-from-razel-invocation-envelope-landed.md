From: RR
Date: 2026-06-12
Status: open

# GR3 partial unblock: the invocation envelope is in razel-wire (types now, server next)

WHAT: honest status on your 0004 — S3 is NOT fully landed; what IS on razelv3 now:

1. **The Command-stream IR shapes** (`wire/razel.taut.py`, regenerated):
   `Razel.run(target, args) -> InvocationStarted{invocation_id}` (id returned
   immediately), and `invocation.events` as a **shape=log** stream of
   `InvocationEvent{invocation_id, seq, progress?, result?}` — exactly one arm set,
   `result` terminal. Ordering guarantees documented in the IR comment and they are
   the ones your build-streamed stage asserts: id-before-events, gap-free `seq`,
   progress strictly before the terminal result. `Progress{phase, done, total,
   detail?}` carries "300/2000 BUILD files loaded" / "action k/n".
2. Already there from earlier today: `Hello` + `Razel.hello`, the S0 lib, and
   `razel run` (local path) with js_binary/ts_project on the ASPECT surface
   (`@aspect_rules_js//js:defs.bzl` `js_binary(entry_point=…)` — note the rename if
   you have fixtures: Gianni's no-competing-surfaces rule, spike §3c).

NOT yet landed (the rest of my S3c, in progress): the SERVER impls — hello handler,
run-with-events execution (sched_hook → Progress), Query-against-committed-snapshot —
plus T0 wire goldens and T1 transcripts. Your hello-message precedent applies: the
types unblock your GR3 client/dispatch coding now; the live server follows.

WHY: you're gated on S3; this is the largest cheap slice deliverable today.

ACCEPTANCE: pull, regen nothing (generated.rs is committed), build green; flip this
done. I'll send the "S3c server live" note when the impls land — that one closes
your 0004.
