# DDS — the full transformation, worked end-to-end (no hand-waving)

The build engine, modeled as **a fact schema + recursive queries + a lowering function +
incremental view maintenance**. This runs the DDS on a real 5-target, 2-language case, all
the way to **file-level actions** and a traced **incremental update**. It is the concrete
answer to: (a) define the schema, (b) populate it, (c) transform into build-graph rules,
(d) how they update, (e) the design.

**The case** (exercises every strain from `BzlToDdsValidation.md`):
```
//cc:base   (cc.library)   srcs=base.cc hdrs=base.h
//cc:util   (cc.library)   srcs=util.cc hdrs=util.h   deps=[//cc:base]      # transitive cc + link order (R-α)
//mac:derive(rust.proc_macro) srcs=derive.rs                                # built in the EXEC instance
//app:lib   (rust.library) srcs=lib.rs  deps=[//cc:util] proc_macro_deps=[//mac:derive "derive"]  # cross-lang + per-edge alias + cross-instance (R-β)
//app:main  (rust.binary)  srcs=main.rs deps=[//app:lib]
```
Instances: everything in target instance **`I_t`** (e.g. linux-opt); the proc-macro is in the
**exec instance `I_x`**.

---

## (a) The schema — the DDS as typed relations

```
== INPUT stratum (asserted by loading + the Bazel adapter; the "build definition facts") ==
Target   (tk PK, instance, label, kind, pack)
Attr     (tk→Target, name, value, merge_class)                 # properties; merge ∈ Scalar|Set|OrdList|Map|OrdDepset
SrcFile  (tk→Target, file SourceKey, role)                     # role ∈ src|hdr ;  SourceKey = {repo,path} (config-independent)
DepEdge  (consumer→Target, dep→Target, kind, role, alias)      # R-β: edge-typed.  kind ∈ deps|exports|runtime_deps|proc_macro_deps

== DECLARATION stratum (the dialect; hashed into DeclarationSetId) ==
ProviderSchema (ptype PK, schema_id, fields[])                 # CcInfo, CrateInfo, DepInfo …  (closed FieldType universe + Map + OrdDepset, R-α)
TargetKind     (kind PK, attr_schema, lower)                   # cc.library→ccLower, rust.library→rustLower …
Toolchain      (id, kind, features)                            # cc-toolchain (feature config = a Derived fixpoint, R-γ)

== DERIVED stratum (produced by the queries below; EVERY derived row carries read_set + provenance) ==
Provider     (tk→Target, ptype, value)                         # ATOMIC provider fact (value = a typed record; vindicated by java)
Action       (ak PK, target→Target, kind, argv, env, tool, exec_instance)
ActionInput  (ak→Action, file, logical_path)                   # (path,digest) pairs — B5
ActionOutput (ak→Action, file)
ActionDep    (ak→Action, dep_ak→Action)                        # the file-level action DAG
```
This is the whole schema. It *is* simple — your point (1) stands.

---

## (b) Populated INPUT rows (the build definition, as data)

```
Target:  (t_base,I_t,//cc:base,cc.library,cc) (t_util,I_t,//cc:util,cc.library,cc)
         (t_lib,I_t,//app:lib,rust.library,rust) (t_main,I_t,//app:main,rust.binary,rust)
         (t_drv,I_x,//mac:derive,rust.proc_macro,rust)              # ← exec instance
SrcFile: (t_base,base.cc,src)(t_base,base.h,hdr)(t_util,util.cc,src)(t_util,util.h,hdr)
         (t_lib,lib.rs,src)(t_main,main.rs,src)(t_drv,derive.rs,src)
Attr:    (t_util,copts,{-O2},Set) (t_base,copts,{-O2},Set) …
DepEdge: (t_util, t_base, deps,           cclib,      —)
         (t_lib,  t_util, deps,           cclib,      —)            # cross-LANGUAGE (rust→cc)
         (t_lib,  t_drv,  proc_macro_deps, proc-macro, "derive")    # cross-INSTANCE (dep@I_x) + per-edge ALIAS
         (t_main, t_lib,  deps,           rlib,       —)
```

---

## (c) The transform — productions = recursive queries + a lowering fn → the file-level graph

### The two recursive queries (datalog form; the transitive closures)
```
# ordered cc link closure (post-order: deps' closure first, self's archive last → correct link order, R-α)
cc_link(T) :=  ( ⨁ over DepEdge(T,D,deps): cc_link(D) )  ++  [ archive_of(T) ]      # ⨁ = ordered concat, dedup-keep-first
# transitive cc headers (for compile -I), set-union (order-insensitive)
cc_hdrs(T) :=  hdrs(T)  ∪  ( ⋃ over DepEdge(T,D,deps): cc_hdrs(D) )
```

