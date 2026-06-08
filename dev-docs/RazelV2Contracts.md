# Razel V2 — Seam Contract Specs

The concrete contracts for the seams `RazelV2FinalArchProposal.md` §5b names (Review-55's
"write the missing contracts before coding Phase 2"). Type sketches are Rust/taut-ish; the
**semantics are the contract** (encodings, equality, determinism, acceptance test). These
are what the Phase-2 cc gate proves *together*.

---

## 0. The DDS — the fact database (the spine)

*Added in response to "the DDS is central and not fleshed out." It is — everything below
is a facet of it.* The **DDS (Data Definition System)** is razel's **in-memory typed fact
database**: the single place facts live, the declaration registry, the **denormalization
point**, and the data source every query and matcher reads. The other contract sections are
not separate subsystems — they are the DDS's schema (§1–§2), write semantics (§3), read API
(§9), and serialization (§10). Naming the spine is what makes the pieces fall together:

| Thing named elsewhere | = DDS facet |
|---|---|
| `Analysis` / Session (AD2) | a **DDS instance** (per `AnalysisInstanceId`) |
| canonical contract (AD1) | the **fact schema** + the materialized fact set |
| registry (AD4) | the DDS's **declaration stratum** (loaded once) |
| `FactView` (§9) | the DDS **query API** |
| merge-classes (§3) | the DDS **write/merge semantics** |
| taut/CBOR substrate (AD6) | the DDS **serialization** (for mesh merge) |

**Two strata, one store:**
- **Declaration facts** — rule-packs, provider *schemas*, target-kinds, matchers, toolchains,
  config dimensions. Asserted once when the dialect is assembled; the "registry."
- **Analysis facts** — targets, provider *instances*, actions, derived facts. Per-instance.

**The reconciliation with AD2/AD3 (this is the important part).** The DDS is **"kind of
imperative"** — facts are **asserted** into it (mutation). That does **not** violate "no
ambient state": the DDS is the **one explicit, passed, scoped mutable store** (a value, not a
thread-local/global). "No ambient state" never meant "no mutable state" — it meant *the
mutable state must be visible-in-signatures, plural, and passed*. The DDS is exactly that.
And forcing (AD3) is preserved by the **producer/store split**:
- **Producers are pure and *return* facts** — `rule_pack.lower(facts, &DDS) -> Declared`,
  `matcher(&FactView) -> FactDelta`. They read the DDS (`&`) and return; they cannot assert.
- **Only the assembler/evaluator *asserts*** the returned facts into the DDS, under the
  merge-class discipline (§3). So the single imperative seam (`assert`) is one controlled,
  testable choke point — exactly YIDL's "imperative DDS + pure matchers."

**The DDS API (the contract):**
```
trait Dds {                                   // a per-AnalysisInstanceId handle
  fn assert(&self, f: Fact) -> Result<()>;    // THE imperative seam; merges per §3 merge-class; records provenance; errors on Scalar conflict
  fn query(&self, q: FactQuery) -> FactView;  // pure read: by subject, by field-namespace, by provider type-id, by predicate
  fn explain(&self, k: FactKey) -> Provenance;// the provenance chain (§4) — backs MCP explain
  fn export(&self, scope: Scope) -> Bytes;    // canonical taut/CBOR (§10) for mesh ship
  fn merge(&self, other: Bytes) -> Result<()>;// merge a peer instance's facts (§2 schema-compat + §3 merge-classes)
}
```
`assert`/`merge` are the only mutators; `query`/`explain`/`export` are pure reads. Producers
get `&FactView`/`&dyn Dds` (read), never the mutating handle. **Indexing (denormalization):**
the DDS indexes facts by subject, by field-namespace, and by provider type-id so matchers and
queries ("all targets exposing `CcInfo`", "all cflags of target T") are index hits, not graph
walks — this is the denormalization the matchers depend on.

