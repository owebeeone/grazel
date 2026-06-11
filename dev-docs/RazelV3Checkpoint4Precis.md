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

## Round delta — razelV3 round 34 (2026-06-11, stabilization lane — scheduling groundwork)

**The scheduling lever is measured, its headroom proven, and its gate identified: seeding
waits on the coverage lane's deferred-select fix.** Post-round-33 parallel wall, threads=12:
**317-318/835 @ 98s, 564s user (5.75 avg cores busy** — round 27: 3.3); a 12s mid-run
sample shows the tail is 1-busy/11-parked on package waits — the spine ceiling, confirmed
post-futures. The round-23 spine-seeding infrastructure (`RAZEL_TFLOAD_SEED`, parked when
the pool was unsound) re-tested: **wall 98s → 25.1s (3.9×)** — the fan-out machinery works
— but coverage 317 → 195. Root-caused, not corruption: seeding entry-drives
`@xla//xla/tsl/platform` BEFORE `//xla/tsl:fuchsia` is declared; the hybrid eager-select
leaves an unresolvable `select_expr` that filegroup's typed `srcs` rejects at DECLARE →
`PkgState::Failed` caches the package-in-error → 277 consumers inherit it (~122 net
Ok→Fail). Unseeded ordering always declares the condition first (0 occurrences across both
full sweeps) — the eager-select class's order-sensitivity at spine scale, exactly the
round-27 wording. **Sequencing consequence: deferred-select (coverage lane, item 1) is now
also the scheduling lane's gate; re-run the seeding experiment when it lands — the <60s
target is plainly inside the 25s envelope.** No code this round; observations only.

## Round delta — razelV3 round 35 (2026-06-11, stabilization lane — fetch R1)

**The fetch arc opens (decision: Gianni — razel cannot be Bazel-compatible without fetch;
cache SIBLING to Bazel's, below the root byte-identical layout for `diff -r` comparison;
plan: `RazelFetchPlan.md`) and R1 LANDS: `xtask fetch-extract` evaluates TF's ENTIRE
WORKSPACE chain — `WORKSPACE` → `workspace3/2/1/0.bzl`, 100+ loads, the real `repo.bzl` —
with `repository_rule` bound to a RECORDER, and writes the lockfile:** 312 repo specs (285
unique; `maybe()` re-declares collapse), **196 sha-pinned, 39 patched, 113 with
build_file/link_files — every named census wall present (@curl/@ruy/@stablehlo/@FP16/
@riegeli/@gemmlowp), and @flatbuffers carries exactly the `link_files` overlay the hand
vendor missed** (the R3 materializer kills that bug class by construction). No network, no
repository_ctx: the recorder runs the REAL wrapper macros (mirror expansion, patch lists).

What it took (the probe loop, ~25 iterations): ~30 host-stub rows (WORKSPACE dep-inits:
bazel_tools repo rules http/git/java/local + utils, rules_python repositories/versions/pip,
cc-autoconf trio, workspace0's apple/swift/grpc/closure/pkg/foreign_cc inits, googleapis,
generated cuda/nccl/nvshmem/llvm redist `version.bzl`s via one shared no-CUDA stub);
`repository_rule` is now a UNIVERSAL .bzl global (Bazel-faithful — definition was never
WORKSPACE-scoped); `native.bazel_version = "7.4.5"` (skylib versions.check; consistent with
the all-True @bazel_features posture); `@//`/`@@//` main-repo load forms; and **Bazel-style
TAB indentation** (starlark-rust rejects tabs outright; leading-run expansion at every
external parse boundary — rules_ml_toolchain is tab-indented). Sequential sweep unchanged
(27/105 sample-8); 63 bins; 3 gates; 6 sentinels; 2 rungolds. Next: R2 fetcher + download
cache (sha-verified, mirror-first), R3 materializer + external-resolution second base
(vendored-first), R4 census re-base.

## Round delta — razelV3 round 36 (2026-06-11, stabilization lane — fetch R2+R3, bazel ground truth)

**Real Bazel ran on the tree (tools/bazel-7.7.0, the .bazelversion pin — Homebrew's 9.x
removed WORKSPACE support) and razel's fetch pipeline now materializes BYTE-IDENTICAL repo
trees: 5/5 bazel-comparable repos at ZERO content diffs** (curl, gemmlowp, FP16, stablehlo
incl. its patch, flatbuffers incl. the link_files overlay — `flatbuffer_py_library` is now
on disk; ruy + riegeli materialized razel-only). Ground truth confirmed the whole model:
`.bazelrc` says `--noenable_bzlmod --enable_workspace` (the WORKSPACE chain IS authoritative
— the 328-line MODULE.bazel is the opt-in `--config=bzlmod` future); output base =
`_bazel_<user>/<md5(workspace path)>` (hash scheme verified byte-equal); repository cache =
`cache/repos/v1/content_addressable/sha256/<sha>/file`, and **our lockfile's curl sha256 is
the exact content-address bazel cached**. The materialized BUILD.bazel == @xla's curl.BUILD.

