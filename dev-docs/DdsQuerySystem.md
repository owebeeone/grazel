# The DDS Query System — the covering feature set

**Thesis.** The build engine *is* a **demand-driven, incremental, monoid-aggregating
datalog evaluator over the fact schema.** A rule pack writes **productions** (datalog-style
rules: `head :- body`, with aggregation + a pure lowering UDF); the engine supplies
recursion, aggregation, memoization, and incremental maintenance. If the **covering set of
features below is closed and correct**, then: the engine is a *known, solved kind* (Salsa /
differential-dataflow / semi-naive datalog with provenance), and **each ruleset is just
declarative productions** — which is what makes the rest "trivial + a small test set."

This document: (1) classifies every kind of captured data, rule, and transformation; (2)
gives the query notation + Python pseudocode; (3) pins the **covering feature set** as a
checklist (taxonomy → feature, so nothing is missed); (4) shows incremental add/remove of
rules; (5) honestly scopes what the **full `.bzl` corpus** must still validate.

Notation choice: a **datalog-derived DSL**, not SQL. Recursion, provenance, and incremental
maintenance are *native* to datalog and to differential dataflow (the proven IVM engine for
exactly this); SQL recursive-CTEs make aggregation-in-recursion and IVM awkward. We separate
the **notation** (what rule packs write) from the **engine** (demand-driven memoized IVM).

---

## 1. Classification (so we don't miss anything)

### 1a. Kinds of captured data — the closed element + **monoid** set
A fact value is an element type combined by a **merge monoid**. The claim: this set is closed.
```
element types : Scalar(prim|bool|int|str) | File | Label | ProviderRef | Tuple/Record
containers    : List | Set | OrderedDepset(order) | Map<K,V>
merge monoids : Scalar(unique-or-error) | Set(∪) | Map(key-merge)
              | OrderedDepset — a 4-ORDER FAMILY (default|preorder|postorder|topological),
                dedup=keep-first-at-flatten, MERGE IS PARTIAL: a confluence guard at the fold
                site errors on mismatched non-default orders (Bazel `Order.isCompatible`).
# NOTE (Bazel rip-apart): the value/merge algebra is CLOSED at the above — no 6th merge kind
# exists in Bazel's 39/103 rule API. `Overrideable`/precedence is NOT a rule-facing monoid:
# Android resource/manifest override is a NATIVE engine merge, not on the Starlark API at all
# — so if used it lives in the engine layer, not as a rule-pack monoid. (See DdsCoveringSet.md §2.)
```
`Overrideable` is the precedence monoid we *reserved* — Android resource merging makes it
**real**, not hypothetical.

### 1b. Kinds of rules
```
leaf-compile    : one src → one action (cc/rust/go/java/proto-descriptor compile)
aggregate/link  : deps' outputs → one binary/archive/jar/test
collect         : filegroup, *_library-as-collection, proto_library (gathers .proto + descriptor)
codegen         : aidl/aapt2/protoc — produce FILES (sources/resources) consumed downstream
macro           : expands to other rules (handled in the adapter, pre-fact)
aspect          : run a production over EACH node in a transitive closure (cc_proto aspect)
config/toolchain: config_setting, toolchain, platform, constraint
meta            : alias, test_suite, environment
```

### 1c. Kinds of transformations (productions)
```
1:1 map              : per-src action
fold/closure         : transitive provider accumulation (Set | OrderedDepset | Map | Overrideable)
edge-discriminated   : propagate differently along deps vs exports vs runtime_deps vs proc_macro_deps
re-propagation       : `exports` splices a child provider transitively through the exporter
conditional          : select/config_setting (stratified branch by config)
Constrain (NEW)      : cc feature config — NOT a monotone fold/closure. It is monotone
                       implies-closure THEN a non-monotone pruning worklist (removes/cascades)
                       + a `provides` mutual-exclusion check. A pure, bounded, deterministic,
                       memoized CONSTRAINT-SOLVER UDF, engine-provided (Bazel ships it native;
                       `FeatureSelection.java:145-251`). The ONE production construct the
                       rip-apart forced beyond map/fold/select/value-creation/aspect.
value-creation       : mint ActionKey + generated-File rows (skolem)
codegen→consume      : generated files become inputs to a downstream production
aspect-traversal     : a production that fires per-node-in-closure, attaching/propagating facts
cross-instance       : read a dep's provider in another AnalysisInstanceId (exec/proc-macro/transition)
multi-output         : one action → several role-typed outputs (ijar/hjar, R.java + resources.ap_)
```

