From: RR (current session)
Date: 2026-06-13
Status: done — RR, 2026-06-13: oriented. Per Gianni the V2 engine slice is HELD for his
go/no-go — this session did NOT start the message-API slice; the design doc
(RazelDepsEngineV2.md, added with this flip) is preserved for that decision. Landed instead:
banked the in-flight depset O(N×M)→BTreeSet perf + provider-salvage fix (aa95fe2, on share) and
closed the open coordination note 0011-from-grazel — `run` rides the loader (pinned by a daemon
transcript test), bare-name `razel build` finds BUILD.razel via the canonical resolve_build_file.
Suite 82 ok; gates+probe green. V2 awaits Gianni's go/no-go.

# RR -> RR: tfload regression and DepsEngine V2 handoff

You are RR in `/Users/owebeeone/limbo/glial-dev/razel`. This is a self-handoff for
the next RR session. Do not assume the worktree is clean; it is intentionally dirty.

## Latest user intent

Gianni agrees the loader/eval problem is structural. The requested direction is:

- wrap the current loader in a new interface,
- create a new message-driven graph engine behind the same interface,
- run legacy/new/compare modes until the new engine is correct,
- then swap and retire the old path.

The immediate user question was: "what exactly is the minimal API - is versioning
an issue?" I answered by creating the design doc:

- `dev-docs/RazelDepsEngineV2.md`

The headline answer in that doc:

- Minimal API = one command ingress, one typed event egress, and immutable snapshot
  ids for all readable results.
- Versioning is mandatory: API schema version, workspace epoch, committed
  `SnapshotId`, and engine/cache ABI digest.
- The first implementation milestone should be a legacy adapter implementing the
  message API before any new graph behavior lands.

## Current worktree state

At handoff, `git status --short` shows:

```text
 M crates/razel-loading/src/decls.rs
 M crates/razel-loading/src/dialect.rs
 M crates/razel-loading/src/glob.rs
 M crates/razel-loading/src/lib.rs
 M crates/razel-loading/src/provider_values.rs
 M crates/razel-loading/src/rules.rs
 M crates/razel-loading/src/selects.rs
 M crates/razel-loading/src/state.rs
 M crates/razel-loading/tests/args_fidelity.rs
 M crates/razel-loading/tests/sched_seam.rs
 M xtask/src/tfload.rs
?? crates/razel-loading/tests/aspects.rs
?? crates/razel-loading/tests/demand_characterization.rs
?? crates/razel-loading/tests/glob.rs
?? dev-docs/RazelDepsEngineV2.md
```

Do not revert these. The loader/test changes predate the design-doc step in this
handoff and are part of the tfload investigation/fixes. The only file added by the
final design-doc request was `dev-docs/RazelDepsEngineV2.md`; this handoff note is
the second new doc file.

## What happened in this session

### 1. Wedged tfload was characterized

The original "wedged" TF load was not a deadlock. At about 20 minutes it was still
using CPU and huge memory. Process inspection showed a hot O(NxM)-shaped path:

```text
dialect::depset()
  file_path(v)
  seen.contains(&key) over Vec<String>
  String/Vec equality dominated samples
```

The first fix changed depset dedupe in `crates/razel-loading/src/dialect.rs` from
linear `Vec::contains` to set-backed insertion while preserving ordered output.
Diagnostics were added so `tfload` can aggregate depset shape.

Relevant knobs:

```text
RAZEL_TFLOAD_DIAG_DEPSET=1
RAZEL_TFLOAD_DIAG_PROVIDER_REANALYZE=1
RAZEL_TFLOAD_SAMPLE=<N>
RAZEL_LOAD_THREADS=<N>
```

Focused verification already run after the depset fix:

```text
cargo test -p razel-loading depset_diagnostic_reports_dedupe_shape -- --nocapture
cargo test -p xtask depset_diag_aggregates_shape_events -- --nocapture
```

Earlier full package checks also passed:

```text
cargo test -p razel-loading
cargo test -p xtask
```

Warnings were pre-existing unused imports in loader modules and
`xtask/src/fetchpypi.rs`.

### 2. Post-fix tfload numbers

Post-fix sample-256 on 1 thread:

```text
RAZEL_TFLOAD_SAMPLE=256 RAZEL_LOAD_THREADS=1 \
RAZEL_TFLOAD_DIAG_DEPSET=1 RAZEL_TFLOAD_DIAG_PROVIDER_REANALYZE=1 \
gtimeout 240 cargo run -q -p xtask -- tfload

phases: parallel read+parse 2ms (...), eval 187171ms
provider-reanalyze: 0 fallback(s) across 0 label(s)
depset: 58058 call(s), input 11487282 item(s), unique 5373587 item(s),
duplicate 6113695 item(s), direct 7927853 item(s), transitive 23011 depset(s),
max input 274837, max seen 6595
```

Post-fix sample-256 on 6 threads:

