# RazelV3 Checkpoint 3 — Précis

*2026-06-11, at `razelV3/p4a-pool-sound` (round 27). Companion to `RazelV3Plan.md` (the plan of
record) and `RazelEvalParallelPlan.md` (the worker-pool plan). Successor snapshot to
`RazelV2Checkpoint1Precis.md` (whose per-round deltas carried the state through round 27 —
checkpoint "2" was the run-golden milestone, rounds 14–16, recorded there as deltas rather than
a standalone doc). **Round deltas append HERE from round 28.** This doc answers: what does the
TF tree-load yardstick say, what did the parallel-load work actually buy, and what falls next.*

## §1 Where we are (grounded inventory)

**Code:** ~27.4k LOC Rust across 17 crates (+2.4k integration-test LOC, +1.0k host/engine
`.bzl`), **61 green test bins**, 3 enforced gates (AD2 no-ambient-state · razel-dds boundary ·
C3c no-language-in-core), **6 must-pass probe sentinels**, **2 run-goldens**.

**Proven, beyond checkpoint 1's "validated architecture spike":**
- **Real things BUILD.** The cc run-golden compiles AND archives abseil's `log_severity`
  through real rules_cc `_cc_library_impl` + razel's host `cc_common.compile` /
  `create_linking_context_from_compilation_outputs` (real clang + libtool actions, outputs
  verified in an execroot; static lib-naming paid: `lib<name>.a`). The rust run-golden builds
  tinyjson (a real crates.io crate) through real rules_rust's full `construct_arguments` argv.
