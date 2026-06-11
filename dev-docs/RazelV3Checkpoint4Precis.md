# RazelV3 Checkpoint 4 — Précis

*2026-06-11, at `razelV3/native-dep-providers` (round 32, 59 razelV3 tags). Companion to
`RazelV3Plan.md` (plan of record) and `RazelEvalParallelPlan.md` (P1–P4a landed; scheduling
pending). Successor snapshot to `RazelV3Checkpoint3Precis.md`, which carried the deltas for
rounds 28–32 (the stabilization lane). **Round deltas append HERE from round 33.** This doc
answers: what did stabilization actually change, why the headline coverage number barely moved
while everything under it did, and what the next session should take first.*

## §1 Where we are (grounded inventory)

**Code:** ~27.5k LOC Rust across 17 crates (+~2.6k integration-test LOC, +1.0k host/engine
`.bzl`), **62 green test bins**, 3 enforced gates (AD2 · razel-dds boundary · C3c),
**6 must-pass probe sentinels**, **2 run-goldens** (cc compile+archive, rust compile), plus
two new permanent instruments: the **scheduler observation seam** (`sched_hook` /
`RAZEL_TRACE_LOAD`) and the **`xtask stress` harness** (baseline + N parallel sweeps; loud on
timeout / coverage-band / stall).

**Coverage: 307/835 TF packages (36.8%) @ ~6:09 sequential.** Checkpoint 3 closed at 303/835
@ 1:17 — read §2 before judging either drift; both numbers changed meaning this arc.

**The honest capability statement:** unchanged in kind from checkpoint 3 — razel loads a
third of TensorFlow's package surface and has built one real target in each of two
ecosystems. What changed in *quality*: the failure table is now believed-true (errors
un-muffled, failures memoized with real causes), the parallel pool is sound and
deterministically tested rather than demonstrated, and the remaining failure classes are
each named, sized, and owned. No unknown unknowns surfaced in five rounds of grinding — the
risk model continues to hold.

## §2 The yardstick, re-based — why 303→307 is not "four packages of progress"

Two corrections this arc changed what the curve measures:

**Errors were muffled** (round 29). Three silent paths — `let _ =` on dep-package loads,
"not analyzed" masking consumed-and-failed native bodies, retry-luck coverage from
non-deterministic failure order — meant the class table under-reported and the coverage
number over-flattered. Fixing them *cost* 2 packages (order-luck packages now fail
deterministically, Bazel-faithfully) and re-based the table: the true top classes were 70
and 53 packages, not the 15 visible before.

**The sweep got slower because it got DEEPER** (rounds 30–31). Frozen-depset uniformity
opened the proto/python pipelines; razel now does ~5× more real eval per sweep (1:16 →
6:09). The load trace proves it honest: max 5 re-evals for any key — no retry storm. The
wall-clock lever is the pool, not a bug hunt.

With that lens, the arc's real ledger: **three first-rank classes killed** — frozen-depset
non-uniformity (70 pkgs: silent `[]` from `to_list()`, silently skipped transitive members —
the worst silent-wrong class found to date), `Label.repo_name`/Bazel-7 aliases (35),
`native.package_relative_label` (62), and the protobuf_python native-dep provider class (72)
— each converting into its successor wall rather than into coverage, because the consumers
flow forward until the next gate. The conversion chain is the progress; the headline number
lags it by design.

## §3 What stabilization bought (new at this checkpoint)

- **The pool is sound, proven, and still opt-in.** P4a (per-thread EvalStack) plus six
  named race fixes (bzl single-flight, harvest visibility waits, single-lock index, the
  waits-for cycle graph with CycleProceed, last-finisher purge, InFlight-leak closure).
  threads=12: 0 timeouts, ~94% of sequential coverage, 2–2.6× wall. The residual gap is
  cycle partial-state reads + order-sensitive classes — *not* corruption. Acceptance bar
  (coverage parity) unchanged; demand futures are the named closer.
- **Determinism over demonstration.** The S2 seam scripts interleavings (barrier-forced
  collisions) so the P4a races have regression *tests*, not reproduction folklore. The S2
  gate helper itself got debugged this arc (2-party Barrier could hang when one worker took
  both entries → timeout rendezvous) — instruments earn the same rigor as engine code.
- **Failure semantics are now Bazel's.** `PkgState::Failed` caches declare-phase failures
  as "package in error" with the real cause; analysis-phase failures stay retryable
  (the cross-package harvest contract); consumed native bodies memo their error per
  declaration. One invocation, one verdict.
- **The native/Starlark provider seam closed generically** (round 32): `provider()` records
  declared fields; `dep[P]` on a capture-less native dep synthesizes an absorbing instance
  shaped by them, folded projections riding along — no language names in core, C3c intact.

## §4 Distance calibration (delta vs checkpoint 3)

Checkpoint 3's frame stands: resources + named engine debts, no algorithmic novelty. What
this arc adds to the calibration: **the un-muffling tax is paid** — future class counts can
be trusted, so prioritization is now data rather than triage instinct. The two systemic
debts called out at checkpoint 3 both grew corpora and shrank ambiguity: eager-select is
now precisely sized (**79 packages — the single biggest engine class**, grown by every
class-kill upstream of it) and the demand-futures gap has a stress harness waiting to
verify its closure. Wall-clock: sequential honest-depth cost (6:09) makes pool-by-default
the *perf* priority too, not just a parallelism trophy.

