# RazelCcRules — razel's cc rules over a tight builtin API

Status: spec (2026-06). Companion to `BazelCcCommandLine.md` (the `Constrain` feature-config) and
`RazelParityHarness.md` (the golden harness). Defines how razel provides `cc_library`/`cc_binary`
**compatibly** without inheriting Bazel's cc internals.

## 1. Why razel ships its own `cc:defs.bzl`

Following Bazel's `cc:defs.bzl` lands on native code, not a declaration:
`defs.bzl → cc/private/rules_impl/cc_library.bzl → cc_common.* → native_cc_common = cc_common`
(the Java builtin), or one branch is literally `cc_library = native.cc_library`. The C++ build
*semantics* live in Bazel's Java, not in any `.bzl`. So "evaluate the real `.bzl`" means *supplying
`cc_common` + `native.cc_library`* — reimplementing the backend so the shell has something to call.

Bazel's public `cc_common` is also **~30 methods and leaky** — `get_memory_inefficient_command_line`
is a real public name; the linking surface is a zoo (`create_library_to_link`, `create_linker_input`,
`create_linking_context`, `…_from_compilation_outputs`, `merge_linking_contexts`). Mirroring it has
no payoff.

**Decision.** razel does *not* mirror `cc_common`. It ships its own `cc:defs.bzl` Starlark rules over
a **tight razel builtin API** (`razel_cc.*`), and holds only the real compat boundary.

## 2. The compat boundary (held exactly)

- **Rule API** — `cc_library`/`cc_binary`/`cc_test` attrs (`name, srcs, hdrs, deps, copts, defines,
  includes, …`). BUILD files are portable.
- **Providers** consumed by downstream rules — `CcInfo`, `DefaultInfo`, `CcToolchainInfo` (the actual
  surface: in rules_cc, `CcInfo` is referenced 99×, `DefaultInfo` 58×, `CcToolchainInfo` 40×).

