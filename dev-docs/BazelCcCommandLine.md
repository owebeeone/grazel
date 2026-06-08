# How Bazel assembles the cc command line — the §8c / `Constrain` spec

Verified against the Bazel source (`rules/cpp/CcToolchainFeatures.java`, `FeatureSelection.java`,
`CompileCommandLine.java`, `CompileBuildVariables.java`) and the captured macOS toolchain config
(`local_config_cc` + `rules_cc//cc/private/toolchain/unix_cc_toolchain_config.bzl`).

## Thesis
**The cc argv is not hardcoded — it is a declarative feature-config interpreter.** This is exactly
the `Constrain` construct (RazelV2Contracts/DdsCoveringSet): razel's §8c cc actions builder is *not*
"assemble an argv to match the golden string" — it is **reimplement Bazel's feature selection +
flag expansion over the toolchain's feature definitions + the action's variables.** That is the
only path to a `aquery`-faithful argv, and it generalizes to any toolchain (not just the local one).

## The three phases
1. **Config** — parse the toolchain config into a graph of `Feature` / `ActionConfig` / `FlagSet`
   / `FlagGroup` / `WithFeatureSet`.
2. **Selection** (`FeatureSelection.java`) — from the requested feature set: transitive `implies`
   closure, then a fix-point that disables features whose `requires` aren't met; detect `provides`
   collisions. Result: the enabled features **in config-declaration order** + enabled action_configs.
3. **Expansion** (`FeatureConfiguration.getCommandLine` → `CompileCommandLine.getCompilerOptions`)
   — tool path first (the action_config's tool), then for each enabled feature in order, each
   `FlagSet` whose `actions` include this action and whose `with_features` gate is satisfied, each
   `FlagGroup` (after `expand_if_*` gates), expanding `%{var}` / `iterate_over` against the action's
   variables. Finally `coptsFilter` drops filtered flags — **except** the `unfiltered_compile_flags`
   feature, which bypasses the filter.

## Data model (minimal-faithful)
- **Feature**: `name`, `enabled`, `flag_sets[]`, `implies[]`, `requires[](Set<Set<String>>)`, `provides[]`.
- **ActionConfig**: `action_name`, `tools[]` (with optional `with_features`), `flag_sets[]`, `enabled`, `implies[]`.
- **FlagSet**: `actions[]`, `with_features[]`, `expand_if_all_available[]`, `flag_groups[]`.
- **FlagGroup**: `flags[]`/nested `flag_groups[]`, `iterate_over`, `expand_if_available`/`_not_available`/`_true`/`_false`/`_equal`.
- **WithFeatureSet**: `features[]` (all enabled) + `not_features[]` (all disabled); a FlagSet's
  `with_features` is satisfied if **any** member set matches (disjunction-of-conjunction).

## Variables a `CppCompile` provides (`CompileBuildVariables`)
`source_file`, `output_file`, `dependency_file`, `user_compile_flags` (= `copts`/`cxxopts`),
`include_paths`, `quote_include_paths`, `system_include_paths`, `external_include_paths`,
`preprocessor_defines`, `includes`, `pic`, `module_*` (C++20/modules), plus FDO/LTO/fission extras.
Expansion: `%{name}` string interpolation; `iterate_over` unrolls a sequence variable; `expand_if_*`
gate a flag_group on a variable's presence/value.

## Where the golden's flags come from (macOS, default `CppCompile`)
Structural (hardcoded in the feature defs) vs **toolchain-configured** (from `cc_configure`, in the
`local_config_cc` BUILD's `cc_toolchain_config(...)` attrs):

| token(s) | feature / source |
|---|---|
| `cc_wrapper.sh` | `cpp_compile` action_config tool path |
| `-U_FORTIFY_SOURCE` | `default_compile_flags` (hardcoded flag_group) |
| `-fstack-protector -Wall -Wthread-safety -Wself-assign -Wunused-but-set-parameter -Wno-free-nonheap-object -fcolor-diagnostics -fno-omit-frame-pointer` | `default_compile_flags` ← **`ctx.attr.compile_flags`** (cc_configure-detected) |
| `-std=c++17` | `default_compile_flags` ← **`ctx.attr.cxx_flags`** (cxx actions only) |
| `-frandom-seed=<o>` | `random_seed` feature (`%{output_file}`) |
| `-mmacosx-version-min=<sdk>` | `macos_minimum_os` feature (**host SDK — externalize**) |
| `-MD -MF <d>` | `dependency_file` feature (`%{dependency_file}`) |
| `-iquote . -iquote <bin>` | `include_paths` feature (`iterate_over quote_include_paths`) |
| `-c <src>` | `compiler_input_flags` feature (`%{source_file}`) |
| `-o <o>` | `compiler_output_flags` feature (`%{output_file}`) |
| `-no-canonical-prefixes -Wno-builtin-macro-redefined -D__DATE__/__TIMESTAMP__/__TIME__="redacted"` | `unfiltered_compile_flags` ← **`ctx.attr.unfiltered_compile_flags`** (bypasses coptsFilter, appended last) |

`CppArchive` argv (`/usr/bin/libtool -static -no_warning_for_no_symbols …`) comes from the
`cpp_link_static_library` action_config + `archiver_flags`/`libtool` features (macOS).

## What §8c must implement (V2 minimal-faithful)
1. The data model above (Feature/ActionConfig/FlagSet/FlagGroup/WithFeatureSet).
2. Selection: `implies` closure + `requires` fix-point + `provides` collision (config order preserved).
3. Expansion: action filter + `with_features` gate + `expand_if_*` + `iterate_over` + `%{var}` + coptsFilter (with the `unfiltered_compile_flags` exemption).
4. The compile/link variable set (above), populated from the target's attrs + the propagation queries.
5. **Ingest** the toolchain config — the feature defs (`unix_cc_toolchain_config.bzl`) + the
   configured flag lists from the `cc_toolchain_config(...)` target. Razel must use the **same**
   toolchain config the golden was captured under (pin it in the corpus `meta.toml`).

**Externalize (host-specific, parameterize for cross-machine goldens):** the SDK / `-mmacosx-version-min`
value, tool paths (`cc_wrapper.sh`, `/usr/bin/libtool`), and the cc_configure-detected flag lists.

## Build-vs-buy
- **Implement the interpreter** (recommended): faithful, generalizes across toolchains, and *is*
  the `Constrain` construct V2 already committed to. Cost: the selection + expansion engine (~the
  spec above) — bounded and well-specified.
- **Capture-and-replay** the resolved per-action flag expansion: cheaper short-term, but brittle and
  doesn't generalize (every config/feature combo needs re-capture). Reject except as a test oracle.

So §8c = the cc actions UDF builds an `Args` by running this `Constrain` interpreter over the
ingested toolchain config + the action's variables. The propagation queries (§8b) feed the include
paths / defines / inputs; the interpreter produces the exact argv the parity runner diffs.
