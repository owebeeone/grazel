# RazelStarlarkBoundaryPlan — the Starlark ⟷ razel boundary

Status: plan (2026-06; rev. after independent review — `scratch/RazelStarlarkBoundaryPlan-Review48.md`).
The umbrella over how razel provides Bazel-compatible rules. Folds in the earlier `RazelCcRules`
spec as the **first instance** of a generic model. Companions: `BazelCcCommandLine.md` (the
`Constrain` feature-config), `RazelParityHarness.md` (the golden harness).

## 0. The question

A BUILD file `load("@rules_cc//cc:defs.bzl", "cc_library")`s and calls rules. Where does that
bottom out, and what must razel supply? The answer differs per ruleset, and the difference is
**empirical, not assumed** — decided by one grep.

## 1. The tell (verified)

Does a ruleset's rule `_impl` bottom out in a **rule-specific native builtin** (`<lang>_common.*`
/ `native.<lang>_library`), or in the **generic rule-authoring API** (`ctx.actions.*`, `depset`,
providers, toolchains)? Grepped from fetched source:

| Ruleset | Tell | Bottoms out in |
|---|---|---|
| **cc**   | `cc_common` / `native.cc_library`                                   | native backend |
| **java** | `java_common.compile`/`.merge`, `JavaInfo` (170×), `→ native.java_library` | native backend |
| apple/objc/swift | `apple_common` (hit via cc's SDK)                          | native backend |
| **rust** | `ctx.actions.run`/`args`; `rust_common` exists but is a Starlark `struct` (`rust/private/common.bzl:66`), **not** a native builtin | generic API |
| **proto**| protoc via `ctx.actions.run`, `ProtoInfo`; no `proto_common.compile` in 7.x — **provisional: rests on `com_google_protobuf`, not yet fetched (confirm in Phase E)** | generic API (provisional) |
| **python**| `ctx.actions.{run,args,declare_file,symlink}` + a *thin* `py_internal` bridge (~8 runfiles/launcher utils) | generic API + small bridge |

Heuristic that predicts it: native-backed ≈ Bazel's *historically-native* languages (cc, java,
proto, objc, android — native before Starlarkification). Pure-Starlark ≈ third-party/newer (rust,
go, shell, and most custom rules). For anything new, run the grep.

## 2. The unifying model: one generic build engine

Strip the facades and `cc_common.compile`, `java_common.compile`, the protoc action, and the rustc
action are **the same four moves**:

| Universal move | razel primitive (built for cc) | per-language = **data** |
|---|---|---|
| 1. resolve a **toolchain** (tools + feature/flag config) | a `CcToolchainInfo`-shaped handle | toolchain config (evaluated from its `.bzl`) |
| 2. assemble a **command line** from features + variables | **`Constrain`** (flag-expansion DSL) | the feature/flag config |
| 3. register an **action** (tool + argv + inputs → outputs) | the **derive** / `ctx.actions.run` | action names + action *shape* |
| 4. build a **transitive-info provider** (depsets folded over deps) | **producer + `fold_set` + the value algebra** | the provider schema |

The framing is right, and `Constrain`/derive/producer are genuinely not cc-specific. So the target
is **one engine + N data configs + thin `.bzl` rules**, not N hand-coded backends — the declarative
north star at the meta level: *the engine is the Rust dialect; the languages are data.*

### 2a. Calibration (from the independent review — load-bearing)

The four-move framing is true at a level abstract enough to be nearly tautological. What's
load-bearing is whether razel's **specific primitives** fit a *second* language. Two are currently
**cc-shaped**, so "languages are just data" is a **target to validate (Phase B), not a settled
fact**:

- **The value algebra under-covers `JavaInfo`.** `razel-dds`'s `FieldValue` is `Scalar | Set` only;
  **`OrderedDepset` is reserved, not built.** Real `JavaInfo` carries multiple *order-significant,
  non-cross-merging* depsets (compile-jars vs runtime-jars), nested *struct* providers
  (annotation-processing, module flags), and *conditional* merge (`neverlink`). That's real engine
  work, not a new schema.
- **The action *shape* differs.** javac compiles a whole module in **one** action (classpath as a
  flag); cc is **per-source** compile + archive. `razel_build.action` must not bake in cc's
  one-action-per-source assumption.