### The lowering fn per target-kind (reads input facts + deps' providers *per edge*, emits rows)
`ccLower(T)` and `rustLower(T)` are `lower(target, &FactView) -> Declared`. They **walk
`DepEdge` as typed edges** (R-β) and call the recursive queries. Running them over (b) yields
the DERIVED rows — i.e. **the file-level build graph**:

```
Provider (atomic):
  (t_base, CcInfo,   {hdrs:[base.h], link:[libbase.a]})
  (t_util, CcInfo,   {hdrs:[util.h, base.h], link:[libutil.a, libbase.a]})         # cc_hdrs/cc_link recursion
  (t_drv@I_x, CrateInfo, {name:derive, out:libderive.so, type:proc-macro})
  (t_lib,  CrateInfo, {name:lib, out:liblib.rlib, type:rlib})
  (t_lib,  DepInfo,   {direct:[{crate:lib}], cc_link:[libutil.a,libbase.a]})        # carries the cc closure forward
  (t_main, CrateInfo, {name:main, out:main, type:bin})

Action / ActionInput / ActionOutput / ActionDep (THE FILE-LEVEL GRAPH):
  a_base_c  compile  argv=[clang,-O2,-c,base.cc,-o,base.o]                in=[base.cc,base.h]            out=[base.o]
  a_base_ar archive  argv=[ar,rcs,libbase.a,base.o]                      in=[base.o]                   out=[libbase.a]   dep=a_base_c
  a_util_c  compile  argv=[clang,-O2,-c,util.cc,-Iinc,-o,util.o]         in=[util.cc,util.h,base.h]    out=[util.o]      # reads base.h (transitive hdr), NOT base.o
  a_util_ar archive  argv=[ar,rcs,libutil.a,util.o]                      in=[util.o]                   out=[libutil.a]   dep=a_util_c
  a_drv@I_x compile  argv=[rustc,derive.rs,--crate-type=proc-macro,-o,libderive.so]  in=[derive.rs]    out=[libderive.so]
  a_lib     compile  argv=[rustc,lib.rs,--crate-type=rlib,--crate-name=lib,
                           --extern=derive=libderive.so,                 # ← alias from DepEdge, path into I_x
                           -lstatic=util,-lstatic=base,-o,liblib.rlib]    in=[lib.rs, libderive.so(@I_x), libutil.a, libbase.a]  out=[liblib.rlib]
                                                                          dep=[a_drv, a_util_ar, a_base_ar]
  a_main    link     argv=[rustc,main.rs,--crate-name=main,--extern=lib=liblib.rlib,
                           -lstatic=util,-lstatic=base,-o,main]           in=[main.rs, liblib.rlib, libutil.a, libbase.a]        out=[main]
                                                                          dep=[a_lib, a_util_ar, a_base_ar]
```
That is the complete file-level DAG, **derived purely by the queries + lowering** from (b).
Note the concrete strains *resolved as data*: `--extern=derive=…` is the per-edge **alias**
(R-β); `libderive.so(@I_x)` is the **cross-instance** input (R-β); `[libutil.a, libbase.a]`
is the **ordered** link closure (R-α); `a_util_c` reads `base.h` **but not** `base.o` — the
file-level precision that makes (d) work.

### Each derived row records its `read_set` (the query's footprint — this is the load-bearing bit)
```
a_util_c.read_set = { SrcFile(t_util)={util.cc,util.h},  Attr(t_util,copts),  Provider(t_base).CcInfo.hdrs,  Toolchain(cc).id }
a_lib.read_set    = { SrcFile(t_lib),  DepEdge(t_lib,*),  Provider(t_util).CcInfo.link,  Provider(t_drv@I_x).CrateInfo.out, … }
```
**Crucial:** read-sets are **field-granular on providers** — `a_util_c` read
`CcInfo(t_base).hdrs`, *not* `.link`. (This is the one refinement the worked example forces;
see (e).)

---

## (d) How they update — incremental view maintenance, traced to the file

**Edit 1 — `base.cc` changes (impl only).** `SourceKey(base.cc)` digest flips.
- Invalidate rows whose read_set ∋ `base.cc`: only `a_base_c`. Re-run → new `base.o` → flips
  `a_base_ar`'s read_set (`base.o`) → re-run → new `libbase.a`.
- `Provider(t_base).CcInfo`: `.hdrs` **unchanged**, `.link`=[libbase.a] **digest changed** → the
  *fact value's `.link` field* changes, `.hdrs` doesn't.
