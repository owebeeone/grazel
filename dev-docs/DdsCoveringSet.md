# The DDS Covering Set — validated against Bazel's core

The definitive answer to *"is this all-encompassing — can we rip Bazel apart into a finite
set of DDS query types?"* Grounded in three code-level passes over the Bazel checkout
(`scratch/bazel-api-to-dds.md`, `scratch/bazel-hardcases-to-dds.md`,
`scratch/bazel-skyframe-to-ivm.md`), not inference.

## Verdict (up front)

**Yes — and now proven, with the residual gaps named.** Three results:
1. **The covering set is finite — proven by enumeration.** Bazel's entire rule-facing
   contract is a **closed graph of 39 top-level `@StarlarkBuiltin` interfaces (103 annotated
   types)** with **no reflection/FFI escape**. A rule impl can only touch values reachable in
   that closure, so the covering set *is* that closure. Finiteness is a count, not a hope.
2. **The engine is a known, proven kind.** Skyframe **is** demand-driven memoized incremental
   evaluation with per-node dependency tracking + value-equality change-pruning — i.e.
   *exactly* razel's read-set IVM. We extracted the implementable protocol.
3. **Two algebras, both essentially closed**, with one named addition:
   - **Value/merge algebra: CLOSED at 5 monoids.** No 6th merge kind appears anywhere.
   - **Production algebra: needs one addition — `Constrain`** (a pure, bounded constraint
     solver), forced by cc feature config. Everything else (map/fold/select/value-creation/
     aspect) holds.
The remaining gaps are **not query-types or monoids** — they are **three engine-feature
axes** (config/edge derivation, production composition, execution-phase dynamism), each
bounded and individually scoped below.

---

## 1. The finite surface (the proof of finiteness)
- **39** top-level `@StarlarkBuiltin`/`@GlobalMethods` interfaces in `starlarkbuildapi/`;
  **103** annotated types with per-language subdirs + nested provider interfaces. Closed
  type graph, no escape hatch (`bazel-api-to-dds.md`).
- Per-language subdirs (cpp/java/…) add **typed provider records + opaque toolchain UDFs**,
  *not new mechanics* — so the closure doesn't blow up with each language.
- **This is the empirical bound:** the covering set = the closure of these 103 types. "Are we
  missing anything" is now a finite checklist, answered in §2–§6.

## 2. The value / merge algebra — CLOSED at 5 monoids
Every transitive accumulation in Bazel is a depset or a record/Map of depsets; **no 6th merge
kind exists** (`bazel-api-to-dds.md` Deliverable 3).
```
Scalar (unique-or-error) | Set (∪) | Map (key-wise) | Record(of the above)
OrderedDepset — a 4-MEMBER FAMILY (default | preorder | postorder | topological), NOT one monoid
   · dedup = keep-first at flatten;  · topological uses a reverse-build/reverse-read trick for diamonds
   · merge is PARTIAL: Order.isCompatible errors on mismatched non-default orders → a confluence
     guard is required AT THE FOLD SITE (NestedSet.java; Order.java:185)
```
**Correction to `DdsQuerySystem.md` §1a:** `OrderedDepset` is a *4-order family + a
compatibility guard*, not a single monoid. (Android resource/manifest "override" merge is
**native Java, not on the rule API at all** — so it is an *engine primitive*, not a
rule-facing monoid; the reserved `Overrideable` belongs to the engine layer, if used.)

## 3. The production / computation constructs — +1
The rule-facing computation reduces to: **`map`** (per-src action), **`fold`** (transitive
closure over an edge-kind), **`select`** (config conditional), **value-creation** (`ctx.action`
mints actions/files), **`aspect`** (per-node-in-closure traversal) — **plus one addition:**
```
Constrain  — a pure, bounded, deterministic, memoized constraint-solver UDF
           (forced by cc feature config: monotone implies-closure THEN a NON-monotone
            pruning worklist that removes/cascades selectables + a `provides` mutual-exclusion
            check — FeatureSelection.java:145-251).  A monotone fold/Derived closure CANNOT
            express the greatest-fixpoint shrinking; `Constrain` is engine-provided (like
            Bazel's native FeatureConfiguration), not rule-pack-authored.
```
This upgrades cc / R-γ from "tighten the framing" to **"add `Constrain`."** It is the only
new construct the rip-apart forced.