---

## 2. The query notation + Python pseudocode

A production is `lower(ctx) -> [Fact]`. `ctx` is the **demand-driven, read-set-tracking**
handle: every access is recorded (for IVM) and recursive accesses are memoized.

```python
# ---------- monoids (the closed merge set) ----------
class Monoid:                         # how a fold combines values
    SCALAR        = "scalar"          # exactly one; conflict = error
    SET           = "set"             # commutative union
    ORDDEPSET     = "orddepset"       # ordered concat + dedup-keep-first (preorder|postorder)
    MAP           = "map"             # key-wise merge (inner monoid per value)
    OVERRIDEABLE  = "override"        # precedence-ordered (last|first wins)  ← android resources

# ---------- the fact store (the DDS) ----------
# EDB (input, from BUILD/.bzl via the adapter):   Target, Attr, SrcFile, DepEdge
# IDB (derived, by productions):                  Provider, Action, ActionInput/Output/Dep
# every IDB fact carries: value, provenance, read_set (the keys it read, incl. PREDICATE reads)

# ---------- demand-driven evaluation context ----------
class Ctx:
    def __init__(self, tk, instance, engine):
        self.tk, self.instance, self.engine, self.read_set = tk, instance, engine, ReadSet()

    def attr(self, name):                         # a property (with select already resolved, see §below)
        self.read_set.add(("Attr", self.tk, name))
        return self.engine.attr(self.tk, name)

    def deps(self, kind=None):                    # per-EDGE access (R-β): returns edge records
        self.read_set.add(("DepEdge", self.tk, kind))
        return [e for e in self.engine.dep_edges(self.tk) if kind is None or e.kind == kind]
        # each e = Edge(dep_tk, kind, role, alias)

    def provider(self, dep_tk, ptype, field=None, instance=None):   # RECURSION + field-granular read
        inst = instance or self.instance
        self.read_set.add(("Provider", (inst, dep_tk), ptype, field))   # FIELD-granular (DdsWorkedTransformation refinement)
        return self.engine.analyze(dep_tk, inst).provider(ptype, field)

    def fold(self, kind, ptype, field, monoid, order=None):          # the recursive aggregation
        # transitive closure over DepEdge(self, *, kind), reading dep.Provider(ptype).field,
        # combined by `monoid`. The engine memoizes the closure; read_set records the frontier.
        return self.engine.fold(self.tk, self.instance, kind, ptype, field, monoid, order, self.read_set)

    def action(self, kind, argv, inputs, outputs, env=None, exec_instance=None):  # VALUE-CREATION (skolem)
        ak = digest(kind, argv, env, [self.engine.digest(i) for i in inputs], outputs, exec_instance)
        self.engine.emit(Action(ak, self.tk, kind, argv, env, exec_instance),
                         [ActionInput(ak, f) for f in inputs], [ActionOutput(ak, f) for f in outputs])
        return ak, outputs

    def in_instance(self, instance):              # CROSS-INSTANCE (proc-macro/exec/transition)
        return self.engine.subctx(self.tk, instance, self.read_set)

    def toolchain(self, kind):                    # a Derived (incl. cc feature FIXPOINT, see below)
        self.read_set.add(("Toolchain", kind, self.instance))
        return self.engine.toolchain(kind, self.instance)

# ---------- example productions (the whole point: each ruleset is THIS small) ----------
def cc_library(ctx):
    hdrs = ctx.attr("hdrs"); copts = ctx.attr("copts")
    thdrs = ctx.fold("deps", "CcInfo", "hdrs", Monoid.SET) | set(hdrs)            # transitive headers
    objs  = [ctx.action("compile", [CC, *copts, "-c", s, *inc(thdrs), "-o", obj(s)],
                        inputs=[s, *thdrs], outputs=[obj(s)])[0] for s in ctx.attr("srcs")]
    arch  = ctx.action("archive", [AR, "rcs", lib(ctx.tk), *map(out_of, objs)],
                       inputs=[out_of(o) for o in objs], outputs=[lib(ctx.tk)])
    tlink = ctx.fold("deps","CcInfo","link", Monoid.ORDDEPSET, order="postorder") + [lib(ctx.tk)]  # R-α ordered
    return [Provider("CcInfo", {"hdrs": thdrs, "link": tlink})]

def rust_library(ctx):
    externs, infiles = [], []
    for e in ctx.deps("deps"):                                                   # per-edge (R-β)
        ci = ctx.provider(e.dep, "CrateInfo")
        externs.append(f"--extern={e.alias or ci['name']}={ci['out']}"); infiles.append(ci["out"])
    for e in ctx.deps("proc_macro_deps"):                                        # cross-instance (R-β)
        ci = ctx.provider(e.dep, "CrateInfo", instance=EXEC)
        externs.append(f"--extern={e.alias}={ci['out']}"); infiles.append(ci["out"])
    cclink = ctx.fold("deps","CcInfo","link", Monoid.ORDDEPSET, order="postorder")  # cross-language
    ak, [o] = ctx.action("rustc", [RUSTC, src_of(ctx), *externs, *link(cclink), "-o", rlib(ctx.tk)],
                         inputs=[src_of(ctx), *infiles, *cclink], outputs=[rlib(ctx.tk)])
    return [Provider("CrateInfo", {"name": crate_name(ctx), "out": rlib(ctx.tk), "type": "rlib"}),
            Provider("DepInfo",  {"cc_link": cclink})]

def java_library(ctx):                                                           # the exports splice (R-β, the hard one)
    compile_jars = ctx.fold("deps","JavaInfo","compile_jars", Monoid.ORDDEPSET) \
                 | ctx.fold("exports","JavaInfo","compile_jars_PLUS_self", Monoid.ORDDEPSET)   # exports re-propagate
    runtime_jars = compile_jars | ctx.fold("runtime_deps","JavaInfo","runtime_jars", Monoid.ORDDEPSET)
    hjar, classjar = ctx.action("javac", ..., outputs=[hjar(ctx.tk), classjar(ctx.tk)])        # multi-output
    return [Provider("JavaInfo", {"compile_jars": compile_jars | {hjar},                       # header→compile closure
                                  "runtime_jars": runtime_jars | {classjar},                   # full→runtime closure
                                  "exported": ctx.deps("exports")})]                            # for re-propagation

def cc_proto_aspect(ctx, proto_tk):                                              # ASPECT + codegen
    info = ctx.provider(proto_tk, "ProtoInfo")
    gen  = ctx.action("protoc", [PROTOC, "--cpp_out", out_dir, info["descriptor"]],
                      inputs=[info["descriptor"]], outputs=gen_srcs(proto_tk))    # codegen → generated files
    # then compile the generated srcs as a cc_library production, propagate CcInfo per proto node
    return cc_library_over(ctx, srcs=out_of(gen))

def android_resources(ctx):                                                      # OVERRIDEABLE merge + multi-output
    res = ctx.fold("deps","AndroidResInfo","resources", Monoid.OVERRIDEABLE, order="dep-order")  # transitive override
    ak, outs = ctx.action("aapt2", [AAPT2, "link", *res], inputs=res,
                          outputs=[r_java(ctx.tk), ap_(ctx.tk)])                  # multi-output
    return [Provider("AndroidResInfo", {"resources": res | own_res(ctx), "r_java": r_java(ctx.tk)})]

# ---------- select / config (stratified conditional) — resolved when an attr is read ----------
def resolve_attr(engine, tk, name, instance):
    raw = engine.raw_attr(tk, name)
    if is_select(raw):
        for cond, val in raw.branches:           # config_setting match against instance.config
            if engine.config_matches(cond, instance): return val   # first match (or //conditions:default)
    return raw
```