**Acceptance:** a producer cannot `assert` (compile-time — it has `&FactView`, not the
handle); a `query` is deterministic (§10 ordering); two DDS instances `export`/`merge` a
provider fact across the mesh under §2 schema-compat; `explain` answers "why does this fact
exist?".

---

## 1. Identity & keys

All keys have a **canonical byte encoding** (for hashing + taut/CBOR) and **structural
equality** over that encoding. No key is a bare display string.

```
RepoId         = canonical repo name (main = "@"/"" ); resolved THROUGH RepoMap, not apparent  (HR-4)
Label          = { repo: RepoId, package: PackagePath, name: Name }     // a TARGET; canon "@repo//pkg:name"
BzlModuleKey   = { repo: RepoId, path: PackagePath }                     // a .bzl FILE — NOT a Label (55-2 B4)
RequestedInstanceKey  = { repo_mapping: Digest, target_platform, exec_platform, config: BTreeMap<..> }  // pre-analysis (B3)
ToolchainResolutionId = Digest    // = hash(a ToolchainResolution node, computed FROM RequestedInstanceKey)  (B3)
AnalysisInstanceId    = Digest    // = hash(RequestedInstanceKey ⊕ ToolchainResolutionId)
TargetKey      = { instance: AnalysisInstanceId, label: Label }         // STORED/SERIALIZED form always carries instance
ProviderTypeId = Native(&'static str)                                   // stable, e.g. "razel.cc.CcInfo"
               | Starlark{ module: BzlModuleKey, provider_id: ProvId }  // the DEFINING provider object (H3) — not an alias
ProviderSchemaId = Digest                                               // schema VERSION — separate from type (55-2 B1)
ProviderKey    = { ty: ProviderTypeId, schema: ProviderSchemaId }       // lookup matches `ty` first, THEN schema-compat (§2)
FieldId        = ProviderField{ ty: ProviderTypeId, tag: u32 }          // NAMESPACED by provider type (55-2 B2)
               | AttrField(Symbol) | ConfigField(ConfigKey) | Builtin(Symbol)
FactKey        = { subject: Subject, field: FieldId }                   // merge unit; field is namespaced
ActionKey      = Digest over { tool identity, exec platform, working dir/exec-root policy,
                               (logical-path, digest) input pairs, declared output shape,
                               argv, sorted env + inherit policy, param-files, exec props }  (55-2 B5)
Subject        = Target(TargetKey) | Package(PackageKey) | Source(SourceKey) | Action(ActionKey)
                 // PackageKey/SourceKey are INSTANCE-scoped (carry AnalysisInstanceId) — B2
```

