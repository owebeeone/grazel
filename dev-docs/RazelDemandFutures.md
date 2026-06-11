# Demand futures — per-declaration InFlight/wait (the parallel coverage closer)

*2026-06-11, stabilization lane (round 33 design). Companion to `RazelEvalParallelPlan.md`
(P3's "the per-target `analyzing` set gets the same treatment" — this is that treatment,
designed against what P4a actually taught) and `RazelV3Checkpoint3Precis.md` §3 (the
coverage-degrades-with-threads finding this exists to retire). Engine-core: supervisor-grade.*

**Goal:** threads=12 full-sweep coverage == sequential coverage (307/835 at round 32), so
`xtask stress`'s band tightens from 90% to 100% and pool-by-default becomes a presentable
decision. Sequential (`threads=1`) behavior MUST stay byte-identical.

## §1 What the gap actually is (grounded, not assumed)

The parallel sweep loses 20–24 packages (303→283 at round 27 scale) to **cross-thread reads
of mid-flight package state**, in two shapes:

1. **Entry-vs-entry timing.** Worker B entry-drives P2 whose target T2 deps T4 declared in
   P1, mid-flight on worker A. Sequentially this read never happens at this timing: the
   sweep entry-drives P2 either after P1 completed, or as a no-op (`Ready`) because P1's
   eval already dep-loaded P2. Cross-thread, B's demand finds T4 in `pending` (Session-wide)
   but the declaration body lives on A's live module heap — unreachable (`local_pending`
   fails, harvest not yet stashed) — and errors "`T4` is neither a declared target nor a
   source file" / "not analyzed". Same wall, lost to timing.
2. **CycleProceed partial reads.** In a package cycle, the cycle-breaking reader proceeds
   against the owner's partial state. Sequential has the same mechanism (re-entry no-op)
   but exactly one partial reader per cycle at a deterministic, usually-late point; the pool
   has arbitrarily many at arbitrary, usually-early points.

Plus one true race the futures retire for free: **the native FnOnce slot** — two workers
demanding the same deferred native body (`Session.native_decls[i].take()`); the loser sees
an empty slot with no results row yet and errors with the wrong reason.

## §2 The completion event (the P4a bug-#3 question)

A declaration analyzed inside a consumer's eval produces TWO publications with different
visibility:

- **The `results` row + DDS facts** — published by `record_target` (`deps.rs`), visible
  cross-thread the moment it runs. This is the ONLY cross-thread-visible completion that
  exists mid-eval.
- **Captured Starlark provider instances** — land in the producing module's
  `DeclStore.captured` and become cross-thread-readable only when that whole module
  freezes and harvests (`stash_captured_for_freeze` → `index_harvest`). P4a bug #3's
  lesson, unchanged: no per-declaration event can make a live `Value` visible early.