## 4. The engine — Skyframe's IVM, the protocol to implement
Confirmed: Skyframe = razel's read-set IVM (`bazel-skyframe-to-ivm.md`). The protocol razel
**must** implement (each cited in the scratch doc):
- **Demand-driven compute + dependency capture** (`Environment.getValue` records the read-set,
  captured in **ordered groups** — a flat read-set is insufficient for sequencing).
- **Restart-on-missing-dep** (`compute` returns null, re-runs) — razel must choose restart-style
  (dep-grouping + compute-state memo) vs block-style. (`DdsQuerySystem.md` §4 omitted this.)
- **dirty → `CHECK_DEPENDENCIES` → (`VERIFIED_CLEAN` | `NEEDS_REBUILDING`) → clean** — the
  re-check-deps-*without-recompute* path is the core IVM saving; §4 collapsed it into
  "recompute" and must be fixed.
- **Change-pruning by value equality** (`oldValue.equals(newValue)` keeps the old version →
  doesn't propagate → parent `VERIFIED_CLEAN`). Plus a `NotComparableSkyValue` always-dirty
  escape for non-deterministic facts.
- **Eager reverse-closure dirtying + lazy, signal-scheduled recompute** — *not*
  `for key in topo(dirty): recompute`.
- **EDB delta via a `Differencer.Diff`** (the add/remove-rules entry point).

**The sharpest correction (`DdsQuerySystem.md` §4): glob / add-remove is REIFICATION, not
"predicate read-sets."** Skyframe makes the predicate result a *memoized node*
(`DIRECTORY_LISTING_STATE` → `GLOB` → `PackageFunction`); adding a non-matching file dirties
the listing but the glob's matched-set `.equals` the old → change-pruning stops it. razel gets
O(affected) **without any predicate read-set** — reification + change-pruning is the proven
mechanism; predicate read-sets become an optional optimization.

## 5. The four ⚠ — resolved against real code
| ⚠ | verdict | grounding |
|---|---|---|
| **java `exports` re-propagation** | **✅ closed** — a plain `fold` over the `exports` edge-kind reading the *same* field (no splice operator) | `java_info.bzl:287-291` |
| **cc feature-config fixpoint** | **needs `Constrain`** (§3) — non-monotone pruning, not a fold | `FeatureSelection.java:145-251` |
| **proto aspect-traversal** | **still ⚠ — not yet traced** (this pass covered exports+fixpoint+depset+transitions) | — |
| **android resource override** | **native engine merge, not rule-facing** — not a query-type; precise semantics still untraced | `starlarkbuildapi/android/` has only `AndroidConfigurationApi` |
The one §5 worried about most (`exports`) **closes as a fold.** Two ⚠ remain to trace:
**proto aspect** and **android override** (the latter now known to be engine-native, not a monoid).

## 6. The real gaps — three ENGINE-feature axes (DECISIONS)

What "schema + monoid-folds + IVM" does *not* cover. **Decision (the transitions/
tree-artifacts call): DEFER both — but commit the exec/host subset, and reserve the shapes so
retrofit is additive, not a migration.** Each:

1. **Config / edge derivation.**
   - **exec/host transition — IN for V2.** It's `in_instance` + `AnalysisInstanceKey.exec_platform`
     (proc-macros and build-tools/codegen already use it; edges already cross instances). **The
     cc/rust dogfood gate exercises it** (a codegen tool built for the exec platform) — de-risks
     the cross-instance mechanism cheaply.
   - **General `Transitions` — DEFER (reserved capability). Retrofit risk LOW.** The keys
     already admit multi-instance (`TargetKey={instance,label}`, instance ← config) and edges
     already cross — so the *only* reservation is that **`DepEdge` carries an optional
     `transition`** (None = inherit instance; later = a fn → a different / `N` instances for
     split). Adding split + per-edge config rewrite is *additive*, not a key migration. **Go
     mixed-mode is the un-defer trigger — post-V2-core.**
   - **Dormant deps / computed-late-bound defaults** — defer; the `Attr` model reserves a
     computed-default form (None in V2).
2. **Production composition** — rule **extension** (`parent`/`super`) + **subrules**. Defer;
   reserved capability. (A rule-pack meta-feature; no V2 consumer.)
3. **Execution-phase dynamism — DEFER (reserved capability `DynamicIO`), retrofit risk
   MODERATE → reserve carefully.** cc/rust/py/sh don't need it; it's for proto/aidl/aapt2
   codegen (runtime-discovered output sets) + java annotation processing (post-V2-core), plus
   cc-header *precision* (an optimization — V2 declares the header superset; field-granular
   read-sets mitigate). Two reservations keep the retrofit additive:
   - **`File` is an OPEN enum** with a reserved **`Tree(ActionKey)`** variant (directory
     artifacts = a new variant, not a model change).
   - **The `Action`/`ActionKey` contract MUST NOT assume a fully-static input set** — reserve a
     post-run **`discovered_inputs` / `unused_inputs`** refinement (None in V2). This is the one
     genuinely-new exec-phase capability (the action graph gains a dynamic expansion), so
     reserving it now is what keeps it additive.
   - **Leading edge to un-defer:** the `.d`/`HeaderDiscovery`-style **input pruning** for cc
     caching precision (the MakeXS `DEPENDENCIES_OUTPUT` primitive, used to *prune* a declared
     superset — never to *discover with lag*; F2/F3). First exec-dynamism feature a real cc
     workload wants.

**Net:** V2 builds cc/rust/py/sh + the derivation core with **neither general-transitions nor
tree-artifacts**; exec/host is **in + gate-exercised**; both gaps are **reserved additively**
(`DepEdge.transition`, open `File` + `discovered_inputs`, the `Transitions`/`DynamicIO`
capabilities). Go and proto/android are the post-V2-core forcing functions that un-defer them.

---

## 7. Bottom line — your thesis, validated with a precise caveat

**It IS rip-apart-able into a finite set, and Bazel already did the distillation:** the
covering set is the closure of a **39/103 closed API**; the value/merge algebra is **closed at
5 monoids** (OrderedDepset a 4-order family); the production algebra is **map/fold/select/
value-creation/aspect + one addition, `Constrain`**; and the engine is **Skyframe's proven
IVM**, whose protocol we now have. For these layers, "the system becomes trivial + a small
test set" **holds** — the engine is fixed and a known kind, and rulesets are declarative
productions.

**The caveat (so this isn't hand-waving):** completeness is *not* "schema + queries" alone.
Three **engine features** sit outside the query/monoid algebra — config/edge derivation
(transitions), production composition (rule extension), and execution-phase dynamism (tree
artifacts). Most are deferrable for V2; **transitions and tree-artifacts are the two
load-bearing ones** that a faithful cc/java/go eventually needs and that AD8/static-actions
do not yet cover.

## 8. To fully close (two small traces left)
- **proto** — trace the `cc_proto_library` aspect through the §2/§3 notation (confirm
  aspect-traversal needs no new construct).
- **android** — trace aapt2 resource/manifest override (confirm it's an engine-native merge,
  pin its semantics).
Then the covering set is *proven* closed (value+production), with the three engine-feature
axes as the explicitly-scoped remainder.

*Amendments to fold into `DdsQuerySystem.md`: add `Constrain` (§3); OrderedDepset = 4-order
family + compat guard (§1a); replace predicate-read-sets with reification + the full
dirty→VERIFIED_CLEAN / restart / grouped-deps IVM protocol (§4).*