**`xtask fetch <repo>…` (R2+R3 MVP, xtask-resident):** sha-pinned mirror-first download
(curl + shasum, write-rename) into `_razel_<user>` — SIBLING root, Bazel-identical layout
below (decision: Gianni) — then extract → strip_prefix → `patch -p1` (repo.bzl's exact
semantics, read from source) → build_file→BUILD.bazel + link_files. Two findings the
ground truth forced: (1) **bare label-string attrs on repository rules resolve against the
DEFINING module's repo** — FP16's `//third_party/FP16:FP16.BUILD` exists only in @xla's
tree; the spec's `kind` carries the defining module, so the resolver derives the root from
it; tf_vendored specs (@xla→third_party/xla) come from the lockfile itself. (2) **Bazel-core
plants zero-byte WORKSPACE/REPO.bazel boundary files INCONSISTENTLY per repo** (curl/FP16
both, gemmlowp one, stablehlo/flatbuffers none) — razel stays content-faithful and
fabricates none; comparisons ignore empty markers; the why is parked as an open question.
Also noted for the census: TF's default config carries `--deleted_packages` over the whole
tfrt family — our 835 denominator includes packages Bazel itself deletes. patch_cmds
(shell) refuse loudly. Lockfile relocated to `<workspace>/razel-lock.json` (Bazel's
MODULE.bazel.lock placement). Next: R4 — wire the materialized root into external
resolution (vendored-first) and re-base the census.

## Round delta — razelV3 round 37 (2026-06-11, stabilization lane — boundary parity)

