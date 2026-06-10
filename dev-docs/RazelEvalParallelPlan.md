# Eval worker-pool plan (the next 10×)

*2026-06-11. Premise measured, not assumed: full TF tree sweep is 1:00 single-threaded at
97% of one core (12-core machine); parse is 28ms parallel; sys-time is 2s. The only lever
left worth >1.5× is parallel eval. Gianni has accepted the price: convert Session's ~20
`RefCell` fields to concurrent forms.*

## Why the architecture is already parallel-shaped

- Cross-package values travel ONLY as frozen modules (`FrozenModule`/`OwnedFrozenValue` are
  `Send + Sync`) plus plain data (the freeze-and-harvest design). No live `Value` ever
  crosses a package boundary.
- Each package eval is single-threaded inside its own `Module`/heap — starlark-rust's model,
  untouched.
- The shareable caches (bzl_cache, harvest stores + indexes, walk/exists/glob/fold memos,
  ast_cache) are exactly Skyframe's shared state.

## Phases (each lands green on suite + gates + sentinels + rungold before the next)

**P1 — mechanical Sync conversion, still single-threaded.**
Session fields `RefCell<T>` → `parking_lot::Mutex<T>` (or `RwLock` for read-heavy maps:
results, aliases, config_specs, the memos). No behavior change; the win is isolating
borrow-discipline fallout from concurrency fallout. The [R1] rule ("never hold a borrow
across a nested eval") becomes lock discipline: take short guards, clone out, drop before
recursion — the existing code mostly does this already because RefCell forced it.
Gate note: AD2 is untouched (state stays per-Session; the xtask gate greps for
statics/OnceLock, not Mutex fields).

**P2 — Send bounds.**
- `NativeAnalyzeFn`: `Box<dyn Fn(..) + Send>` — bodies capture only plain data by
  construction (E0c), so this is a bound change, not a refactor.
- `eval.extra`: the `&Session` must be `&(dyn Sync)` — verify starlark's extra slot bounds.
- `AstModule` is already `Send` (proven by `prepare_build_asts`).

**P3 — demand futures + cycle detection.**
`loaded: HashSet<pkg>` → `HashMap<pkg, PackageState>` where
`PackageState = InFlight{owner: WorkerId, cond: Condvar-ish} | Done(Result)`.
Demand-loading a package another worker owns = wait on it. Cycle detection: each worker
carries its in-progress package stack; before waiting, check the owner's stack for the
requester's packages (or simpler: a global waits-for map under the same lock; abort the
wait with the existing "dependency cycle" error). The per-target `analyzing` set gets the
same treatment for cross-worker demand analysis.

**P4 — the pool.**
`load_tree_report_prepared` drives a work queue with N = available_parallelism workers,
each running today's sequential `load_package_entry` loop body. Measure contention with the
established profiling recipe (`CARGO_PROFILE_RELEASE_STRIP=none` +
`CARGO_PROFILE_RELEASE_DEBUG=true`, `sample` + dsymutil). Expect the DDS store and
`results` to be the contended locks; shard if measured, not before.

## Risks, ranked

1. **Lock-across-recursion deadlocks** (the RefCell-borrow equivalent). Mitigation: P1
   lands alone; every `.lock()` audited for nested-eval reach, same as the [R1] audit.
2. **Re-entrancy on the same package from two workers** — P3's InFlight state must cover
   the demand path (`ensure_analyzed` → `load_package`) not just the queue path.
3. **Throughput cliff from one mega-package** (llvm): the pool's tail is the biggest
   package's sequential eval. Acceptable; do not parallelize inside a package.
4. **Hidden thread-hostility in engine values** (Absorb/AbsorbWith etc. are values, fine;
   anything caching `Heap` references must stay per-eval — audit `alloc_complex_no_freeze`
   users for Session-held `Value`s; the DeclStore is module-rooted, fine).

## Acceptance

- Full TF sweep < 10s on 12 cores with identical coverage (270/835 baseline — must not drop).
- All 60 test bins, 3 gates, 6 sentinels, both rungolds green.
- A `RAZEL_LOAD_THREADS=1` escape hatch reproducing today's sequential behavior exactly.
