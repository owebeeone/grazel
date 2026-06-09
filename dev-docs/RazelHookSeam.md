# RazelHookSeam — Phase C3 design (the per-language hook seam)

Companion to `RazelStarlarkBoundaryPlan.md` §10 Phase C. C0–C2 landed the substance: the loader is
decomposed, the four-move `razel_build` surface exists (`command_line` + `action`), and the provider
model converged on the `razel-dds` value algebra — OWN providers in a `FieldValue` map, transitive
closures recovered by one `DdsRead` fold, two instances (cc + java). **One C2 deliverable is NOT yet
built**: the plan's generic `razel_build.info(schema, …)` constructor — capture is still the two
hardcoded `CcInfo`/`JavaInfo` builtins (`dialect.rs`). So C3a must *build* `info`, not layer on it.

**C3 is the last subphase**: formalize the per-language extension points and enforce that *no language
name leaks into the engine core*, so adding a language is a **registration**, not an edit to shared
code.

This is a *design* subphase, not a mechanical one (see §4 — the toolchain hook hits a real abstraction
gap). **Revised per the design review in `scratch/RazelHookSeam-Review48.md`**, which corrected the §2
inventory (a missed native-path fold + a python entanglement) and the C3a scope. Read before opening C3.

---

## 1. Goal (from the plan)