**Decision:** a per-declaration future completes at `record_target`, published by whichever
worker runs the analysis; the **package's `finish_resource` is the backstop publisher** —
finishing a package (Ok, FailRetry, or FailCached) sweeps every outstanding declaration
entry of that package from the graph and wakes all waiters, who then re-resolve against the
now-terminal package state (harvest visible → demand-analyze locally; failed → surface the
package error). The future therefore guarantees *existence/analysis* (files, folded DDS
fields, alias rows), NOT instance locality: consumers that need captured instances keep the
existing local re-analysis fallback (`decls.rs` resolve_label_attr's wait-and-reread block).
That is also why cross-worker duplicate demand-analysis of **harvested** declarations stays
legal waste — single-flighting it would add a wait and still end in a local re-analysis.

## §3 Key namespaces (fix the discriminator BEFORE adding target keys)

The wait graph discriminates packages from `.bzl` modules by `:` in the string key
(`acquire_resource_locked`'s cycle arm, `loaded_done`'s filter). Target labels contain `:`
too — adding them as strings would classify every declaration as a `.bzl` and take over its
"eval" on cycle. **Decision:** a typed key, no string parsing:

```rust
enum ResKey { Pkg(String), Bzl(String), Decl(String) }   // HashMap key; one graph, one lock
```

`WaitGraph.res/waiting/live` re-key on `ResKey`; cycle resolution dispatches on the variant
(Pkg → CycleProceed, Bzl → takeover, Decl → §4). Hook/trace rendering keeps today's strings
(pkg name, bzl path, decl label) — existing sched_seam gates (`k == "a"`,
`ends_with("info.bzl")`) stay valid; new declaration events use distinct point names so
tests never parse keys.

## §4 The futures (what replaces the `analyzing` treatment, and what doesn't)

The per-thread `analyzing` set STAYS as the same-thread dependency-cycle detector (target
cycles are errors, Bazel semantics). The futures add the cross-thread coordination it never
had, in two modes:

**(B) Claim-to-run — native FnOnce bodies.** `ensure_analyzed`'s native arm acquires
`Decl(label)` BEFORE taking the slot: `Own` → take, run, `finish` (Ok → Done;
run-failure → `FailCached(err)`, mirroring the `native_errors` memo so waiters and later
demanders get the identical "analysis of `X` previously failed: …" error). `Ready`/`Failed`
→ re-read results / surface the cached error. Sequentially the first demander is always
`Own` and later demanders hit the results-first or `Failed` fast paths — byte-identical
messages, no new waits.

**(A) Wait-for-publication — pending declarations of mid-flight packages.** When
`ensure_analyzed` finds `label` still missing after the package-load step, but present in
the Session-wide `pending` map with its package `InFlight` on ANOTHER worker, the demander
inserts a **proxy entry** `Decl(label) = InFlight(package owner)` (atomically re-verifying
the package is still InFlight under the graph lock — a finish racing the insert must not
strand a waiter) and parks. Wakers, in order of arrival:
- `record_target(label)` flips an existing `Decl` entry to Done + notify (one mutex
  acquire + map probe per recorded target — cheap; the sched event fires only when an
  entry existed, so the hook stream doesn't flood).
- The package's finish sweep (§2) removes the entry + notify; the woken demander
  re-resolves via harvest or surfaces the package failure.
- The 20s backstop (loud `decl-timeout` event; MUST be zero in practice — the finish
  sweep makes a silent strand structurally impossible, P4a bug #6's lesson).

**Cycle handling:** proxy entries ride the same waits-for graph; the acquire-time walk
already traverses `waiting`/`res` generically, so package↔declaration cycles detect with no
new machinery. The variant rule: a **package** waiter in a cycle gets CycleProceed (today's
semantics — it CAN proceed against partial state); a **declaration** waiter in a cycle does
NOT wait (waiting would deadlock — the publisher is blocked on the waiter) and proceeds
into today's fallthrough paths. Both orderings of the A↔B dance resolve: if A parks on
Pkg(P2) first, B's decl-walk finds the cycle and B proceeds-partial; if B parks on Decl(T4)
first, A's package-walk finds the cycle, A gets CycleProceed, drives on, and `record_target`
wakes B — both packages succeed. Same-thread `Reentry` on a proxy entry (owner demanding
its own pending decl from inside a nested module) proceeds-partial — that IS sequential's
re-entry read, unchanged.

## §5 The restart pass (what closes the LAST gap to ==)

§4's futures make the lucky ordering win more often, but a declaration waiter that detects
a cycle still proceeds-partial and fails — and which party is "lucky" is a coin flip per
cycle. Sequential never flips this coin. **Decision: Skyframe's answer — restart.** A
cross-thread partial read is *recorded*, and an entry-drive that failed after consuming one
is *retried after the pool drains*:

- `EvalStack` (per-thread, Session-owned — AD2-clean) gains a `partial_proceeds` counter,
  incremented on package CycleProceed and on a declaration cycle-proceed. Both are
  cross-thread-only by construction (sequential re-entry takes the `Reentry` arm), so
  **threads=1 never sets it**.
- The tree driver's worker loop snapshots the counter around each entry; a failed entry
  with a partial read goes on a retry list. After the queue drains, retries run
  **single-threaded**, rounds until no progress. By then the cycle partners are terminal,
  so the retry sees what a sequential entry would have seen. FailRetry semantics already
  purge + clear for clean re-entry; a package that meanwhile completed via someone's
  dep-load is a `Ready` no-op — exactly the sequential sweep's shape.
- Termination: each round either converts a failure or stops; the loop is finite with no
  cap to tune. Retried counts surface in the report path (loud, not silent).

Declare-phase (`FailCached`) failures are not retried — a partial read during DECLARE has
no observed corpus shape; registered as a residual if the stress run ever shows one.

## §6 What this does NOT do

- No instance-locality change: `dep[P]` on a cross-thread mid-flight producer still
  resolves via wait → harvest → local re-analysis. The future bounds *when*, not *where*.
- No scheduling change: the spine wall (waiters-work / breadth-first seeding) is the next
  lever after this lands, per the précis §5 order.
- No default flip: the pool stays opt-in; parity numbers go to Gianni with the stress
  output (the plan's acceptance bar, unchanged).

## §7 Acceptance + test plan

1. `tests/sched_seam.rs` additions (deterministic, via the S2 hook; the gate helper is the
   TIMEOUT rendezvous, never a hard Barrier): (a) native single-flight — forced collision,
   one `own`, the loser served the same outcome, zero timeouts; (b) mid-flight decl demand —
   forced collision, the demander waits and succeeds (no "neither declared" error); (c) the
   existing package-cycle test upgraded to read `d[MyInfo]` through the cycle (the contract
   it explicitly deferred to this design).
2. `tests/parallel_parity.rs` — 10+ consecutive green runs (the corruption canary).
3. Full sweep: threads=12 coverage == sequential; then `xtask stress` with
   `RAZEL_STRESS_BAND_PCT` default tightened 90 → 100.

Roll-build order: ResKey refactor (mechanical, green) → (B) native single-flight → (A)
pending-decl wait + publish + finish-sweep → restart pass → acceptance.