- `a_util_c.read_set` ∋ `CcInfo(t_base).hdrs` (unchanged) and ∌ `base.cc`/`base.o` → **NOT
  recomputed.** ← *file+field-level early-cutoff: util's compile is provably unaffected.*
- `a_lib`/`a_main` read_set ∋ `libbase.a` (changed) → **re-link only.**
- **Net: recompute {a_base_c, a_base_ar, a_lib, a_main}; skip {a_util_c, a_util_ar, a_drv}.**

**Edit 2 — `base.h` changes (header).** `SourceKey(base.h)` flips.
- read_set ∋ `base.h`: `a_base_c` **and** `a_util_c` (it read `base.h` via `cc_hdrs` recursion).
  Both recompute → `base.o`,`util.o` change → both archives → `a_lib`,`a_main` re-link.
- **Net: the header fan-out** (every compile that *read* the header) — exactly Bazel's
  behavior, **derived from read-sets**, not hard-coded.

**Edit 3 — add `proc_macro_deps=[//mac:derive2]` to `app:lib`.** New `DepEdge` row.
- `a_lib.read_set` ∋ `DepEdge(t_lib,*)` → re-run `rustLower(t_lib)` → new `--extern=derive2=…`,
  new input → `a_lib` recomputes; `t_drv2@I_x` analyzed in the exec instance. Everything below
  `t_lib` (only `a_main`) re-links; cc untouched.

This is incremental view maintenance: **Δfact → invalidate the rows whose read_set intersects
Δ → re-run those productions → early-cutoff where the re-derived value-digest is unchanged.**

---

## (e) The design (how the above maps onto the contracts, precisely)

- **DDS = the relations.** `DdsRead`/`FactView` = (recursive) `SELECT`; `DdsWrite.commit` =
  the production inserting its derived rows atomically (§0).
- **A production = a `TargetKind.lower`** — a parameterized query: it reads input facts +
  deps' providers **per typed edge** (R-β), invokes the recursive closures, and **returns**
  `Declared{providers, actions, facts}`. Purity holds: it gets `&FactView`, returns rows; the
  evaluator commits (the forcing wall). The recursion over `DepEdge` is the transitive query;
  the cc feature config is a **`Derived` fixpoint** computed before `ccLower` (R-γ).
- **`read_set` = the query's dependency footprint**, recorded as the engine memo key (the
  digest-metadata model, 55-5 H1). The engine (`razel-engine`) = the **incremental view
  maintainer**: index rows by the facts their read_set touches; on Δfact, dirty the
  intersecting rows, re-run, early-cutoff on unchanged value-digest. That is `Analyze`/
  `ActionPlan`/`ActionExec` nodes (§7) — now concretely *what they compute and re-compute*.
- **Cross-instance** is just a `DepEdge` whose `dep.tk` carries a different `instance`
  (`t_drv@I_x`); the query reads `Provider(t_drv@I_x)`; the `ActionInput` references the
  `I_x` output file. The keys already make this representable (§1).
- **"Queries to file level"** = the productions bottom out at `SrcFile` and emit
  `Action`/`ActionInput`/`ActionOutput` rows — the file-level DAG above.

### The one refinement this exercise forces (earned, not hand-waved)
**Read-sets must be field-granular on providers, not provider-granular.** Edit 1 only achieves
correct early-cutoff because `a_util_c` recorded that it read `CcInfo(t_base).hdrs` and *not*
`.link`. If read-sets were whole-provider, *any* change to `CcInfo(t_base)` (incl. the
`.link` digest) would needlessly recompile `util` — losing Bazel-level precision. So the
atomic provider fact is the **storage/merge** unit (vindicated), but the **read-set** tracks
**provider-field reads** (a sub-fact granularity). This tightens §0's read-set lifecycle and
55-5 H1 — and it is exactly the kind of concrete requirement the abstract contracts could not
have surfaced.

---

## Verdict on "is it simple / am I missing something"

- **Schema (a) + one-shot queries (c-static): genuinely simple and mechanical** — shown above.
- **Not simple:** the queries are **recursive + one fixpoint**; the productions **synthesize
  rows via an imperative lowering fn**; and **the update (d) is real incremental view
  maintenance with file+field-level read-set precision** — the actual engine, and the thing
  every serious build tool (Skyframe/Salsa/Buck2/Pants) is fundamentally *about*.
- **The framing is right and it's a strength:** "incremental maintenance of recursive
  provenance-carrying views over a fact schema" is a precise, known, solvable model — which is
  what turns the prior hand-waving into a design we can actually build and test.

*This is the worked transformation. It also surfaced one real refinement (field-granular
read-sets) — which is the point of doing it concretely.*