```python
# ---------- the engine: demand-driven, memoized, incremental ----------
class Engine:
    def analyze(self, tk, instance):             # TOP-DOWN demand-driven + memoized (Salsa-style)
        key = (instance, tk)
        if key in self.memo and not self.dirty(key):
            return self.memo[key]                # early-cutoff
        ctx = Ctx(tk, instance, self)
        facts = KIND_LOWER[self.kind(tk)](ctx)   # the production runs, recursing via ctx.provider/fold
        result = Result(facts, ctx.read_set, value_digest(facts))
        self.memo[key] = result                  # read_set is the memo dependency footprint
        return result

    def build(self, label):                      # F10 demand-driven: only analyze what's reachable
        return self.execute(self.analyze(self.toplevel(label), self.default_instance))
```

---

## 3. The covering feature set (taxonomy → feature checklist)

| Captured data / rule / transform | Covered by | status |
|---|---|---|
| scalar/list/set/file/label/record attrs | fact element types | ✅ |
| **map** attr (env, aliases, defines) | `Map` value type | ✅ (R-α) |
| configurable attr (`select`) | `resolve_attr` stratified conditional vs `instance.config` | ✅ |
| **per-edge data** (alias/role) | `DepEdge` attributes + `ctx.deps(kind)` | ✅ (R-β) |
| transitive provider (set) | `ctx.fold(... SET)` | ✅ |
| **ordered** transitive (link order) | `ctx.fold(... ORDDEPSET, order)` | ✅ (R-α) |
| edge-kind propagation (deps/exports/runtime_deps) | `ctx.deps(kind)` + per-kind `fold` | ✅ |
| **`exports` re-propagation** | a `fold` over `exports` that includes the child's own provider | ⚠ needs corpus check (java) |
| **cc feature config** | **`Constrain`** (constraint-solver UDF, engine-provided) | ✅ resolved (Bazel rip-apart: not a fold) |
| argv/path synthesis | pure UDF inside `lower` | ✅ |
| **action / generated-file creation** | `ctx.action` (skolem/value-creation) | ✅ |
| **codegen → consume** (aidl/aapt2/protoc) | generated `File`s as downstream `inputs` | ✅ |
| **aspect** (per-node-in-closure) | a production parameterized by traversal | ⚠ needs corpus check (proto) |
| **multi-output** (ijar/hjar, R.java+ap_) | `ctx.action(... outputs=[...])` | ✅ |
| **override/precedence merge** (android res) | `Monoid.OVERRIDEABLE` (the reserved class, now real) | ⚠ needs corpus check (android) |
| **cross-instance** (proc-macro/exec/transition) | `ctx.in_instance` / `provider(..., instance=)` | ✅ (R-β); transitions = deferred |
| toolchain resolution | `ctx.toolchain` (Derived) | ✅ |
| **glob / add+remove rules** | **reification** (glob = a memoized node) **+ change-pruning** (§4) | ✅ (Skyframe-proven) |

