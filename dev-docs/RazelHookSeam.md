# RazelHookSeam — Phase C3 design (the per-language hook seam)

Companion to `RazelStarlarkBoundaryPlan.md` §10 Phase C. C0–C2 are done: the loader is decomposed,
the four-move `razel_build` surface exists, and the provider model converged on the `razel-dds`
value algebra (one fold, two instances — cc + java). **C3 is the last subphase**: formalize the
per-language extension points and enforce that *no language name leaks into the engine core*, so
adding a language is a **registration**, not an edit to shared code.

This is a *design* subphase, not a mechanical one — see §4 (the toolchain-resolver hook hits a real
abstraction gap). Read this before opening C3.

---

## 1. Goal (from the plan)

> **C3 — hook seam.** Formalize the extension points the B5 ledger surfaced (toolchain resolver,
> action-transform for ijar/include-scan). *Green:* cc + java's bespoke bits sit behind hooks; an
> `xtask gates`-style check that no language name leaks into the engine core.

The invariant C3 establishes: **the engine core is language-agnostic.** `cc`, `java`, `CcInfo`,
`JavaInfo`, `macos_core_config`, and the field names (`hdrs`/`compile_jars`/…) appear ONLY in the
per-language modules + their registrations — never in `state` / `engine` / `dds` / the generic
`dialect`/loader paths.

## 2. The leak inventory (what C3 must remove from the core)

Surveyed at C2d (`grep` of the core modules). These are the language/provider literals to evict:

| module | leaks | nature |
|---|---|---|
| `state.rs` | `cc_provider_map`; `field_strs`/accessors hardcode `"CcInfo"`/`"JavaInfo"` + the 5 field names | a cc helper + cc/java accessors living in the foundation |
| `engine.rs` | `razel_build.command_line` matches `"cc"` → `macos_core_config` | the toolchain resolver, hardcoded (§4 — the hard one) |
| `dds.rs` (`to_dds`) | registers `DefaultInfo`/`CcInfo`/`JavaInfo` schemas + field names | provider schemas hardcoded in the bridge |
| `dialect.rs` | `CcInfo`/`JavaInfo` capture builtins; the dep-resolution folds `CcInfo.hdrs` / `JavaInfo.{compile_jars,runtime_jars,neverlink}`; the dep struct's `headers`/`compile_jars`/`runtime_jars`; `cc_provider_map` (the filegroup-ish rule) | the rule-API layer is cc/java-shaped |
| `rules.rs` (loader) | `"cc"` ×5 — the `@rules_cc//` prefix + `CcToolchainMode` wiring | these are the **registration site** (`ruleset_modules`), not a core leak — see §6 |

The bulk (state/dds/dialect) is the **provider-schema registry** (§3). `engine.rs` is the
**toolchain hook** (§4). `rules.rs`'s `"cc"` is the per-language *registration*, which is allowed.

## 3. C3a — the provider-schema registry (the bulk; mechanical)

A per-`Session` `ProviderRegistry`: provider type → its fields → each field's `FieldKind`
(`Set`/`OrderedDepset`/`Scalar`) **and** its fold flavour (plain vs `neverlink`-pruned). The
per-language ruleset modules register their providers (cc registers `CcInfo`, java `JavaInfo`); the
core reads the registry.

```
struct ProviderRegistry { schemas: BTreeMap<ProviderTypeId, ProviderSchema>,
                          // + per-field fold policy: ordered? pruned-by(field)? }
```

What it removes:
- **`dds::to_dds`** — registers schemas *from the registry*, not a hardcoded list; asserts whatever
  providers a target carries (already map-driven post-C2d — just drop the schema-name literals).
- **`dialect` dep-resolution** — instead of three hardcoded folds (`CcInfo.hdrs`,
  `JavaInfo.compile_jars`, …), iterate the registry: for each registered provider field, fold by its
  kind/policy and project it onto the dep struct generically (the struct's field names come from the
  registry, not literals).
- **`dialect` capture** — `CcInfo`/`JavaInfo` builtins become thin registrations (or one
  `razel_build.info(provider, fields)` constructor driven by the registry); `set_provider` already
  writes the map generically.
- **`state.rs`** — `cc_provider_map` moves to the cc module; the `hdrs()`/`cflags()`/… accessors
  move to per-language helpers (or callers read the map by registered `(ty, field)`).

AD2: the registry is built per-`Session` at analysis construction (populated by the active ruleset
modules), never a process global — same discipline as `Session::host_cc` (F13).

**Mechanical, low-risk** (the parity tests guard it): the storage is *already* the map; C3a moves the
*names* from literals into data.

## 4. C3b — the toolchain-resolver hook (the hard part: a real abstraction gap)