**The round-36 open question is CLOSED with a proof, not a guess (directive: 100% bazel
compatibility — "figure out what it is"). Bazel-core's repo finalization, pinned by a
controlled 2-impls × 4-archive-shapes matrix on bazel-7.7.0 (file:// archives: plain /
WORKSPACE / WORKSPACE.bazel / MODULE.bazel, through bazel_tools' http_archive AND a
TF-style custom rule):** a repo ending its rule with NONE of {WORKSPACE, WORKSPACE.bazel,
MODULE.bazel, REPO.bazel} gets an empty WORKSPACE AND an empty REPO.bazel from core; ANY
one present — even zero-byte — suppresses both. The "anomaly" dissolving the scatter:
gemmlowp's upstream WORKSPACE is a 0-byte file in the archive. Second fidelity split read
from both sources: TF/XLA `_tf_http_archive` SYMLINKS build_file/link_files; bazel_tools
http_archive COPIES (7.7's workspace_and_buildfile writes BUILD.bazel only). The
materializer now implements both (kind-switched). **Strict acceptance: `diff -r -x
'*.marker'` — boundary files IN scope — is IDENTICAL on all 5 bazel-comparable repos.**
Neither MODULE.bazel nor .bazelrc is involved (version-locked core behavior); the .bazelrc
DOES gate other things (WORKSPACE-vs-bzlmod selection, deleted_packages) — rc consumption
stays the registered RazelGaps item, now with two live consumers. RazelFetchPlan §4b
records the proven rules. Next: R4 unchanged.

## Round delta — razelV3 round 38 (2026-06-11, stabilization lane — fetch R4)

**The fetched root is WIRED into resolution and the sweep hits its high-water mark:
321 → 353/835 (42.3%).** `GlobalFlags.fetched_external_base` (`_razel_<user>/<hash>/
external`) is a SECOND base behind hand-vendored `third-party/` — one helper
(`external_repo_dirs`: vendored-first with `_`→`-` tolerance, fetched exact-name second)
replaced five duplicated lookups (bzl loads, BUILD dirs, two file fallbacks, glob);
tfload/stress auto-wire it when materializations exist. Tests pin fetched-only resolution
AND vendored-wins precedence.

**The census conveyor, live across three sweeps:** wiring the 7 round-36 repos → 346
(@gemmlowp/@FP16/@riegeli/@stablehlo classes ERASED; @curl's 69 converted to @boringssl —
curl now actually evaluates; ruy converted to its own BUILD surface). Fetching the next
hops (boringssl, org_brotli) + RETIRING the hand-vendored flatbuffers → 325 first
(REGRESSION: upstream flatbuffers' BUILD uses Bazel-AUTOLOADED bare `java_library` + an
`@build_bazel_rules_android` load — the curated hand-vendor BUILD had been masking the gap;
lesson: retirement exposes autoload surface, and the curated copy was LOST uncommitted —
it was never tracked) → fixed properly: `java_library/java_binary/java_test` as RECORD-ONLY
placeholder BUILD globals (the target exists; real java analysis stays the java rung's) +
the rules_android host stub (round 33's predicted overlay prerequisite) → **353, with the
tensorflow_py chain (23) and lite framework_experimental chain GONE — the link_files
overlay serves.** Remaining top: eager-select 107 (coverage lane; grew with reach),
@jsoncpp_git 74 (curl hop 3), ruy BUILD class 43, @pypi 37+12, highway 28 (coverage lane),
@net_zstd 10 (riegeli hop 2). The conveyor is now: `xtask fetch <next wall>` + resweep.
64 bins; 3 gates; 6 sentinels; 2 rungolds.

## Round delta — razelV3 round 39 (2026-06-11, stabilization lane — conveyor wave 2)

**353 → 374/835 (44.8%).** Thirteen more repos materialized through `xtask fetch`
(jsoncpp_git, net_zstd, pybind11, googletest, eigen_archive, farmhash_archive, fft2d,
snappy, nasm, libjpeg_turbo, png, zlib, sobol_data) — the @jsoncpp_git 74-class,
@net_zstd 10 and @sobol_data 10 are dead/converted. The ruy class (46) is DIAGNOSED to its
root: ruy's `config_setting_group` references `@bazel_tools//src/conditions:windows_msvc`;
the host package now EXISTS (`host_build` row, adapted from the real bazel_tools with
razel-modelable constraint_values), but the group-member check in **selects.rs:230 errors
without demand-loading the member's package — coverage-lane file, theirs by coordination;
the fix is one demand-load in their select/condition rework and the 46 packages follow.**
New engine class for the stabilization queue: tensorflow/cc `$(location :ops/…)` genrule
lookup (23). Remaining top: eager-select 107 + highway 47 (coverage lane), ruy 46 (gated
on selects.rs), @pypi 37+12 (stub-hub posture), tfrt make-var 9. Wall 7:12 sequential —
deeper again; the pool default-on decision is worth ~5:30 of every sweep. 64 bins; 3
gates; 6 sentinels; 2 rungolds.

## Round delta — razelV3 round 40 (2026-06-11, stabilization lane — full select deferral)

**The eager-select hybrid is RETIRED (Gianni released the selects.rs hold; the coverage
lane never materialized — zero banks since round 27): `select()` NEVER resolves at load,
exactly Bazel's model. Sweep 374 → 385/835 (46.1%), and the entire select failure family
is DEAD: the 107-pkg typed-param class, highway's 47 (the "list+tuple" was the hybrid's
OWN artifact — Bazel rejects that op too but never sees it: `[..]+select(..)` stays a
select-expr; ground-truthed against bazel-7.7.0), and ruy's 46 (the eager probe errored on
an unloaded `config_setting_group` member — at analysis the member demand-loads into the
round-39 src/conditions host package).** Landed: always-defer select(); group members
DEFER on load-forbidden probes; tuple-tolerant select-expr flattening; filegroup ×2 →
str_attr_parts; genrule `cmd` string-selects via a scalar twin (`scalar_attr_parts` —
protobuf upb's `cmd = select(..)` regressed the cc sentinel mid-round, caught by probe,
fixed forward). The impl-time-select unit test rewritten to the attr form (Bazel-legal —
Bazel has no select in impls; the hybrid had made it accidentally work).

**New residue + hops (the conveyor's next plate):** native `deps=`-style attrs that
stringify deferred selects — "dep `select({…})` not analyzed" 36 + "select-expr" 10
(natives needing the str_attr_parts treatment on label attrs; MINE, round 41); @shardy 35
+ @cpuinfo 22+12 (vendor hops behind jit and ruy/lite — fetchable); @pypi 37+12 (posture);
tf/cc `$(location :ops/…)` 23; tfrt make-var 9. Wall 8:00 sequential — the default-on
pool decision now saves ~6:00/sweep. 64 bins; 3 gates; 6 sentinels; 2 rungolds.

## Round delta — razelV3 round 41 (2026-06-11, stabilization lane)

**385 → 393/835 (47.1%).** The deferral's own residue is dead: py natives
(py_library/py_binary/py_test) route srcs/deps through str_attr_parts (they had
stringified deferred selects into bogus dep labels — the 36-class), and str_attr_parts
flattens RECURSIVELY (function-composed selects nest exprs: `ruy_copts_warnings() +
ruy_copts_neon()` — the 10-class). @shardy + @cpuinfo materialized (the 35-class dead;
cpuinfo converts into its own analysis chain, 21). **New top engine class:** `NoneType has
no attribute basename` (33 — a ctx.file/File-shape gap, next plate); the python chains
keep converting inward (tensorflow_py 22 → constant_op 16 → eager:context 15 →
client:session 14 — the @pypi/numpy posture gates the cone). 64 bins; 3 gates; 6
sentinels; 2 rungolds. **Session curve: 307 → 393 (+86; 36.8% → 47.1%).**

## Round delta — razelV3 round 42 (2026-06-11, stabilization lane — two engine plates)

**393 → 397/835 (47.5%); both plates dead.** (1) `NoneType.basename` (33): typed
`attr.output`/`output_list` kwargs now materialize as FILE objects on
`ctx.outputs.<attr>` (@xla cc_embed_data iterates them; razel had only mapped loose
single-string kwargs). (2) `$(location :ops/…)` (23): Bazel's `$(location)` resolves
against the genrule's OWN outs too (tf_gen_op_wrapper_cc locates its outputs in cmd) —
outs join the location table and lookup is `:`-tolerant (`:name` == `name`). Converted
walls now visible: `@local_config_cuda//cuda:cuda_headers` not declared (36 — the no-CUDA
host BUILD needs the target rows) and a grpc `src/compiler` load-path hop (23). Python
cone unchanged (@pypi posture). 64 bins; 3 gates; 6 sentinels; 2 rungolds.