So the covering set is **~16 features over a closed 5-monoid algebra** — small, and each
taxonomy item maps to exactly one. That smallness *is* the "it becomes trivial" claim: the
engine is fixed; rulesets are productions.

---

## 4. Incremental — add/remove rules on the fly (the Skyframe-proven protocol)

The engine is **demand-driven memoized incremental evaluation with change-pruning** — i.e.
Skyframe (verified, `scratch/bazel-skyframe-to-ivm.md`). A BUILD edit is an **EDB delta**;
the protocol below is Skyframe's, the proven reference.

**The state machine each node runs (the core IVM saving — *not* "just recompute"):**
```
clean ──Δ touches a dep──▶ dirty ──CHECK_DEPENDENCIES──▶ { all deps' values unchanged → VERIFIED_CLEAN (NO recompute!)
                                                          { a dep changed          → NEEDS_REBUILDING → recompute
recompute: run lower(ctx); CHANGE-PRUNE: if newValue.equals(oldValue) keep the OLD version
           ⇒ this node's version doesn't advance ⇒ its parents see no change ⇒ they VERIFY_CLEAN.
```
**The protocol razel must implement** (all from Skyframe):
- **Grouped dep capture** — read_set is captured in *ordered groups*, not a flat set (needed
  for restart sequencing + parallelism). A flat read_set is insufficient.
- **Restart-on-missing-dep** — `lower` requests a dep that isn't ready ⇒ it *returns/suspends*
  and re-runs from the top once ready (vs block-style). Pick one; razel must choose.
- **Eager reverse-closure dirtying, lazy recompute** — on Δ, mark the reverse-dep closure
  dirty *eagerly*; recompute *lazily on demand* via signaling. **Not** `for k in topo(dirty)`.
- **Change-pruning by value equality** + a `NotComparable` always-dirty escape for
  non-deterministic facts.