```text
RAZEL_TFLOAD_SAMPLE=256 RAZEL_LOAD_THREADS=6 \
RAZEL_TFLOAD_DIAG_DEPSET=1 RAZEL_TFLOAD_DIAG_PROVIDER_REANALYZE=1 \
gtimeout 480 cargo run -q -p xtask -- tfload

phases: parallel read+parse 2ms (12 threads), eval 184906ms
provider-reanalyze: 244 fallback(s) across 225 label(s)
depset: 87575 call(s), input 13259007 item(s), unique 6047499 item(s),
duplicate 7211508 item(s), direct 9258043 item(s), transitive 36138 depset(s),
max input 274837, max seen 6595
tfload: 0/4 packages load
```

The 6-thread run had essentially no wall-clock speedup versus 1 thread. Process
samples showed the main thread parked and only one active worker around the
recursive path:

```text
load_package_entry -> load_package_body -> resolve_label_attr_inner -> analyze_deferred
```

Conclusion: the remaining bottleneck is not the old `glob()` issue and not the
fixed `Vec::contains` depset bug. It is structural:

- recursive demand eval hides runnable graph work inside one worker's stack,
- depsets are still eagerly materialized too often,
- 6-thread mode also reintroduces provider reanalysis fallbacks.

### 3. Bazel source inspection

Bazel source used for comparison:

```text
/Users/owebeeone/limbo/bazel-dev/bazel
```

Key local source references:

- `src/main/java/com/google/devtools/build/skyframe/SkyFunction.java`
- `src/main/java/com/google/devtools/build/skyframe/AbstractParallelEvaluator.java`
- `src/main/java/com/google/devtools/build/skyframe/NodeEntry.java`
- `src/main/java/com/google/devtools/build/skyframe/IncrementalInMemoryNodeEntry.java`
- `src/main/java/com/google/devtools/build/skyframe/InMemoryGraphImpl.java`
- `src/main/java/com/google/devtools/build/lib/collect/nestedset/NestedSet.java`
- `src/main/java/com/google/devtools/build/lib/collect/nestedset/NestedSetBuilder.java`
- `src/main/java/com/google/devtools/build/lib/skyframe/PackageFunction.java`
- `src/main/java/com/google/devtools/build/lib/skyframe/GlobFunctionWithMultipleRecursiveFunctions.java`

Summary:

- Bazel makes evaluation a `SkyKey -> SkyValue` graph.
- `SkyFunction.compute` requests deps through `Environment`.
- Missing deps cause `null`/restart; the scheduler owns wakeup and reverse deps.
- `getValuesAndExceptions` expresses dependency groups that can be evaluated in
  parallel.
- `NodeEntry` is the single-flight/synchronization point per key.
- `NestedSet`/`Depset` is a persistent DAG and flattens only at consumption.

The direct lesson for Razel: make recursion data, not control flow.

### 4. New design doc created

`dev-docs/RazelDepsEngineV2.md` is documentation-only. It defines:

- `RazelDepsEngine { submit, subscribe }`,
- `EngineCommand` and `EngineEvent`,
- typed debug graph events replacing string-only `SchedHook`,
- four-layer versioning,
- `NodeMsg::{NeedOne, NeedMany, Produced, Failed, Emit}`,
- structured `QueryKey`,
- single-flight node lifecycle,
- persistent `DepsetNode`,
- legacy/new/compare engine adapters,
- migration and acceptance matrix.

No tests were run for that doc-only addition.

## Recommended next move

Start with a small, test-first implementation slice:

1. Add message API types behind a new module/crate boundary.
2. Implement `LegacyDepsEngine` over the existing `rules.rs` entry points.
3. Add tests proving:
   - `Evaluate(BuildSource)` emits accepted -> snapshot committed -> finished,
   - typed events can round-trip through the old `SchedHook` adapter,
   - semantic options affect an options digest while scheduling-only options do not.
4. Add `RAZEL_DEPS_ENGINE=legacy|graph|compare` plumbing only after the legacy
   adapter exists.

Do not start with the full scheduler. The seam first is the point of the agreed
migration plan.

## Warnings for next RR

- Gianni explicitly said normal TF run timeouts should be more than 10 minutes on
  1 thread and more than 7 minutes on 6 threads. For regression characterization,
  he allowed continuing to 20 minutes. Do not use tiny timeouts and call it wedged.
- The current 6-thread sample used `RAZEL_LOAD_THREADS=6`; the parse line still
  reports 12 threads because parse uses available parallelism.
- The 6-thread post-fix provider-reanalyze count is a serious signal. Do not lose it.
- The worktree contains user/session changes. Work with them; do not clean or
  revert unrelated files.

## Acceptance for incoming RR

- Read this note and `dev-docs/RazelDepsEngineV2.md`.
- Confirm current `git status`.
- Continue with the message API legacy-adapter slice, or explicitly ask Gianni if
  he wants more design before code.
- Flip this note to `Status: done` when oriented, with one line saying where the
  next work landed.