- **The "uniform" move 1 (toolchain) is the least-built seam.** `rule()` *discards* `attrs`
  (`rules.rs:600`; "the schema is not consulted") and has **no `toolchains`**; deps resolve only to
  `DefaultInfo.files`, not arbitrary providers. So move 1 + dep→provider have no real live mechanism.
  Resolution — depth per track (the **"middle" lean**): *shallow* for razel's authored rules (Phase A:
  a thin `razel_build.toolchain` accessor + dep→`CcInfo` + attr-*kinds*); *full* for run-it (Phase D:
  real schema/coercion/`cfg`/multi-label + `ctx.toolchains`/registration/platforms).

Honest read: the abstraction is probably right, but it must be **extracted from two instances**
(§9/§10), not extrapolated from cc.

## 3. One track, drawn on to different depths ("run it ++")

This collapses the old "reimplement vs run-it" split into **one track** — everything runs its
`.bzl` over `razel_build`, differing only in how much of the surface it draws:

- **rust / go** → bare primitives (`ctx.actions.{run,args}`, `depset`). *Target:* razel runs the
  real upstream `.bzl`. **Reality check:** today `razel-cc-toolchain/src/rust.rs` is a hand-written
  argv *template* (the parity bridge), **not** run-it. Running the real `rules_rust` (~2800-line
  `rustc.bzl`) needs `Args` fidelity razel lacks — `map_each`/`format_each`/param-files/tree
  artifacts (Phase D). The fidelity bar is higher than "just point the loader at it."
- **proto / python** → bare primitives + a little of the commons (`ProtoInfo` construction; a
  `py_internal` runfiles bridge). Real upstream `.bzl`, run over razel's builtins.
- **cc / java** → the *full* commons (`configure_features` → `command_line` → transitive `info`).
  Their upstream `.bzl` is a native pointer (`cc_common`/`java_common` aren't *in* the files), so
  razel substitutes its **own** thin `.bzl` rule layer over `razel_build` + the §8 hook tail.

## 4. The compat boundary (held exactly)

- **Rule API** — `cc_library`/`java_library`/… attrs. BUILD files portable.
- **Providers** consumed downstream — `CcInfo`/`JavaInfo`/`ProtoInfo`/`PyInfo`/`DefaultInfo`/
  `*ToolchainInfo` (real surface: `CcInfo` 99×, `JavaInfo` 170×, `PyInfo` 195×). The provider
  contract must stay stable across the **bundled-`.bzl` ⟷ fetched-ruleset** boundary (razel's
  bundled cc `.bzl` emits `CcInfo`; fetched `rules_rust` consumes it) — a versioning constraint, not
  a free lunch.

Explicitly **not** held: the `<lang>_common` method surfaces (cc_common's `create_linker_input`/
`library_to_link`/`get_memory_inefficient_command_line` zoo). Those are razel's to design tight.

## 5. The generic surface `razel_build.*`

The engine, exposed to Starlark. Per-language specializations (`razel_cc`, `razel_java`) are thin
parameterizations — the *config* differs, not the engine. **Frozen only after Phase B (two
instances).**

```python
razel_build.toolchain(ctx, lang) -> ToolchainInfo
    # tools + the evaluated FeatureConfig + naming/host params (cfg, sdk, repo, target).

razel_build.command_line(toolchain, action, features, variables) -> [str]
    # Constrain.select + command_line. The clean replacement for get_memory_inefficient_command_line.

razel_build.action(ctx, *, tool, argv, inputs, outputs, mnemonic)
    # ctx.actions.run with the assembled argv (the DECLARED graph; §6 live-path / §7 exec notes).
    # NOTE: must support whole-module (java) AND per-source (cc) shapes — not cc-baked.

razel_build.info(*, schema, direct, deps) -> Provider
    # construct + transitively merge a typed provider. Value algebra: Set folds today;
    # OrderedDepset (ordered, non-cross-merging — classpath/link order) is RESERVED, lands Phase B.
```

`CcInfo`/`JavaInfo`/`ProtoInfo` are `razel_build.info` with different **schemas** — *once* the algebra
covers ordered + nested fields (Phase B).

## 6. First instance: cc (folds `RazelCcRules`)

`razel_cc` is `razel_build` specialized to cc's (richest) feature config. razel ships its **own**
`cc:defs.bzl` over it:

```python
def _cc_library_impl(ctx):
    tc   = razel_build.toolchain(ctx, "cc")
    deps = [d[CcInfo] for d in ctx.attr.deps]
    comp = razel_cc.compile(ctx, srcs = ctx.files.srcs, public_hdrs = ctx.files.hdrs,
                            deps = deps, copts = ctx.attr.copts)        # → objects + headers
    arch = razel_cc.archive(ctx, name = ctx.label.name, objects = comp.objects)  # → lib<name>.a
    return [
        razel_build.info(schema = CcInfo, direct = struct(headers = comp.headers,
                         library = arch.library), deps = deps),
        DefaultInfo(files = [arch.library]),
    ]
```

- `razel_cc.compile`/`archive` = `razel_build.action` + the path model, with the cc feature config.
- The config feeding `Constrain` is the **evaluated** `cc_toolchain_config_lib.bzl` + `local_config_cc`
  (via `razel_build.toolchain`) — **not** the hand-ported `cc_macos_core.bzl`. Subsumes "kill the hand-port."
- Runs through the **existing** `rule()` evaluator; register the builtins, have `BzlLoader` serve the
  bundled `cc:defs.bzl`.

### 6a. The live-path gap (from the review — must close in Phase A)

"We already built cc" is true of the **engine**: `Constrain` + the `derive` + the path model
reproduce the golden, and the parity runner proves it. It is **not** true of the path the CLI runs:
`analyze_bazel`'s cc lowering is a *second, hardcoded* backend (the `/usr/bin/c++ -iquote …` argv in
`razel-loading/src/rules.rs`) that **bypasses `Constrain` entirely** — and rust is symmetric: a
separate live `/usr/bin/rustc` backend (`rust_rules.rs`) distinct from `rust.rs`'s template. Phase A
makes the **live cc** path go through the engine + the bundled `.bzl` (Phase D does the same for
rust), so the runner checks the real output, not a sidecar.