## §5 Near-term (in order)

1. **Eager-select / deferred composition (79 pkgs).** The biggest single lever on the
   board, corpus in hand (highway + the round-32 converts). Engine work in
   selects.rs/values.rs; test-first from the corpus shapes.
2. **Demand futures** (per-declaration InFlight on the analyzing path): closes parallel
   coverage parity → pool-by-default → the 6:09 collapses. Tighten the stress band to
   100% when it lands.
3. **schema_fbs make-var class (33)** + the vendor/decision queue (curl, apple_support,
   @pypi stub-hub posture) — the cheap-conveyor remainder.
4. **Scheduling** (waiters-work / spine breadth-first) only after futures — re-measure
   then; round 23's lesson (scheduling questions are moot over an unsound pool) now reads:
   scheduling questions are *premature* over a parity-gapped pool.
5. **Checkpoint 5 trigger:** pool-by-default at coverage parity, or coverage ≥45% —
   whichever lands first.

## Round delta — razelV3 round 33 (2026-06-11, stabilization lane)

**Demand futures are IN (`razelV3/reskey` → `native-single-flight` → `decl-futures` →
`restart-pass`; design: `RazelDemandFutures.md`) and the parallel coverage gap is CLOSED on
every package both orderings actually drive.** The four steps: (1) typed `ResKey{Pkg,Bzl,
Decl}` wait-graph keys retire the `:`-in-key discriminator (labels carry `:`). (2) Deferred-
native FnOnce demand-runs single-flight — the loser waits on the runner's record instead of
seeing an empty slot; run failures cache for waiters. (3) Pending-declaration PROXY futures:
a consumer demanding a mid-flight package's undriven declaration parks on `Decl(label)`
owned by the package owner; `record_target` publishes (the only cross-thread-visible
mid-eval completion — instances still wait for freeze, P4a bug #3), the package finish
SWEEPS stragglers (leak-proof), and the cycle walk is breaker-aware: park THROUGH a cycle
carrying a Pkg/Bzl wait edge (that waiter cycle-proceeds on wake — both orderings of the
classic A↔B dance now succeed), proceed-partial only on all-Decl cycles (a true cross-
thread deadlock shape). (4) The restart pass: a per-thread partial-read counter (cross-
thread-only by construction; threads=1 never advances it) marks entry loads that failed
after consuming partial state; the tree driver retries them single-threaded post-drain,
rounds until no progress — Skyframe's answer. Three new deterministic seam tests (7 bins'
worth of S2-gated schedules); parity fixture 10+ consecutive green; threads=12 stress runs
are now IDENTICAL run-to-run (26/26/26 sample-8) where round 27 wandered 282–287.

**Acceptance verdict: engine parity, metric residue.** Full tree, threads=12 vs sequential:
317 vs 321. Every diff package was hand-verified to FAIL STANDALONE-sequentially: seq 321 =
316 + 5 packages whose entry was a Ready no-op behind an earlier consumer's dep-load
(declarations deferred, never driven — the round-24 "load conflates declarations with
analysis" debt, now biting); par 317 = 316 + 1 such of its own. **Driven-work parity:
316 == 316, zero timing losses, zero corruption, 0 takeover-timeouts, 1–2 restarts/sweep.**
The 5 artifact packages sit on real registered walls (@curl/@gemmlowp/@stablehlo/@riegeli
unvendored; `ctx.executable` unwired). Stress band default: 99 on full tree, 90 sampled
(one artifact ≈ 4% of a 25-pkg baseline); the plan's 100 needs the per-target report
refinement or those vendors — **Gianni's call, twice over: (a) pool default-on at driven-
parity?, (b) metric ruling (per-target scoring / drive-Ready-entries) vs vendor grants.**
Pre-GENDIR full-stress wall: par 93.2s vs seq 377.5s = **4.05×** at parity coverage —
scheduling (<60s target) is next and now measurable honestly.

**The GENDIR/BINDIR genrule fix (`razelV3/genrule-gendir`) flipped +14 packages: sweep
307 → 321/835 (38.4%).** The schema_fbs_srcs make-var class (33) is dead (1 order-class
remnant). New visible walls behind it, with causal roots in the error text: the
tensorflow_py chain (23) roots at `@flatbuffers//:build_defs.bzl` lacking
`flatbuffer_py_library` — the vendored repo is RAW upstream; TF's repo rule `link_files`
TF's own overlay over it (round-25 precedent: "TF overlay pattern", but the overlay loads
`@build_bazel_rules_android` → needs a host shim first — round-34 candidate, sized). The
lite framework_experimental chain (26) roots at **@ruy unvendored**; python/dtypes (11) at
**@pypi//numpy** — vendor asks, NOT granted yet. Top classes now: eager-select srcs 79
(coverage lane item 1, unchanged), @curl 69, @pypi 37, lite/@ruy 26, tensorflow_py/
flatbuffers-overlay 23, @stablehlo 19, @FP16 18, highway list+tuple 15 (coverage lane).
Registered: round-32's absorbing synthesis silently serves cycle partial-state instance
reads (sequential-equivalent, value-divergent under timing; surfaced designing the F4 test
— RazelGaps round-33). 62 bins; 3 gates; 6 sentinels; 2 rungolds.