- **Field-granular provider read-sets** (DdsWorkedTransformation refinement) for Bazel-grade
  cutoff (util's compile reads `CcInfo.hdrs`, not `.link`).

**glob / add-remove rules = REIFICATION, not "predicate read-sets" (the correction):**
Skyframe does **not** track predicates in read-sets. It **reifies the predicate as a memoized
node**: a non-hermetic `DirectoryListing` node (the dirent set) → a `Glob` node that reads it
and recurses → `PackageLoad` reads the glob result by an *ordinary* edge.
- **Add a non-matching file** ⇒ dirties the listing, but the glob's matched-set value
  `.equals` the old ⇒ change-pruning stops it ⇒ package `VERIFIED_CLEAN`. O(affected),
  **with no predicate in any read-set.**
- **Add a matching rule / file** ⇒ glob result changes ⇒ exactly its readers recompute.
- **Remove a rule** ⇒ its EDB rows delete ⇒ the package/glob node value changes ⇒ consumers
  re-resolve (missing dep → typed error fact).
- **Edit an attr** ⇒ one `Attr` row ⇒ readers of that attr recompute, field-granular cutoff
  downstream.

So razel-specific parts reduce to: (a) **demand-driven** (top-down) so we never materialize
the whole repo (F10); (b) **field-granular** provider read-sets; (c) the glob/package
**reification** above. Predicate read-sets become an *optional optimization*, not the base
mechanism. Everything else is Skyframe's shipped, proven protocol.

---

## 5. Is it all-encompassing? — answered against Bazel's core (see `DdsCoveringSet.md`)

**Finite — PROVEN by enumeration:** Bazel's rule-facing contract is a closed 39-interface /
103-type graph, no FFI escape. The covering set is its closure. **The engine is a known kind:**
Skyframe = the IVM above. **The value/merge algebra is CLOSED at the §1a monoids** (no 6th
merge kind in the API).

**The four ⚠, resolved against real code:**
1. **java `exports`** — **✅ closed**: a plain `fold` over the `exports` edge-kind reading the
   *same* field (`java_info.bzl:287`); no splice operator. (The one most feared — it closes.)
2. **cc feature config** — **needs `Constrain`** (§1c, §3): a constraint-solver UDF, not a
   fold; the one new construct (`FeatureSelection.java:145-251`).
3. **proto aspect-traversal** — **still ⚠**, one trace left.
4. **android override** — **native engine merge, not a rule-facing monoid** (not on the
   Starlark API); precise semantics one trace left.

**The genuine remainder is NOT query-types/monoids — it is three ENGINE-feature axes**
(`DdsCoveringSet.md` §6), each bounded:
- **Config/edge derivation** — **transitions** (the load-bearing deferred one: per-edge +
  *split*, beyond `in_instance`/AD8's exec/host/whole-graph-mode), dormant deps, computed/
  late-bound defaults.
- **Production composition** — rule extension (`parent`/`super`) + subrules (inheritance).
- **Execution-phase dynamism** — `map_directory` / **tree-artifacts** / `unused_inputs_list`:
  data-dependent action I/O the static analysis-time graph doesn't model. **The other
  load-bearing one.**

**So the closed-set claim holds for value + production + engine** (with `Constrain` added);
the residual is three named engine features, of which **transitions and tree-artifacts** are
the two a faithful cc/java/go eventually forces. Two small traces (proto aspect, android
override) remain to *prove* the value/production set fully closed.

---

## 6. The corpus — what's on disk, what to gather, the validation

On disk now: **`rules_rust`** (full), **cc** (Bazel Java provider/API), **buck2 prelude**
(go/java/sh cross-ref). **Not** on disk: `rules_cc` (the `.bzl` impls), `rules_go`,
`rules_java`, `rules_shell`, `rules_android` (aidl/aapt/aapt2), `rules_proto`/`protobuf`. So
"gather literally all" needs cloning the canonical repos (network).

**Validation plan (turns the ⚠ into ✅ or a 6th feature):** once the corpus is on disk, run
the `DdsWorkedTransformation`-style **row-level** trace for one representative target of each
ruleset, *expressed in the §2 notation*, and check whether it stays inside the §3 covering
set. The four ⚠ + transitions are the targets that decide whether the set is closed. If all
five express without a new construct, the covering set is **proven closed** and the engine
(known kind) + the productions (declarative) make the rest a small, pointed test set — your
prediction, validated rather than asserted.

*This is the query system and the covering set. The remaining work is to gather the corpus
and run the four ⚠ cases through this notation to close the completeness claim.*