`engine.rs`'s `command_line` hardcodes `"cc" => macos_core_config()`. Evicting `"cc"` needs a
*registered* resolver. **The blocker:** there is no unified toolchain *type*.
- cc's toolchain is a `FeatureConfig` (the `Constrain` feature/flag model).
- java's "toolchain" is a Starlark **template** (`java -jar JavaBuilder @args`) — it does NOT use
  `command_line` at all (it rides `razel_build.action` directly). There is no java toolchain object.

So a generic `Toolchain` returning a command line is cc-shaped today. Two options:

- **(i) Generic `Toolchain` trait/enum** — `fn command_line(action, vars) -> Vec<String>`, cc impl
  wrapping `FeatureConfig`+`Constrain`, others wrapping their own producer. This is the *right* end
  state but it's a real abstraction — and it really wants the *second* command-line-shaped toolchain
  (not java, which is template-shaped) to avoid a one-member abstraction. That second instance
  arrives with **Phase D** (real upstream toolchains). Designing it now risks a premature abstraction.
- **(ii) Interim: a registered resolver fn (recommended for C3b).** `Session` holds
  `toolchain_resolvers: BTreeMap<String, ResolverFn>` where `ResolverFn` is registered by the cc
  module (`"cc"` → `macos_core_config`). `engine.rs` calls `session.resolve_toolchain(name)` — **no
  `"cc"` literal in the engine**. The resolver still returns `FeatureConfig` (cc-coupled type), so
  this de-leaks the *engine* without inventing the generic toolchain type. Note the residual
  honestly: `state`/`Session` referencing `FeatureConfig` is a known cc-coupling, retired when (i)
  lands in Phase D.

**Recommendation:** do (ii) for C3b (engine surface goes language-free, the named hook exists), and
record (i) as the Phase-D generalization (it needs the real-toolchain second instance to be honest).

## 5. C3c — the action-transform hook + the gate

- **Action-transform hook** (the ledger's other extension point): cc's include-scan, java's `ijar`
  (header-jar) are per-language *post-processors* on an action's inputs/outputs. For C3 this is a
  **seam** (a registration point + where it'd be called), not the real transforms (those are
  Phase-D, with the real toolchains). Define the registration shape; leave the impls stubbed +
  documented. Don't fake the transforms.
- **The gate** (mirrors the AD2 / dds-boundary checks in `xtask`): scan the engine-core files
  (`state.rs`, `engine.rs`, `dds.rs`, the generic parts of `dialect.rs`, the loader) for banned
  language/provider literals (`"CcInfo"`, `"JavaInfo"`, `"cc"`, `"java"`, `macos_core_config`, the
  field names). The per-language modules (`native_cc`, `rust_rules`, `py_rules`, `sh_rules`,
  `shims`, the `.bzl`) + the registry are the allowlist. The gate fails if a new language name
  appears in the core — enforcing the seam going forward.
  - The gate is landable **only after** C3a+C3b evict the current leaks; adding it earlier just
    fails. So gate **last**.

## 6. What is NOT a leak (the allowed registration surface)

`rules.rs`'s `"cc"` (the `@rules_cc//` prefix, `CcToolchainMode`, `rules_cc_module`) is the
**registration site** — `ruleset_modules` maps `@rules_*//` to per-language modules. A language name
*there* is the point (it's the table that wires a language in). The gate's allowlist must include the
`ruleset_modules` registration + the per-language modules. The invariant is "no language name in the
*generic* engine," not "nowhere."

## 7. Staged roll-build plan

| step | scope | risk | green |
|---|---|---|---|
| **C3a** | provider-schema registry; `to_dds`/dep-resolution/capture read it; move `cc_provider_map` + accessors to the cc module | mechanical (storage already map) — parity-guarded | cc + java parities + bins |
| **C3b** | `Session` toolchain-resolver registry; `engine.rs` reads it (interim cc-coupled resolver) | small; engine goes language-free | parities; engine has no `"cc"` |
| **C3c** | the `xtask` no-language-in-core gate + the action-transform seam (stubbed, documented) | gate authoring | gate green on the de-leaked core |

*Exit (C3 / Phase C):* a generic engine with two instances behind a clean, **gate-enforced** hook
seam.

## 8. Phase-D handoff (explicitly out of C3)

- The **generic `Toolchain` type** (§4 option i) — needs the real-upstream-toolchain second instance.
- Real **action-transforms** (ijar, include-scan) — need the real toolchains.
- The `FeatureConfig`-in-`Session` cc-coupling (§4 ii) retires when (i) lands.

These are tracked in `RazelGaps.md`; C3 builds the seam, Phase D fills it with real toolchains.