- **A real TF cc target analyzes end-to-end** (`//tensorflow/core/kernels/risc:risc`, sentinel
  #6) through tensorflow.bzl's macro layer, real rules_cc, protobuf's proto pipeline +
  cc_proto_aspect, skylib build settings, and the vendored llvm graph.
- **The TF full-tree load driver** (`xtask tfload`) sweeps all 835 packages under
  `tensorflow/` — the checkpoint-3 yardstick this doc is named for.
- **The eval worker pool exists and is SOUND** (§3) — opt-in via `RAZEL_LOAD_THREADS`.

**The honest capability statement:** razel loads 36% of TensorFlow's package surface and has
built one real target from each of two ecosystems (cc, rust). The remaining failure classes
are dominated by *resources* (unvendored repos) and *named, registered engine debts* — not
unknown unknowns. Nothing TF-shaped *runs* yet; analysis fidelity is the frontier.

## §2 The tree-load yardstick

**Coverage = of 835 packages under `tensorflow/`, how many load end-to-end**: BUILD eval
through the real macro layer, every declaration driven through analysis, dependency packages
demand-loaded recursively. Sequential, single-session, deterministic order.

**The curve so far** (sequential): unmeasurable (debug, killed at 14.5min) → 2:42 → 8:10
(deeper llvm walks opened) → 1:06 → 1:00 → **303/835 (36.3%) @ ~1:17 today** (the regression
from 1:00 is the round-24 honest-retry cost: failed packages re-eval per consumer; the
phase-tagged failure memo retires it — registered).

**Top failure classes at this checkpoint** (the ticket feed, sequential run):

| n | class | lever |
|---|---|---|
| 67 | `@curl//` not vendored | vendor decision (Gianni) |
| 37 | `@pypi//*` not vendored | posture proposal pending (host-materialized stub hub — round-25 delta) |
| 23+22+15+11 | `tensorflow_py` / `protos_all_py` / `constant_op` / `eager:context` "not analyzed" | py macro layer fidelity (one family) |
| 22 | `//tensorflow/lite:framework_experimental` not declared | lite macro layer |
| 21+11 | lite `schema_fbs` / `Label.repo_name` | Label surface (`repo_name` member) |
| 15 | highway `list + tuple` | eager-select composition — deferred-select debt, corpus named |
| 14 | `@stablehlo//` not vendored | vendor decision |
| 8 | TFRT `$(TFRT_MAX_TRACING_LEVEL)` | template-variable flow (`toolchains=` → `ctx.var`) — registered |

Calibration: the top two classes are pure resources (~104 pkgs); the py-macro family (~70) is
one engine investigation; everything else is a named debt with a fix shape already written
down. **~36% → ~55-60% is visible from here without new subsystem work.**

## §3 Parallel load (the P4a story — new at this checkpoint)

### What was built

P1–P3 (rounds 20–21) made the Session lockable and packages single-flight: 27 `RefCell` →
`SyncCell` (RwLock with the borrow API), `Send` bounds on native closures, per-package
`InFlight/Done` + condvar waits. P4 scaffolded the opt-in pool (`RAZEL_LOAD_THREADS=N`
workers over a shared queue against ONE Session). **P4a (round 27) made it sound.** Six
distinct concurrency bugs, each pinned by a failing repro before its fix — the first by the
new parity test bin (`parallel_parity.rs`: a heterogeneous 12-package fixture where any
cross-thread context bleed is a hard failure), the rest by it + TF-scale shakeout:

1. **Per-eval-stack state was Session-wide** (`current_pkg`, `current_bzl_repo`, the
   in-flight target, `analyzing`): concurrent workers clobbered each other's package context
   and every label misresolved — the round-23 "208ms anomaly" root cause. → ThreadId-keyed
   `EvalStack` on the Session, accessors only (the landed shape; not the sketched
   eval.extra `EvalCtx` — same isolation, no signature cascade).
2. **`.bzl` double-eval minted two provider identities** (`dep[P]` is ptr-eq) → per-module
   single-flight.
3. **Record-vs-harvest visibility**: a producer's `results` row is visible mid-eval but its
   captured provider instances harvest at package-eval END → wait-load the dep's package and
   re-read before demand re-analysis.
4. **`index_harvest` len-then-push under two lock acquisitions** mis-mapped harvest labels
   under concurrency → push+index under one lock.
5. **The 20s-timeout-only takeover livelocked at TF scale** (package cycles are dense; every
   edge cost a 20s sleep — presented as deadlock, 0% CPU) → a real **waits-for graph**:
   packages + `.bzl` modules in one keyed map with parked-worker edges; cycles detect at
   acquire. Package cycles resolve by **CycleProceed** (sequential re-entry semantics:
   proceed against the owner's partial state — exactly one partial reader per cycle, like
   sequential); `.bzl` cycles take the eval over (the module value is required).
6. **Early-`?` exits in `load_package_mode` leaked dead `InFlight` entries** (sequentially
   masked by re-entry semantics; under the pool, every unvendored-repo demand became a
   20s-quantum livelock for all waiters) → every owned begin now finishes.

### What it measures (full TF sweep, 12-core machine, fresh runs)

| threads | wall | speedup | cpu | coverage | takeover timeouts |
|---|---|---|---|---|---|
| 1 (default) | 1:17 | 1.0× | 99% | **303/835** | 0 |
| 2 | 56.5s | 1.37× | 130% | 299/835 | 0 |
| 4 | 48.1s | 1.61× | 158% | 296/835 | 0 |
| 8 | 34.0s | 2.28× | 327% | 287/835 | 0 |
| 12 | 30–40s | ~2.3× | 229–400% | 282–286/835 | 0 |

Stability: 11 consecutive clean threads=12 sweeps; parity fixture green 60+ consecutive runs;
threads=1 byte-identical to the pre-pool sequential path.

### The two findings in the curve

1. **The wall ceiling is the spine, not the machinery.** CPU utilization (1.3 cores busy at
   2 threads, 3.3 at 8) says workers park legitimately behind whoever owns the deep
   dependency chain (tensorflow root → core → llvm) — the plan's risk #3, measured. The next
   wall-clock lever is scheduling (waiters-work / driving the spine's independent
   sub-packages breadth-first), not more threads.
2. **Coverage degrades monotonically with concurrency** (303 → 299 → 296 → 287 → ~283): more
   threads = more concurrent package cycles = more `CycleProceed` partial-state reads at
   arbitrary timing (sequential's one-per-cycle partial reader sees a deterministic,
   usually-further-along state). Same walls, lost to timing — NOT corruption (failure-class
   diffing shows the same classes with shifted error shapes). The fix is **per-declaration
   demand futures** (the P3 `analyzing` InFlight treatment): a consumer waits on the specific
   declaration it needs instead of reading whatever exists.

### Posture

**The pool stays OPT-IN (default threads=1) until coverage parity** — the
`RazelEvalParallelPlan.md` acceptance bar (<10s with identical coverage) is unchanged.
Sequential coverage is the ratchet number. Next levers, in order: demand futures (closes the
coverage gap, retires duplicate demand-analysis waste), then scheduling (the wall tail).

## §4 Distance calibration (delta vs checkpoint 1)

Checkpoint 1's subsystem table said: configuration ❌, providers-flow partial, ctx 6 members,
nothing real built, loading 5–10% done. Today: `select()`/`config_setting` resolve against a
host config (modeled keys; unmodeled error loudly), providers flow cross-package by identity
(including under the pool), the ctx surface carries TF's macro layer, two ecosystems build
one real target each, and 36% of TF's packages load. Still ❌ and unmoved: repository rules
(L6a vendoring substitutes), transitions/aspects beyond absorb, execution at scale, runfiles.
The "10× the code that exists" calibration stands directionally — but the remaining surface
is now *enumerated* (the failure-class table + `RazelGaps.md`) rather than estimated.

## §5 Near-term (in order)

1. **Decisions (Gianni):** @curl + @stablehlo vendoring; @pypi stub-hub posture ack;
   apple_support (blocks the TF root entry via grpc).
2. **Demand futures** (engine, supervisor-grade): per-declaration InFlight on the analyzing
   path — closes the parallel coverage gap; prerequisite for pool-by-default.
3. **The py-macro "not analyzed" family** (~70 pkgs): one investigation, likely one fix class.
4. **Phase-tagged failure memo** (declare-phase = package-in-error, cacheable): retires the
   1:00 → 1:17 retry cost.
5. **Deferred select()** (highway corpus named): retires the eager-composition class.
6. **Scheduling** (waiters-work / spine breadth-first): the <10s wall push, after futures.