- **`TargetKey` always carries `AnalysisInstanceId`** in stored/serialized form (the in-code
  shorthand is label-only within one instance's context; never globally "this label under all
  configs"). Same label in two instances = two distinct keys → no cross-mesh collision.
- **Provider identity is two-level (55-2 B1, the key fix):** a stable **`ProviderTypeId`** +
  a **`ProviderSchemaId`** (digest). Lookup/merge match the **type id first**, then apply
  schema compatibility (§2) — so schema *evolution* is versioning, not type replacement.
  `ProviderSet` and `FactKey` key by **`ProviderTypeId`** (compatible), with the
  `ProviderSchemaId` carried on the instance for compat checks.
- **Starlark provider identity = the *defining provider object* (55-2 H3),** not the importing
  alias: a `provider()` call site has a stable `(BzlModuleKey, provider_id)`; aliases /
  re-exports preserve it; an anonymous/private provider gets a stable generated `provider_id`
  (or is declared out of scope for V2 — the adapter rejects it with provenance).
- **`BzlModuleKey` ≠ `Label` (55-2 B4):** a `.bzl` file is its own entity. `load()` resolution
  is *contextual* — apparent repo names resolve through the **importer's repo mapping**;
  `BzlLoad` keys carry the resolved canonical repo + importer context so resolution is
  deterministic and provable (and so `ProviderNs::Starlark`'s module is canonical).
- **`FactKey.field` is namespaced (55-2 B2):** a provider *instance* is stored as **one Fact**
  keyed `(subject=Target, field=ProviderField{ty, tag=whole})`, merge-class `Scalar` (one
  provider of a type per target); attribute/config facts are per-field with their own
  merge-class. So "headers" alone is never a merge unit — the provider type namespaces it.

### Analysis-instance identity + the toolchain bootstrap (55-2 B3 / `REQ-CONFIG`)

The instance id is built in **two stages** to avoid the bootstrap cycle the reviewer caught
(toolchain resolution depends on config/platform/repo-map, but you can't analyze targets
without an instance id):
```
RequestedInstanceKey = {            // computable BEFORE any target analysis
  repo_mapping:   Digest,
  target_platform: PlatformId,
  exec_platform:   PlatformId,      // bounded exec/host; = target if none
  config:          BTreeMap<ConfigKey, ConfigValue>,   // sorted
}
ToolchainResolution  = a graph node: (RequestedInstanceKey) -> resolved toolchain set   // its own memoized node
ToolchainResolutionId = Digest(ToolchainResolution result)
AnalysisInstanceId   = Digest( RequestedInstanceKey ⊕ ToolchainResolutionId )
```
So `AnalysisInstanceKey.toolchains` is **not** an inline grab-bag — it is the
`ToolchainResolutionId` of a **separate, pre-analysis `ToolchainResolution` node**.
**Immutable for the life of the instance.** Multi-instance *is* the configuration boundary:
cross-platform = N instances (AD8), not `(Target×Config)` in one graph; a bounded exec/host
transition produces a *second instance* for the tool subgraph.

**Acceptance:** taut/CBOR fixtures + equality for every key; same `Label` under two instances
→ two non-equal `TargetKey`s, no fact collision; **a toolchain-selection change (same target
label, same config) produces a different `AnalysisInstanceId` with no collision** (B3 test).

---

## 2. Provider schema (B3 / `REQ-CONTRACT-002`)

```
ProviderSchema = {
  ty:     ProviderTypeId,               // STABLE identity (55-2 B1)
  schema: ProviderSchemaId,             // = hash(ty, fields, policy) — the VERSION
  fields: Vec<(Symbol, u32 /*tag*/, FieldType)>,   // declared order canonical; explicit tags
  unknown_field_policy: Reject | Ignore | Carry,
}
// FieldType is a CLOSED universe (55-2 H4): the adapter accepts exactly these from an
// evaluated rule() return; anything else (struct/dict/runfiles/opaque) FAILS analysis
// with provenance — never an opaque CBOR blob.
FieldType = Scalar(Prim) | Set<FieldType> | List<FieldType>
          | Depset<File|Label|Prim>     // ordered (§10)
          | Provider(ProviderTypeId)    // nested by type id
          | File | Label                // (dict/struct/runfiles are OUT OF SCOPE for V2 — test matrix must avoid)
```
- **Lookup matches `ty` first, then schema-compat (55-2 B1):** a consumer needing `ty@D1`
  receiving `ty@D2`: equal → OK; `D2` a field-superset of `D1` *and* policy `Carry`/`Ignore`
  → OK (unknown fields carried/ignored); else a typed **schema-mismatch error** (never
  silent; no coercion across differing field *types*). Identity is by **type id**; the schema
  digest governs *version compatibility*, not identity.
- **Closed field-type universe (55-2 H4):** the adapter enforces the boundary on the rule
  impl's *returned* provider — unsupported field shapes fail at analysis with provenance. If
  `dict`/`struct`/`runfiles` stay out of scope for V2, the stock-`.bzl` test matrix must
  avoid examples needing them and **state the limit** (Plan 3.5).
- **Canonical encoding:** taut/CBOR, fields in declared order with explicit tags (reorder =
  new schema; add-at-end = compatible).

**Acceptance:** a provider serialized on node A loads on node B with a superset schema by
**type id** under `Carry`; a *type* change fails with both digests; a rule() returning a
`dict`-shaped field fails analysis with provenance (not an opaque blob).

---

## 3. Fact model & merge-classes (AD9 / F25 / `REQ-COMP`)

```
Fact = { key: FactKey, value: TypedValue, prov: Provenance }
MergeClass =                              // declared per field in the schema
    Scalar              // exactly one assignment; conflict = ERROR (both provenance)
  | Set                 // deterministic set-union (sorted, §10)
  | OrderedList         // merge ONLY via explicit append/prepend ops; order preserved
  | OverrideableScalar  // narrower precedence scope may override broader, ONLY via explicit `override`
  | Derived             // inference proposes; an authored fact refines or rejects
```
- **Merge is keyed by `FactKey` and governed by the field's `MergeClass`.** No global
  "last-write-wins."
- **Confluence (the hard part, bounded):** `Set` (commutative+idempotent) and `Scalar`
  (no-overlap-or-error) are order-independent by construction. `OrderedList` carries explicit
  op order. `OverrideableScalar` resolves by the **precedence stack**
  `rule-pack-default < workspace < package < sidecar < transaction` (GrazelProposal §6.3).
  **Confluence is checked at *definition* time:** two producers that can write the same
  `Scalar` `FactKey` without a precedence relation = a **definition error**, not a runtime
  surprise. Start where decidable (`select`/`config_setting`/toolchain).
- **Scope:** a matcher/derivation composes the *importer's* facts; it MUST NOT mutate
  another instance's stored facts (no silent global mutation).

**Acceptance:** conflict tests — duplicate `Scalar` (error w/ both provenance), `Set` union
order-independence, ambiguous `select`/toolchain (definition-time rejection).

> **Calibration (Review-48 CAL-1).** Phase 2 **implements only `Scalar` + `Set` +
> definition-time confluence** — exactly what `select`/toolchain need. `OrderedList`,
> `OverrideableScalar` (and its 5-level precedence stack), and `Derived` are **declared but
> reserved** (unimplemented enum variants) — there are *zero* current call sites for them
> (`select` today just picks the first branch). They're built only when a derivation (Phase
> 5) or a sidecar surface actually needs them. This pulls ~300–400 LOC off the Phase-2
> critical path that the providers/engine/loading rebuilds need — *don't over-build the
> composition engine before it has consumers.*

---

## 4. Provenance (`REQ-CONTRACT-004`)

```
Provenance = {
  surface:   Bazel | Grazel | Mcp | Inference,
  origin:    SourceSpan{ file, line, col } | AdapterStep(&str) | InferencePass(&str),
  rule_pack: Option<PackId>,
  merge_op:  Option<MergeOp>,   // set if this fact resulted from a merge/override
}
```
Every non-trivial fact carries it. **Acceptance (`REQ-COMP-002`):** for the cc dogfood
graph, `explain(provider|action)` answers "why does this exist?" — surface, span/adapter
step, producing pack, and any merge — *before* Phase 5.

---

## 5. Adapter contract — the effect-capturing boundary (B1 / `REQ-ADAPTER`)

The Bazel adapter is **not** the pure rule-pack layer; it is the imperative boundary that
captures effects into the contract. Three input classes, each with a defined lowering:

1. **macro-only `.bzl`** (plain `def` calling rules) → expand to rule-pack target
   invocations; no rule runtime needed.
2. **evaluated `rule()` impl** → the adapter supplies an **effect-capturing `ctx`**:
   `ctx.actions.run/.write/...` **record `Action` facts** into the `Analysis`; the impl's
   returned providers are **captured as `Provider` facts**; `ctx.attr`/`ctx.file(s)` read
   from the declared `TargetFacts`. The impl runs as Starlark (imperative, mandated) but its
   *effects become facts*. This is razel's existing rule()-eval machinery, redirected from
   thread-locals to the Session/contract.
3. **native-shimmed rule** (cc/rust/…) → resolved directly to a **pure rule pack** (an
   optimization over evaluating the stock `.bzl`).

Contract: **all imperative Starlark side-effects are captured here and nowhere else**;
downstream (engine, derivations, exec) sees only facts. **Acceptance:** the Phase-2 gate's
evaluated-`rule()` test (consumes a dep provider, **`return`s a typed provider**, calls
`ctx.actions`, loaded via `.bzl`) lands as contract facts — *not via a native shim*, and the
gate asserts the *returned* provider became a fact.

> **Reality check (Review-48 BL-1).** This is **greenfield**, not "redirect existing
> machinery." Today `rules.rs` *discards* the impl's return value (`:564`), has **no
> `provider()` builtin and no typed `Provider`** (`DefaultInfo` is a side-effecting builtin,
> `:701`), and *ignores* `attrs` (`:587`). The thread-local→Session move is only the
> *state-store* half; the provider-capture half — `provider()` + a typed `Provider` value +
> return-value capture in `RuleObjGen::invoke` + the `attr`-schema/`TargetFacts` layer — is
> **new construction** and is a Phase-2 prerequisite of this adapter (Plan Step 2.13a).

---

## 6. Loading as a graph effect (B2 / `REQ-LOAD`)

All loading reads are **engine nodes** over a content-addressed VFS (`razel-vfs`); their
outputs are invalidation keys:

```
SourceSnapshot(Label) -> FileDigest
DirListing(PackagePath) -> Listing                 // backs glob()
BzlLoad(Label)        -> FrozenModule              // depends on SourceSnapshot + transitive loads
PackageLoad(Label, AnalysisInstanceId) -> Vec<DeclaredTarget>
RepoMap()             -> RepoMapping               // depends on MODULE/WORKSPACE snapshots
```
Editing a BUILD/`.bzl`, adding/removing a globbed file, or changing a repo mapping changes a
snapshot/listing digest → invalidates exactly the dependent nodes. **No ambient filesystem
reads during analysis.** **Acceptance (`REQ-LOAD-003`):** the four edit scenarios invalidate
exactly the affected nodes — **asserted on observable behavior (downstream recompute vs
early-cutoff), not a brittle internal dirty-set shape** (55-2 low-sev): an early-cutoff
engine may dirty-then-clean, so the test pins *dependency correctness + recompute/cutoff*,
not the engine's transient marks.

> **Reality check (Review-48 BL-3).** **Greenfield**, not "reuse `razel-vfs`." `razel-vfs`
> (132 LOC) supplies only the `ContentProvider`/COW `View` abstraction and **has no
> dependents**; **none** of `SourceSnapshot`/`DirListing`/`BzlLoad`/`RepoMap` exist, and
> loading today is ambient `fs::read` (`rpc.rs:218`). This is the foundational
> incrementality seam and it is entirely new (`BzlLoad` transitive-load tracking alone is
> non-trivial — Plan splits it 2.6a/2.6b).

---

## 7. Engine node taxonomy (B5 / `REQ-ENGINE`)

```
Node =
    SourceSnapshot(Label) | DirListing(PackagePath) | RepoMap
  | BzlLoad(Label) | PackageLoad(Label, AnalysisInstanceId)
  | Analyze(TargetKey)            -> ProviderSet            // run the rule pack / adapter
  | ActionPlan(TargetKey)         -> Vec<ActionKey>
  | ActionExec(ActionKey)         -> Outputs                // the only process effect
  | Derivation(DerivId, Scope)    -> Vec<Fact>              // F17, later
```
- Each node's **key is its identity + memo key**; config-relevant nodes include the
  `AnalysisInstanceId`. Demand-driven (a node requests deps; early-cutoff via digest equality
  of node values).
- **CLI and daemon both route through this engine** (no straight `execute` loop) — wired in
  Phase 2 so the cc proof exercises the real keys/invalidation (B5).
- `ProviderSet` (the `Analyze` value) must **hash stably** for early-cutoff (canonical
  provider ordering, §10).

**Acceptance:** the cc gate runs end-to-end through `razel-engine` with **actual typed
`Analyze(TargetKey)` and `BzlLoad(Label)` nodes on the graph** (a build whose graph has zero
`Analyze` nodes **fails** the gate); editing a `.bzl` invalidates the `Analyze` node via its
`BzlLoad` dep; an unaffected edit re-runs nothing downstream (early-cutoff).

> **Reality check (Review-48 BL-2).** This is a **rebuild of the engine's value/key model**,
> not "reuse." Today `razel-engine` is `type Key = String; ComputeFn -> Digest`
> (`engine/lib.rs:15-17`) — a node computes to a *Digest*, with **nowhere to store a typed
> `ProviderSet`/`Vec<ActionKey>`/`Outputs`**; the daemon *bypasses* it (`rpc.rs:225` calls
> `execute`); `IncrementalBuilder` wires only string-keyed action nodes. The 285 LOC is a
> reference for the early-cutoff *algorithm*, not a base to extend — the typed-node-value
> model is new (and is why the gate must demand typed `Analyze` nodes, or it passes on the
> Digest-toy with only action outputs on the graph).

---

## 8. Rule-pack API — capability facets, not a god-constructor (H1 / `REQ-RULEPACK`)

```
struct RulePack {
  id:            PackId,
  kernel_abi:    Version,                // 55-2 H5: schema version + feature negotiation
  uses:          Vec<Capability>,        // declared capabilities the pack requires
  target_kinds:  Vec<TargetKindDecl>,    // facet: name + attr schema
  providers:     Vec<ProviderSchema>,    // facet
  inference:     Vec<InferencePass>,     // facet (optional), pure
  validations:   Vec<Validation>,        // facet (optional), pure
  // reserved Capability variants (declared, unimplemented in V2): Aspects, Test, Runfiles, ExecGroups, Transitions, Repos
}
// the lowering is a PURE free function per target-kind:
fn lower(target: &TargetFacts, dds: &FactView) -> Result<Declared, LowerError>;
struct Declared { providers: Vec<ProviderInstance>, actions: Vec<ActionDecl>, facts: Vec<Fact> }
```
- **Forcing (AD3):** `lower` takes `&FactView` (read-only DDS query + kernel primitives) and
  **returns** `Declared`. No `&mut`, no globals, no DDS-assert handle → the *only* place to
  put logic is the return value; an imperative shortcut **does not type-check**.
- Capabilities are **separate facets**, never flags accreting on one constructor (the S5
  scar).
- **Capability negotiation (55-2 H5):** a pack declares `kernel_abi` + the `Capability`s it
  `uses`; **the registry rejects a pack at load** (clear diagnostic) if it uses a
  reserved-but-unimplemented capability or an incompatible `kernel_abi`. The registry exposes
  **capability discovery** so a Phase-4 agent can check whether a kernel feature exists
  *before* editing files (turns the R12 "needs a kernel change" signal into a query).

**Acceptance:** the `cc` pack is expressed entirely through these facets + a pure `lower`;
a second/third pack (rust/py/sh) add **zero kernel lines**; a pack declaring `uses:
[Aspects]` fails at registry load with a diagnostic.

---

## 9. Matcher / derivation evaluator (AD9, bounded)

```
type Matcher = fn(&FactView) -> Option<FactDelta>;   // pure; FactView = read-only fact query
struct FactDelta { add: Vec<Fact>, override_: Vec<(FactKey, TypedValue, PrecedenceScope)> }
```
- **Additive by default** (`add`) → monotone, confluent, terminating (fixpoint).
- **`override_` only via declared `OverrideableScalar` at an explicit `PrecedenceScope`** —
  never implicit, never global.
- **Definition-time confluence:** the registry rejects matcher sets whose `add`/`override_`
  can produce a `Scalar` conflict with no precedence relation. Begin with Eq-decidable
  domains (`select`/`config_setting`, toolchain).
- General non-monotonic composition is **out of scope** (deferred; not a logic engine).

**Acceptance:** `select`/toolchain resolution via matchers with definition-time overlap
rejection; provenance on every derived fact.

---

## 10. Determinism rules (`REQ-CONTRACT-003`)

- **Depsets:** explicit order (preorder/postorder/topological), deterministic dedup.
  *(Review-48 HR-2: the current `depset` **drops order** — `rules.rs:714` `let _ = order`.
  This is a Phase-2 must-fix, not a deferral: providers carry depsets and merge across the
  mesh, so order-dropping breaks both early-cutoff and cross-instance merge. A golden fixture
  must assert a transitive depset's order — Plan Step 2.10.)*
- **Action fixtures (55-2 B5):** the golden set MUST include a **path-sensitive compile
  action** (two inputs, same digest, different logical path → different `ActionKey`), an
  **env-var-dependent action**, and a **param-file action** — proving every semantic field is
  in the key.
- **Sets:** canonical sort (by canonical element encoding).
- **Paths:** workspace-relative, `/`-separated, normalized (no `.`/`..`).
- **Action argv:** declared order; **env:** sorted by key. Both part of the `ActionKey`.
- **Serialization:** canonical CBOR — maps as sorted-key or declared-tag; no float NaN; ints
  minimal-width. Fixtures committed; CI checks regeneration is byte-identical.

**Acceptance:** taut/CBOR golden fixtures for a sample target/provider/action/fact graph;
re-encode is byte-identical across two runs/machines.

---

## 11. Contract → Phase-2 gate map

| Contract | Gate item |
|---|---|
| §1 keys + AnalysisInstanceKey | fixtures + equality; same-label-two-instances no collision |
| §2 provider schema | superset-compat loads; type change errors |
| §3 facts/merge | duplicate-Scalar error; Set order-independence; ambiguous match rejected |
| §4 provenance | `explain` answers "why?" for cc graph |
| §5 adapter | evaluated `rule()` impl captured into facts (not a shim) |
| §6 loading | 4 edit scenarios invalidate exactly the affected nodes |
| §7 engine | cc runs through `razel-engine`; early-cutoff on unaffected edits |
| §8 rule-pack | cc via facets+pure-lower; rust/py/sh add 0 kernel lines |
| §9 matchers | select/toolchain via matchers; definition-time confluence |
| §10 determinism | byte-identical CBOR fixtures |

*Each contract is a Phase-2 deliverable with a **red-first contract test at its implementing
step** (55-2 H1 — the §11 row→step map below); the cc dogfood (2.15/2.16) is the
cross-seam **integration** proof, not the first test of any single seam.*

| Contract | First failing test lands at | Integration |
|---|---|---|
| §0 DDS assert/query/explain | 2.1 (store) / 2.9 (producer can't assert) | 2.16 |
| §1 keys + identity splits | 2.1 | 2.16 |
| §1 instance + toolchain bootstrap | 2.2 | 2.16 (B3 toolchain-change test) |
| §2 provider schema/compat | 2.3 | 2.16 (D1↔D2 compat test) |
| §3 facts/merge (Scalar+Set) | 2.4 | 2.16 |
| §4 provenance | 2.12 | 2.16 (explain) |
| §5 adapter return-capture | 2.12/2.13 | 2.16 |
| §6 loading invalidation | 2.6a/2.6b | 2.16 |
| §7 typed engine nodes | 2.7 / 2.14 (Analyze) | 2.16 |
| §8 rule-pack + capability negotiation | 2.9 | 3.2 (rust = zero kernel) |
| §9 matchers (Eq-decidable) | 2.11 | 2.16 |
| §10 determinism (depset order, action fixtures) | 2.1 / 2.10 | 2.16 |