Concretely, `rule()` also **discards the impl's return** (`rules.rs:576`) — providers are captured
by side-effect, only `default_info` survives — so routing cc through the engine first needs the
**provider-capture change (A2a)**, the eval-model dependency the live path rests on.

## 7. Where the `.bzl` live — the bundling decision

razel's rule implementation is **split across a language boundary**: Rust builtins (`razel_build.*`)
+ a Starlark rule layer (razel's `cc:defs.bzl`). These two halves are **one implementation** and must
version atomically — the `.bzl` is a *contract with the binary's builtin API*.

- **razel's own `.bzl`** (cc, java) → **bundled into the binary** (`include_str!`/`include_dir!`);
  `BzlLoader` serves them for the razel-owned prefixes. Atomic versioning; zero-fetch install; small
  (weight is in the Rust builtins). Dev/disk **override** = the escape hatch, *deferred*.
- **Upstream run-it rulesets** (rust, go, proto, python) → **fetched + pinned** per the project's
  `MODULE.bazel` (third-party, project-versioned). razel runs them over the bundled Rust builtins.
- **Rust builtins** → always in the binary (they *are* razel).

So: bundle what razel *authors*; fetch what razel *runs*.

**Declared-vs-executable — resolved by a per-language toolchain MODE** (revised after the ·ii spike
proved "just adopt the toolchain" breaks execution: a single faithful argv isn't runnable without a
materialized toolchain, and razel-build *runs* the graph). razel's cc rule is mode-parameterized:

- **Native** (default): resolve the host compiler by walking `PATH` (`cc`/`c++`/`clang++`), **log +
  digest** which tool was chosen, and run it directly with real paths → an **executable** graph. This
  is what razel-build runs. It's also exactly what Bazel's `cc_configure` does (probe the host), so
  razel-native ≈ Bazel-native for host-derived languages (cc, py). The tool's digest becomes a keyed
  action input (the host tool can change underneath you — RazelGaps "toolchain-change cache").
- **Adopt-Bazel**: the faithful **declared** graph (`cc_wrapper.sh` + `bazel-out`, the Constrain
  flags) over razel's `cc:defs.bzl`. Consumed by the graph-parity runner — which only *declares +
  diffs*, never executes — and, once the toolchain is materialized, hermetic builds.

The MODE *is* the resolution: the **parity** context wants faithful (Adopt-Bazel); the **build**
context wants runnable (Native); selected per-language, eventually `.razelrc`-driven (RazelGaps).
No forced one-argv split, no blocked build. (Materializing Bazel's toolchain so Adopt-Bazel executes
too — true `declared == executable` — stays the end goal, now a follow-on, not a prerequisite.) The
native resolution (PATH-walk + digest/log) is **in scope here**, not deferred.

## 8. The extension-hook tail (honesty: not a fixed %)

There IS a generic core + a per-language bespoke tail (apple Xcode/SDK, java ijar/header-jars +
classpath reduction, cc include-scanning/`.d`, proto plugins, python launcher/runfiles). The earlier
"~10–20%" was a guess; the review is right that it's optimistic for java and the run-it path. **We
don't estimate it — we measure it:** the Phase-B java spike produces an explicit generic-vs-bespoke
ledger (B5), and that's the real number. Each hook must be a bounded extension feeding the generic
surface, not a parallel implementation — but how much hook java needs is an output of the spike, not
an assumption.

**`CppModuleMap` — decided: scoped out, but asserted not silent.** razel does not model C++ modules
(layering-check / `.cppmap`). The graph-parity runner (§10 A0) asserts razel's action *set* equals
the golden's **minus an explicit allowlist `{CppModuleMap}`**, and **logs** the omission — a recorded
scope line, not a silently-missing action (the per-action-vs-per-graph gap Review49 caught). Modules
become a later feature behind that allowlist.

## 9. Build sequence (reordered — prove, then generalize)

The review's key correction: **don't freeze `razel_build` from one example.** Reordered so the
abstraction is extracted from *two* instances and the live path uses the engine first:

- **A — cc end-to-end on the real mechanism.** Starts with **A0, a *failing* graph-parity acceptance
  test** (AGENTS.md Rule 0); then `razel_cc` (concrete), bundled `cc:defs.bzl`, the **live** path
  through the engine, config evaluated — driving A0 green on the path the CLI runs.
- **B — java compile spike.** A genuinely different shape (whole-module javac, `JavaInfo`'s ordered
  non-merging depsets); lands `OrderedDepset`; produces the generic-vs-bespoke ledger. **The second
  data point that validates the abstraction.**
- **C — extract `razel_build`.** *Now* freeze the generic API, refactoring cc + java onto it.
- **D — run-it path.** Build **generic-`rule()` fidelity** (real `attrs` schema/coercion/`cfg`,
  `ctx.toolchains`/registration, `Args`, `depset`) → run the **real** `rules_rust`, retiring *both*
  rust backends (`rust.rs` template + the live `rust_rules.rs`).
- **E — breadth.** proto (also resolves §1's provisional verdict), python, link/classpath coverage.

## 10. Detailed roll-buildable plan

Each step is a green, committed, tagged (`razel-v2/…`) roll-build with an explicit parity/test gate.

### Phase A — cc end-to-end on the real mechanism
*No new abstraction (`razel_cc` concrete); de-risk the machinery + close the live-path gap (§6a).*
- **A0 — graph-parity runner (the failing acceptance test).** In `razel-parity` (kept pure): add
  `Action { mnemonic, argv, inputs, outputs }`, `parse_golden(normalized) -> [Action]` reading the
  **whole action set** (generalizes today's cherry-pick `golden_argv`), and `diff(razel, golden, omit)
  -> Report` — a **set** diff asserting `razel == golden − omit`, per-action argv (ordered) +
  inputs/outputs (sorted) equal, **logging** the `omit = {CppModuleMap}` allowlist (§8). A harness
  (test / `xtask parity`) wires `analyze_bazel` → render → diff. *Green:* the **runner** is
  unit-tested on synthetic actions (match/mismatch/missing/extra); **cc/transitive is a tracked RED
  baseline** (the gap A1–A4 close). Built first, red — AGENTS.md Rule 0. This is the A4 gate.
- **A0b — diff input-filter (A4 prerequisite).** A0's `diff` compares inputs exactly, but the
  golden's inputs include `external/<repo>/*` toolchain files + `.cppmap`s razel never emits. Filter
  those from inputs (both sides) before comparing, so parity needs only **source-level** inputs
  (toolchain/sandbox inputs = the §8 tail). *Green:* the filter is unit-tested; the cc/transitive
  baseline stays RED (argv/paths still differ).
- **A1 — builtin scaffold.** Register a `razel_cc` Starlark global namespace; one method
  (`command_line`) wrapping `Constrain`. *Green:* a `rule()` impl via `analyze_starlark` calls it and
  gets the golden argv tokens.
- **A2a — provider capture (eval-model change).** *Found by verifying `rules.rs:576`:* `rule()`
  discards the impl's return value; the target is captured by side-effect and only `default_info`
  survives. Change the eval to **capture the returned providers**, store `CcInfo` (OWN exported
  headers) on the target, and expose `dep[CcInfo]` to dependents (transitive set via `fold_set`).
  *Green:* a rule returning `[CcInfo(headers=…), DefaultInfo(…)]` makes `dep[CcInfo]` readable by a
  dependent; the transitive header set folds.
- **A2b — compile/archive builtins.** `razel_cc.compile`/`archive` register the declared
  `CppCompile`/`CppArchive` over `derive` + path model, drawing transitive headers from `dep[CcInfo]`
  (A2a). *Green:* a test rule emits the actions with the golden argv + outputs + source-level inputs
  (incl. transitive `base.h`).
- **A3+A4 — `cc:defs.bzl` + live switch (one integration).** Serving the bundled `.bzl` for
  `@rules_cc//cc:defs.bzl` *is* the live switch (it replaces native `cc_library`), so these aren't
  separable — a "bundle without switch" A3 is a dead artifact. Two internal greens keep it de-risked,
  not big-bang:
  - **·i (isolated logic).** Author `cc:defs.bzl` (`cc_library` `_impl` over `razel_cc.command_line`
    + the path model + `dep.headers` (A2a) + `ctx.actions.run(mnemonic=…)` (A3-prep)); test via
    `analyze_starlark` (package `""`). *Green:* the produced actions have the right **structure** —
    mnemonic `CppCompile`/`CppArchive`, argv[0] `cc_wrapper.sh`, the feature flags, the
    `_objs/<target>/<stem>.{o,d}` path *shape*, transitive headers in inputs. No live switch; can't
    golden-match (no package).
  - **·ii (live switch via toolchain MODE — §7).** `include_str!` the `.bzl`; `BzlLoader` serves it
    for `@rules_cc//cc:defs.bzl` **only in Adopt-Bazel mode**; **Native** (default) keeps the existing
    native `cc_library` (executable). The graph-parity runner (A0) runs `analyze_workspace` in
    Adopt-Bazel → faithful → matches the golden; razel-build runs Native → unchanged. **Open: live
    `cfg`** — `bazel-out/<cfg>` is a placeholder; pick a fixed segment `normalize()` maps to `<cfg>`.
    *Green:* **A0's runner goes GREEN on cc/transitive** (modulo `{CppModuleMap}`); razel-build +
    characterization stay green (Native untouched). *(This is the ·ii that the first spike got wrong —
    it replaced native globally and broke execution; the mode fixes that.)*
  - **·iii (native toolchain resolution — folded in, §7, not deferred).** Resolve the Native-mode cc
    tool by walking `PATH` (`cc`/`c++`/`clang++`), **log + digest** the chosen tool; retire the
    hardcoded `/usr/bin/c++`. *Green:* a resolver unit test (controlled candidates → first existing +
    its digest); Native cc uses the resolved tool; characterization asserts a real resolved compiler.
    (The digest → action-key fold is the follow-on, RazelGaps "toolchain-change cache".)
- **A5a — real config API (foundation).** razel bundles `cc_toolchain_config_lib` (the real
  constructors + `cc_common.create_cc_toolchain_config_info`), prepended in `parse_feature_config`;
  configs are written in the real API (`CONFIG = cc_common.create_cc_toolchain_config_info(features=…,
  action_configs=…)`). *Green:* the macOS config evaluates via the real API + A0 stays green.
- **A5b → Phase D — eat the ACTUAL generated config.** *Found by verifying the generated
  `cc_toolchain_config.bzl`:* it `load()`s real @rules_cc library modules (`cc_common`,
  `cc_toolchain_config_lib`, `feature_injection`, `cc_toolchain_config_info`) that razel currently
  *shims* — evaluating them + a `cc_common` builtin + the BUILD's ~20 host attrs **is** the run-it
  path. Folded into Phase D (run real ruleset Starlark), not a Phase-A bolt-on.

*Exit: a real BUILD's cc **declared** graph (Adopt-Bazel mode) is produced by the engine through
razel's bundled `.bzl`, **parity-proven against the golden**; config in the real cc_common API. NOTE
(F18): the **executed** path is **Native** mode (the PATH-resolved host compiler + simple flags) —
razel's runnable lowering, NOT the Bazel-faithful declared graph and NOT golden-tested (only the
Adopt-Bazel declared graph is). Converging them — running the engine's declared graph as the executed
graph (toolchain materialization) — is Phase C/D (RazelGaps "Native cc path parity"). The actual
generated config is likewise Phase D.*

### Phase B — java compile spike (validate the abstraction)
*The second data point — before any generic API is frozen.*
- **B1 — java golden.** Add `rules_java` `java_library` transitive to the corpus; capture the `Javac`
  golden + java-specific normalization. *Green:* golden committed, hermetic.
- **B2 — `OrderedDepset`.** Land the reserved ordered monoid in the value algebra (driven by javac
  classpath order). *Green:* ordered-fold tests (order-preserving, dedup-keeps-first, associative).
- **B3 — java compile spike.** Reproduce the `Javac` command line: **whole-module** (all srcs one
  action), classpath from deps' `JavaInfo` via `OrderedDepset`. *Green:* parity vs the java golden
  (argv + classpath order).
- **B4 — `JavaInfo`.** Model it: compile-jars vs runtime-jars as separate ordered depsets that **do
  not cross-merge**, the header/ijar slot, `neverlink` conditional. *Green:* merge tests (no
  cross-merge; neverlink respected).
- **B5 — spike retro (the ledger).** Write the explicit generic-vs-bespoke account for java (what fit
  `razel_build`'s shape; what needed a hook — ijar, whole-module action, dual depsets). *Green:* doc
  artifact; feeds Phase C + the §8 honest number.

*Exit: two worked instances (cc + java) + a measured account of where the abstraction held.*

### Phase C — extract `razel_build` (generalize from two instances)
- **C1 — four-move API.** Extract `razel_build.{toolchain,command_line,action,info}`; refactor cc +
  java onto it (`action` supporting both per-source and whole-module). *Green:* both parities hold
  through the unified surface.
- **C2 — provider engine.** `razel_build.info(schema, direct, deps)` generic over the algebra (Set +
  OrderedDepset + scalar + nested struct); `CcInfo`/`JavaInfo` become schemas. *Green:* both providers
  via the one constructor.
- **C3 — hook seam.** Formalize the extension points the ledger surfaced (toolchain resolver,
  action-transform for ijar/include-scan). *Green:* cc + java's bespoke bits sit behind hooks; an
  `xtask gates`-style check that no language name leaks into the engine core.
  **Design + staged plan (C3a registry / C3b toolchain hook / C3c gate): `dev-docs/RazelHookSeam.md`**
  — note the toolchain-resolver hook hits a real abstraction gap (cc `FeatureConfig` vs java
  template), so C3b ships an interim registered resolver and the generic `Toolchain` type is Phase D.

*Exit: a generic engine with two instances + a clean hook seam.*

### Phase D — run-it path (real upstream rules)
*The real price of run-it = generic-`rule()` fidelity (the §2a/§6a least-built seam, now in full).*
- **D1 — real `attrs` schema.** Honor the declared schema beyond A2's attr-*kinds*: types, defaults,
  `mandatory`, **multiple** label attrs (`deps`/`proc_macro_deps`/`crate`), `providers=`,
  `cfg=exec|target`, coercion. *Green:* a rule with a real schema resolves all label attrs to
  providers + applies defaults/coercion.
- **D2 — `ctx.toolchains` + resolution.** `rule(toolchains=…)` + registered toolchains + platform
  resolution → `ctx.toolchains[type]` (beyond A3's thin accessor); `CcInfo` consumed for cc-interop.
  *Green:* a rule reads its toolchain via `ctx.toolchains` + a cc dep's `CcInfo`.
- **D3 — `Args` fidelity.** `ctx.actions.args` gains `before_each`/`format_each`/`map_each` +
  param-file (`@argfile`). *Green:* `Args` unit tests; the rust template's argv reproduced via real
  `Args`. (`map_each` only bites here, per Review48/49 — the D1/D3 split sequences around it.)
- **D4 — run real `rules_rust`.** `BzlLoader` loads the **real** fetched `rules_rust` `.bzl` (drop the
  shim) for `@rules_rust//`; it runs over `razel_build` + the generic `rule()`. *Green:* **A0's runner
  (rust corpus) goes GREEN via the REAL `rules_rust`** — retiring *both* rust backends (`rust.rs`
  template + the live `rust_rules.rs`).

*Exit: a pure-Starlark ruleset runs unmodified over the engine.*

### Phase E — breadth
- **E1 — proto.** Fetch `com_google_protobuf` + `rules_proto`; run real `rules_proto` over the engine
  (`ProtoInfo`); golden. Resolves §1's provisional proto verdict.
- **E2 — python.** Run real `rules_python` + the small `py_internal` runfiles/launcher bridge; golden.
- **E3 — link/coverage.** `cc_binary`/`java_binary` (link/classpath order via `OrderedDepset`); more
  corpus goldens (multi-src, generated headers, `select()`).

Each phase is independently valuable; the ordering pays for the unification with two instances before
it can calcify.
