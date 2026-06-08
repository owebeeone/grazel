# RazelStarlarkBoundaryPlan — the Starlark ⟷ razel boundary

Status: plan (2026-06). The umbrella over how razel provides Bazel-compatible rules. Folds in the
earlier `RazelCcRules` spec as the **first instance** of a generic model. Companions:
`BazelCcCommandLine.md` (the `Constrain` feature-config), `RazelParityHarness.md` (the golden
harness).

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
| **rust** | `ctx.actions.run`/`args`, no `rust_common`                          | generic API |
| **proto**| protoc via `ctx.actions.run`, `ProtoInfo`; no `proto_common.compile` (7.x) | generic API |
| **python**| `ctx.actions.{run,args,declare_file,symlink}` + thin `py_internal` bridge (~8 runfiles/launcher utils) | generic API + small bridge |

Heuristic that predicts it: native-backed ≈ Bazel's *historically-native* languages (cc, java,
proto, objc, android — native before Starlarkification). Pure-Starlark ≈ third-party/newer (rust,
go, shell, and most custom rules). For anything new, run the grep.

## 2. The unifying model: one generic build engine

Strip the facades and `cc_common.compile`, `java_common.compile`, the protoc action, and the rustc
action are **the same four moves**:

| Universal move | razel primitive (built for cc) | per-language = **data** |
|---|---|---|
| 1. resolve a **toolchain** (tools + feature/flag config) | a `CcToolchainInfo`-shaped handle | toolchain config (evaluated from its `.bzl`) |
| 2. assemble a **command line** from features + variables | **`Constrain`** (general flag-expansion DSL) | the feature/flag config (cc's rich one; javac/protoc/rustc are subsets) |
| 3. register an **action** (tool + argv + inputs → outputs) | the **derive** / `ctx.actions.run` | action names |
| 4. build a **transitive-info provider** (depsets folded over deps) | **producer + `fold_set` + the value algebra** | the provider schema (`CcInfo`/`JavaInfo`/`ProtoInfo`/`PyInfo`) |

Nothing in those primitives is cc-specific — we built them *for* cc but they are the
language-agnostic commons. So the "6–7 languages" are **one engine + N data configs + thin `.bzl`
rules**, not N hand-coded backends. Call the exposed engine **`razel_build.*`** (§5). Languages
become declarations over it — the declarative north star at the meta level: *the engine is the
Rust dialect; the languages are data.*

## 3. One track, drawn on to different depths ("run it ++")

This collapses the old "reimplement vs run-it" split into **one track** — everything runs its
`.bzl` over `razel_build`, differing only in how much of the surface it draws:

- **rust / go** → bare primitives (`ctx.actions.{run,args}`, `depset`). razel runs the real
  upstream `.bzl` unchanged.
- **proto / python** → bare primitives + a little of the commons (`ProtoInfo` construction; a
  `py_internal` runfiles bridge). Real upstream `.bzl`, run over razel's builtins.
- **cc / java** → the *full* commons (`configure_features` → `command_line` → transitive `info`).
  Their upstream `.bzl` is a native pointer (`cc_common`/`java_common` aren't *in* the files), so
  razel substitutes its **own** thin `.bzl` rule layer over the same `razel_build` surface.

cc and java stop being "reimplement a backend" and become *config + a thin razel `.bzl` over the
engine* — the same shape as rust, just drawing more of the ++.

## 4. The compat boundary (held exactly)

- **Rule API** — `cc_library`/`java_library`/… attrs. BUILD files portable.
- **Providers** consumed downstream — `CcInfo`/`JavaInfo`/`ProtoInfo`/`PyInfo`/`DefaultInfo`/
  `*ToolchainInfo` (real surface: in rules_cc, `CcInfo` 99×; rules_java `JavaInfo` 170×; rules_python
  `PyInfo` 195×).

Explicitly **not** held: the `<lang>_common` method surfaces (cc_common's `create_linker_input`/
`library_to_link`/`get_memory_inefficient_command_line` zoo). Those are razel's to design tight.

## 5. The generic surface `razel_build.*`

The engine, exposed to Starlark. Per-language specializations (`razel_cc`, `razel_java`) are thin
parameterizations — the *config* differs, not the engine.

```python
razel_build.toolchain(ctx, lang) -> ToolchainInfo
    # tools + the evaluated FeatureConfig + naming/host params (cfg, sdk, repo, target).

razel_build.command_line(toolchain, action, features, variables) -> [str]
    # Constrain.select + command_line. The clean replacement for get_memory_inefficient_command_line.

razel_build.action(ctx, *, tool, argv, inputs, outputs, mnemonic)
    # the derive / ctx.actions.run with the assembled argv (the DECLARED graph; §7 exec note).

razel_build.info(*, schema, direct, deps) -> Provider
    # construct + transitively merge a typed provider (value algebra: Set folds; OrderedDepset for
    # ordered fields — link/classpath order). Backed by the producer + fold_set.
```

`CcInfo`/`JavaInfo`/`ProtoInfo` are `razel_build.info` with different **schemas** — not different code.

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
  values (via `razel_build.toolchain`) — **not** the hand-ported `cc_macos_core.bzl`. This plan
  subsumes "kill the hand-port."
- Runs through the **existing** `rule()` evaluator (`analyze_starlark` already runs user rule `_impl`s
  with `ctx.actions`); just register the builtins and have `BzlLoader` serve razel's `cc:defs.bzl`.
- Already parity-proven against `cc/transitive` (argv + outputs + source-inputs); this re-homes the
  proven `derive` behind the builtins.

java is the second instance — same shape, `razel_java` over `razel_build`, JavaInfo schema.

## 7. Where the `.bzl` live — the bundling decision

razel's rule implementation is **split across a language boundary**: Rust builtins (`razel_build.*`)
+ a Starlark rule layer (razel's `cc:defs.bzl`). These two halves are **one implementation** and must
version atomically — the `.bzl` is a *contract with the binary's builtin API*.

- **razel's own `.bzl`** (the native-backed instances: cc, java) → **bundled into the binary**
  (`include_str!`/`include_dir!`); `BzlLoader` serves them for the razel-owned load prefixes
  (`@rules_cc//cc:defs.bzl`, `@rules_java//…`). Atomic versioning; zero-fetch install; small (the
  weight is in the Rust builtins). A dev/disk **override** is the escape hatch — *deferred*.
- **Upstream run-it rulesets** (rust, go, proto, python) → **fetched + pinned** per the project's
  `MODULE.bazel` (the version-drift rule from before still holds — these are third-party,
  project-versioned). razel runs them over the bundled Rust builtins.
- **Rust builtins** → always in the binary (they *are* razel).

So: bundle what razel *authors*; fetch what razel *runs*.

## 8. The extension-hook tail (the honest ~10–20%)

The generic engine is ~80–90%; the rest is a **small set of per-language hooks**, not backends:
apple Xcode/SDK resolution, java ijar/header-jars + classpath reduction, cc include-scanning/`.d`
parsing, proto plugin protocol, python launcher/runfiles. Each is a bounded hook feeding the generic
surface — designed in, not a parallel implementation.

## 9. Build sequence

1. **`razel_build` surface** — generalize `Constrain`/`derive`/`producer` into the four-move API;
   land **`OrderedDepset`** in the value algebra (link/classpath order — the reserved monoid).
2. **cc as first instance** — `razel_cc` over `razel_build`; razel's `cc:defs.bzl`; **bundle it**;
   `BzlLoader` serves it; `analyze_bazel` flows through the `.bzl`; parity vs the golden.
3. **Config eval** — `razel_build.toolchain` evaluates the real `cc_toolchain_config_lib.bzl` +
   `local_config_cc` (retires `cc_macos_core.bzl`).
4. **Run-it fidelity** — close the generic-Starlark-API gaps (`Args.before_each`/`format_each`,
   `OrderedDepset`, toolchain resolution) → run the **real** `rules_rust` over `razel_build`
   (retiring `derive_rust_library_action`, the bridge/parity-proof).
5. **java** (second native instance) + **proto/python** (run-it ++ with the small bridges).