Everything else (`cc_common.*`'s method surface) is razel's to design.

## 3. The layers (clean seam)

```
BUILD file            cc_library(name, srcs, hdrs, deps)         ← declaration (data)
  ↓ (BzlLoader serves razel's cc:defs.bzl for @rules_cc//cc:defs.bzl)
razel cc:defs.bzl     _cc_library_impl(ctx): … razel_cc.compile/archive … return [CcInfo, DefaultInfo]
  ↓ (Starlark builtins registered as globals)                   ← target-type logic, editable
razel_cc.* builtins   Rust, Starlark-facing — the tight API
  ↓
razel internals       Constrain · derive · producer/fold · path model   ← parity-proven (§5)
```

The rule layer runs through the **existing** `rule()` evaluator (`analyze_starlark` already runs
user rule `_impl`s with `ctx.actions`). No new evaluation machinery — just register `razel_cc` and
have `BzlLoader` serve razel's `cc:defs.bzl`.

## 4. The `razel_cc` builtin API (the tight surface)

Six methods. Each is the clean version of a `cc_common` cluster, backed by a primitive that already
exists and already reproduces the golden.

```python
razel_cc.toolchain(ctx) -> CcToolchainInfo
    # Resolve the cc toolchain: the FeatureConfig + tool paths + path-model params
    # (cfg, sdk, repo, target). Backed by the EVALUATED cc_toolchain_config_lib.bzl (§6).

razel_cc.compile(ctx, *, srcs, public_hdrs=[], private_hdrs=[], deps=[], copts=[], features=[])
        -> struct(objects=[File], compilation_context=struct(headers=depset))
    # One CppCompile per src. Feature set = Constrain.select(toolchain.features + features).
    # Per src: CompileInputs via the path model; quote_include_paths from deps' transitive headers;
    # argv via Constrain (cc_compile_argv). headers = public_hdrs ∪ fold(deps.headers).
    #   ≈ cc_common.compile + create_compilation_outputs + configure_features

razel_cc.archive(ctx, *, name, objects) -> struct(library=File)
    # One CppArchive over objects → lib<name>.a. argv via Constrain (cc_archive_argv) + path model.
    #   ≈ part of cc_common.link / create_library_to_link

razel_cc.link(ctx, *, name, objects, deps=[]) -> struct(executable=File)   # RESERVED (§7)
    # One CppLink over objects + deps' libraries (ordered). Needs a link golden first.
    #   ≈ cc_common.link / create_linking_context

razel_cc.cc_info(*, compilation_context, library=None, deps=[]) -> CcInfo
    # Assemble + merge: OWN headers/library merged with deps' CcInfo. Propagation is the DDS fold,
    # not a re-walk here.   ≈ CcInfo() + merge_cc_infos

razel_cc.command_line(toolchain, action, features, variables) -> [str]   # low-level escape hatch
    # Direct Constrain.command_line — the clean replacement for get_memory_inefficient_command_line.
    # For tests / advanced rules; the high-level builtins use it internally.
```

### Mapping to what's already built (§5-proven)

| Bazel `cc_common` (leaky)                                                            | razel internal                          |
|---|---|
| `configure_features` · `is_enabled` · `get_memory_inefficient_command_line` · `get_tool_for_action` | `Constrain` (`select`/`command_line`/`tool_path`) |
| `compile` · `create_compilation_outputs`                                             | `derive_cc_library_actions`             |
| `CcInfo` merge / propagate                                                           | producer + `DdsRead::fold_set`          |
| artifact naming                                                                      | path model (`bazel_compile_inputs`)     |

So the builtins are a thin Starlark-facing wrapper over primitives that already match the golden.

## 5. The provider contract

DDS-backed (typed facts; Set fields fold transitively):

- `CcInfo { headers: Set<File>, libraries: OrderedDepset<File> }` — `headers` folds (V1 Set);
  `libraries` is ordered (link order) → the reserved `OrderedDepset` monoid.
- `DefaultInfo { files: Set<File> }`.
- `CcToolchainInfo { feature_config, tool_paths, cfg, sdk, repo, target }` — carries the evaluated
  `FeatureConfig` + path-model params.

These are the §2 boundary; the rule returns `[CcInfo, DefaultInfo]`, the assembler asserts them.

## 6. The rule layer (`cc:defs.bzl`)

Target-type logic lives here, legible:

```python
def _cc_library_impl(ctx):
    tc   = razel_cc.toolchain(ctx)
    deps = [d[CcInfo] for d in ctx.attr.deps]
    comp = razel_cc.compile(ctx, srcs = ctx.files.srcs, public_hdrs = ctx.files.hdrs,
                            deps = deps, copts = ctx.attr.copts)
    arch = razel_cc.archive(ctx, name = ctx.label.name, objects = comp.objects)
    return [
        razel_cc.cc_info(compilation_context = comp.compilation_context,
                         library = arch.library, deps = deps),
        DefaultInfo(files = [arch.library]),
    ]

def _cc_binary_impl(ctx):   # link reserved (§7)
    tc   = razel_cc.toolchain(ctx)
    deps = [d[CcInfo] for d in ctx.attr.deps]
    comp = razel_cc.compile(ctx, srcs = ctx.files.srcs, deps = deps, copts = ctx.attr.copts)
    exe  = razel_cc.link(ctx, name = ctx.label.name, objects = comp.objects, deps = deps)
    return [DefaultInfo(files = [exe.executable])]
```

The config the builtins feed `Constrain` is the **evaluated** `cc_toolchain_config_lib.bzl` +
`local_config_cc` values (via `razel_cc.toolchain`) — not the hand-ported `cc_macos_core.bzl`. This
spec subsumes "kill the hand-port."

## 7. Scope / reserved

- **Declared vs executable.** The builtins emit the *declared* graph (parity argv: `cc_wrapper.sh`
  + bazel-out paths). What razel *runs* is the executor's concern (separate; razel may adopt the
  bazel toolchain). Parity is on the declared graph.
- **Linking depth.** `razel_cc.link` + `CcInfo.libraries` ordering land with a captured `CppLink`
  golden. The `cc_common` linker-input zoo is deliberately *not* reproduced — ordered-depset
  libraries cover the real need.
- **Out**: modules/`cppmap`, shared libraries, the `cc_common` create_* zoo, `objc_*`.

## 8. Parity anchor

Each builtin's declared output is golden-checked by the hermetic runner (`RazelParityHarness.md`).
`compile` + `archive` are already proven against `cc/transitive` (argv + outputs + source-inputs);
`link` needs its golden before `razel_cc.link` ships.

## 9. Build sequence (roll-builds)

1. **`razel_cc` builtins** — register the six as Starlark globals over Constrain/derive/producer/path
   model; unit-parity vs the golden (the primitives already pass, this is the wrapper + the seam).
2. **razel `cc:defs.bzl`** — `cc_library` `_impl` over the builtins; `BzlLoader` serves it for
   `@rules_cc//cc:defs.bzl`; the `analyze_bazel` characterization now flows through the `.bzl`,
   and the declared graph parity-checks against the golden.
3. **Config eval** — `razel_cc.toolchain` evaluates `cc_toolchain_config_lib.bzl` + `local_config_cc`
   into the `FeatureConfig` (retires `cc_macos_core.bzl`).
4. **`cc_binary` / link** — capture a `CppLink` golden; `razel_cc.link` + `CcInfo.libraries`.
5. **rust** — the same shape (`razel_rust.*` over `derive_rust_library_action`), once cc lands the
   pattern.