> **C3 — hook seam.** Formalize the extension points the B5 java spike surfaced (toolchain resolver,
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
| `dialect.rs` | `CcInfo`/`JavaInfo` capture builtins; the **Starlark-path** dep-fold (`CcInfo.hdrs` / `JavaInfo.{compile_jars,runtime_jars,neverlink}`); the dep struct's `headers`/`compile_jars`/`runtime_jars`; `cc_provider_map` (the filegroup-ish rule) | the rule-API layer is cc/java-shaped |
| `deps.rs` | `resolve_dep` (`:63,70`) hardcodes `ProviderTypeId::new("CcInfo")` + folds `hdrs`/`cflags` | the **native-path** dep-fold — generic machinery shared by all 4 native rulesets (review-caught; the original inventory missed this) |
| `rules.rs` (loader) | `"cc"` ×5 — the `@rules_cc//` prefix + `CcToolchainMode` wiring | the **registration site** (`ruleset_modules`), not a core leak — see §6 |

Two consequences the review surfaced, both binding on C3a:
- **There are TWO transitive dep-folds** that hardcode the provider set — the Starlark path
  (`dialect.rs`) AND the native path (`deps.rs::resolve_dep`). C3a must de-leak **both**, or the gate
  is a fiction.
- **Python piggybacks `.py` sources on the `CcInfo.hdrs` channel** (`py_rules.rs:84` calls
  `cc_provider_map`, "carried through the `hdrs` channel by `resolve_dep`"). So `cc_provider_map`
  **cannot simply "move to the cc module"** (the §3 sketch) — py must first get its own provider/field
  (or the channel stay generic), else py breaks. The registry must own this, not a cc-private helper.

The bulk (state/dds/dialect/deps) is the **provider-schema registry** (§3). `engine.rs` is the
**toolchain hook** (§4). `rules.rs`'s `"cc"` is the per-language *registration*, which is allowed (§6).

## 3. C3a — the provider-schema registry (the bulk; mechanical)

A per-`Session` `ProviderRegistry`: provider type → its fields → each field's `FieldKind`
(`Set`/`OrderedDepset`/`Scalar`) **and** its fold flavour (plain vs `neverlink`-pruned). The
per-language ruleset modules register their providers (cc registers `CcInfo`, java `JavaInfo`); the
core reads the registry.

```
struct ProviderRegistry { schemas: BTreeMap<ProviderTypeId, ProviderSchema>,
                          // + per-field fold policy: ordered? pruned-by(field)? }
```

The registry must carry, per provider field: the `FieldKind` + fold policy (plain / `neverlink`-pruned)
AND a **`dep_struct_projection` name** — because the `.bzl` ABI does not match the field names. Example
the review caught: the provider field is `hdrs` (`state.rs`, `dds.rs`), but the dep struct projects it
as `headers` and `cc_defs.bzl:23` reads `d.headers`. So "names come from the registry" is not "drop the
literals" — it's a `(provider_field → dep_struct_name)` map the `.bzl` interface pins. Renaming `hdrs`
to match would break the `.bzl`; re-hardcoding the rename defeats the point. The registry owns the map.

What it must do:
- **`dds::to_dds`** — register schemas *from the registry*, not a hardcoded list; assert whatever
  providers a target carries (already map-driven post-C2d — drop the schema-name literals).
- **BOTH dep-folds** — `dialect.rs` (Starlark path) AND `deps.rs::resolve_dep` (native path) replace
  their hardcoded `CcInfo`/`JavaInfo` folds with a registry iteration: for each registered field, fold
  by kind/policy and project under its `dep_struct_projection` name. The review showed `deps.rs` is a
  second, equally-hardcoded fold — de-leaking only `dialect.rs` leaves the gate failing.
- **Build `razel_build.info(provider, fields)`** — the generic capture constructor *does not exist yet*
  (C2 left two hardcoded `CcInfo`/`JavaInfo` builtins). C3a builds it, driven by the registry schema;
  `set_provider` already writes the map, so the builtin becomes schema validation + the map write.
- **`state.rs` / `cc_provider_map`** — relocating it is **blocked** until python is untangled from the
  `CcInfo.hdrs` channel (§2): give py its own provider/field via the registry first, *then* the cc map
  is cc-private. Until then `cc_provider_map` is a shared (not cc-only) helper — don't move it naively.

AD2: the registry is built per-`Session` at analysis construction (populated by the active ruleset
modules), never a process global — same discipline as `Session::host_cc` (F13).

**Not "mechanical" — the storage is already the map, but C3a (a) builds the missing `info` constructor,
(b) adds a field→projection map the `.bzl` ABI pins, (c) de-leaks TWO folds, (d) untangles py from the
cc channel.** Still parity-guarded, but scope it as a real refactor, not a rename. (Original §3 called
this "mechanical, low-risk" — the review corrected that.)

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

- **Action-transform hook** (the spike's other extension point): cc's include-scan, java's `ijar`
  (header-jar) are per-language *post-processors* on an action's inputs/outputs. **The review flagged
  this as a premature-abstraction risk — it has zero current users** (the very trap §4 avoids for the
  toolchain). So **do NOT build the seam API in C3.** Move it to the Phase-D handoff (§8): design the
  registration shape *with* the first real transform (a real ijar/include-scan), not before.
- **The gate** (mirrors the AD2 / dds-boundary checks in `xtask`): scan the engine-core files
  (`state.rs`, `engine.rs`, `dds.rs`, the generic parts of `dialect.rs`, `deps.rs`, the loader) for
  banned tokens. **Ban distinctive identifiers, NOT bare `"cc"`/`"java"`** — the review noted the
  existing scanner only skips full-line `//` comments, so substrings like `cc`/`java` false-positive
  heavily (they appear in `macos`, prose, paths). Ban: `CcInfo`, `JavaInfo`, `macos_core_config`,
  `cc_provider_map`, and the bare provider-field literals. The per-language modules (`native_cc`,
  `rust_rules`, `py_rules`, `sh_rules`, `shims`, the `.bzl`) + the registry are the allowlist; the gate
  fails if a distinctive language token appears in the core.
  - The gate is landable **only after** C3a+C3b evict the current tokens; adding it earlier just
    fails. So gate **last**.
  - Honest cost note: at N=2 languages this gate mostly polices code that is *legitimately*
    language-specific. Its value is forward-looking — it bites when a 3rd, non-cc-shaped language lands.
    Worth it as a cheap forward guard, but it is not paying down a present-tense defect.

## 6. What is NOT a leak (the allowed registration surface)

`rules.rs`'s `"cc"` (the `@rules_cc//` prefix, `CcToolchainMode`, `rules_cc_module`) is the
**registration site** — `ruleset_modules` maps `@rules_*//` to per-language modules. A language name
*there* is the point (it's the table that wires a language in). The gate's allowlist must include the
`ruleset_modules` registration + the per-language modules. The invariant is "no language name in the
*generic* engine," not "nowhere."

## 7. Staged roll-build plan

| step | scope | risk | green |
|---|---|---|---|
| **C3a** | provider-schema registry (kind + fold policy + `dep_struct_projection`); **build `razel_build.info`**; both dep-folds (`dialect.rs` + `deps.rs`) read it; untangle py from the `CcInfo.hdrs` channel; then relocate `cc_provider_map` | **real refactor** (not a rename) — parity-guarded | cc + java parities + bins |
| **C3b** | `Session` toolchain-resolver registry; `engine.rs` reads it (interim cc-coupled resolver — §4 ii) | small; engine goes language-free | parities; engine has no `"cc"` |
| **C3c** | the `xtask` gate on **distinctive tokens** (`CcInfo`/`JavaInfo`/`macos_core_config`/`cc_provider_map`/field literals), not bare `cc`/`java` | gate authoring | gate green on the de-leaked core |

### Step detail — the roll-build sequence

Protocol (the project's roll-build discipline): each step below is **one green-gated commit + tag**
(`razelV2-RSB/C3…`). Gate every step on `cargo test --workspace` (cc + java + py parities + all bins)
**and** `cargo run -p xtask -- gates` (AD2 + dds-boundary); commit + tag only when green. The parity
tests are the safety net — a wrong fold / capture / projection fails loudly, it cannot land silently.
Order is fixed: **C3a → C3b → C3c**; the gate is last (it only passes once the tokens are evicted).

**C3a — registry + generic capture (the real refactor):**

1. **C3a.1 — `ProviderRegistry` (additive, no behavior change).** New type: provider → fields, each
   field `{FieldKind, fold policy (plain | pruned-by ⟨field⟩), dep_struct_projection}`. Built
   per-`Session` at construction; the active ruleset modules register their providers — cc →
   `CcInfo{hdrs→headers : Set, cflags : Set}`, java →
   `JavaInfo{compile_jars : OrderedDepset, runtime_jars : OrderedDepset pruned-by neverlink, neverlink : Scalar}`,
   py → its OWN `PyInfo{srcs}` (the untangle target). AD2: per-Session, not a global.
   *Touches:* a new `registry` module, `state.rs` (Session holds it), the ruleset modules.
   *Green:* registry populated + unit-tested (the schema + projection map); nothing else wired →
   parities unchanged.
2. **C3a.2 — build `razel_build.info`; capture + `to_dds` read the registry; untangle py.** Build the
   generic `info(provider, fields)` constructor (validate against the registry schema, write the map
   via `set_provider`); `CcInfo`/`JavaInfo` builtins become thin wrappers over it. `to_dds` registers
   schemas + asserts from the registry (drop the hardcoded `DefaultInfo`/`CcInfo`/`JavaInfo` list).
   **Move python OFF** `cc_provider_map`→`CcInfo.hdrs` onto its `PyInfo` channel (`py_rules.rs` +
   `resolve_dep`). *Touches:* `engine.rs`, `dialect.rs`, `dds.rs`, `py_rules.rs`, `deps.rs`.
   *Green:* cc + java + py parities; capture flows through one constructor.
3. **C3a.3 — both dep-folds via the registry; relocate `cc_provider_map`.** Route the **Starlark-path**
   fold (`dialect.rs` dep-resolution) AND the **native-path** fold (`deps.rs::resolve_dep`) through the
   registry: iterate registered fields, fold by kind/policy, project under `dep_struct_projection` (the
   `hdrs`→`d.headers` ABI rename now lives in the registry, not a literal). With py untangled, relocate
   `cc_provider_map` + the cc/java accessors out of `state.rs` into the language modules.
   *Touches:* `dialect.rs`, `deps.rs`, `state.rs`. *Green:* cc + java + py parities (dep-struct
   byte-identical via the projection). **Done ⇒ `state`/`dds`/the generic `dialect`/`deps` paths carry
   no `CcInfo`/`JavaInfo`/field literals.**

**C3b — toolchain-resolver hook:**

4. **C3b.1 — `Session` toolchain-resolver registry.** `Session` holds
   `toolchain_resolvers: BTreeMap<String, ResolverFn>`; the cc module registers `"cc" → macos_core_config`;
   `engine.rs::command_line` calls `session.resolve_toolchain(name)`. Interim: the resolver returns
   `FeatureConfig` (§4 ii — the generic `Toolchain` type is Phase D). AD2: per-Session, registered at
   construction. *Touches:* `state.rs`, `engine.rs`, the cc registration. *Green:* cc command line
   byte-identical; `engine.rs` free of `"cc"`/`macos_core_config`.

**C3c — the gate (last):**

5. **C3c.1 — the no-language-in-core `xtask` gate.** Mirror the AD2 / dds-boundary scanners
   (`xtask/src/main.rs`): scan the core files (`state`/`engine`/`dds`/generic `dialect`/`deps`/loader;
   allowlist the language modules + the registry + `ruleset_modules`) for **distinctive tokens**
   (`CcInfo`/`JavaInfo`/`PyInfo`/`macos_core_config`/`cc_provider_map`/the bare field literals) — NOT
   bare `cc`/`java`. *Touches:* `xtask/src/main.rs` + a gate unit test. *Green:* the gate passes on the
   now-de-leaked core (lands only after C3a + C3b).

*Exit (C3 / Phase C):* a generic engine with two instances behind a clean, **gate-enforced** hook
seam. (The action-transform hook is explicitly **out** — deferred to Phase D, §8.)

## 8b. C3 progress + the channel redesign (revised after C3a.4)

**Done + green** (`razelV2-RSB/C3a.1…C3a.4`): the `ProviderRegistry` is the live source of truth; it
drives `to_dds`'s schema registration, **both** dep-folds (the Starlark `dialect` path + the native
`deps::resolve_dep` path, via one shared `dds::fold_dep_fields`), and provider **capture** (the new
generic `razel_build.info(provider, fields)` — the `CcInfo`/`JavaInfo` builtins are deleted). The rule
API (`dialect`) and the fold paths (`dds`/`deps`) now carry **zero** language literals.

**The remaining state leak is two `CcInfo.hdrs`-channel abuses** (a real design smell C3a.4 surfaced,
not a rename):
- **py** (`py_rules.rs`) pushes `.py` sources through `cc_provider_map`→`CcInfo.hdrs`. py is *tested*
  (`razel-build/tests/py_rules.rs`) so it needs a real channel: register a **`PyInfo{srcs}`** provider
  (projection e.g. `py_srcs`); `py_library` captures into it; `gather()` reads `dep.field("py_srcs")`.
- **`filegroup`** (`dialect.rs`, a *generic* rule) pushes its files through `CcInfo.hdrs` so cc deps
  pick them up as headers. This hack is **untested** (no test reads a filegroup's headers), so drop it:
  `filegroup` provides **`DefaultInfo` only**. (If a real cc-on-filegroup case appears later, the right
  fix is cc reading dep `DefaultInfo.files` as inputs — not a generic rule faking `CcInfo`.)
- **The accessors** (`AnalyzedTarget::{hdrs,cflags,compile_jars,runtime_jars,neverlink}`) are now
  *test-only* (the live fold reads the map). They hardcode `"CcInfo"`/`"JavaInfo"` — so replace them
  with the generic `field_strs(provider, field)` + `scalar_bool(provider, field)` (caller names the
  provider; tests are allowlisted), and `cc_provider_map` relocates to the cc module once py+filegroup
  are off it.

**ALL DONE** (`razelV2-RSB/C3a.1`…`C3c`): C3a.5 untangled py (→`PyInfo`) + filegroup (→`DefaultInfo`)
and relocated `cc_provider_map` (→generic `set_set`) + the accessors (→generic `field_strs`/
`scalar_bool`); C3b moved the toolchain resolver into `toolchains.rs` (engine reads it); C3c added the
`xtask` no-language-in-core gate (distinctive tokens, skips comments + `#[cfg(test)]`, allowlists the
registries + per-language rules + the ruleset table), unit-tested + **green**. The engine names no
language; adding one is a registration. Phase-D items (the generic `Toolchain` type, the
action-transform hook) remain in §8.

## 8. Phase-D handoff (explicitly out of C3)

- The **generic `Toolchain` type** (§4 option i) — needs the real-upstream-toolchain second instance.
- The **action-transform hook** itself (the registration shape) AND its real transforms (ijar,
  include-scan) — design the seam *with* the first real transform, not speculatively (review: zero
  users today). C3 does not build this seam.
- The `FeatureConfig`-in-`Session` cc-coupling (§4 ii) retires when (i) lands.

These are tracked in `RazelGaps.md`; C3 builds the registry + the engine resolver seam, Phase D adds
the real toolchains + the action-transform seam.
