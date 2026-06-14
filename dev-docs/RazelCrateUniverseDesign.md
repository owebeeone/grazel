# RazelCrateUniverseDesign - `@crates` build + `razel query`, over a shared target graph

*2026-06-14, rev 9 (after reviews `Review55`, `-Review55-2`, `-ReviewA`, `-Review55-3`,
`-Review55-4`, `-ReviewA-1`, + a **self-review grounded in the Bazel source** at
`/Users/owebeeone/limbo/bazel-dev/bazel`). Design. Owner: RR.
Scope: TWO razel features that consume Bazel's world and **share the same loader, target graph,
`DepInfo`/edges, `select()`, materialized `@crates`, and pattern expansion** — so they are
designed together, once, not against each other:*

- ***Part A (§1–§10): the `@crates` build*** — how razel LOADS and BUILDS the `@crates` that
  rules_rust's crate_universe generates, so razel and Bazel build the **identical** external
  graph (NOT the resolver — cargo-bazel keeps that). Milestones §9; open engineering §10.
- ***Part B (§11–§14): `razel query`*** — the query syntax and execution system, reading the
  **loading-phase** target graph that §11 makes the shared foundation. §14 is the non-conflict
  contract between A and B.

*Rev history: rev 1–4 hardened Part A across three reviews; rev 5 added Part B + the shared-graph
foundation (§11); rev 6 hardened Part B per review 55-3 (`LoadedTarget`/`RawAttr` schema, typed
edges, load-only adjacency, `--implicit_deps` default, re-tiered ladder). **Rev 7 (review 55-4)
pins the last query specifics: the `QueryNode` model incl. source/generated-file nodes (§11.1), a
query-facing `rule_class` + the verified `"<class> rule"` kind strings (§11.1/§12), the verified
canonical `@@rules_rust++crate+…` repo identity + accept-both-forms rule (§11.3), query
materializes-but-never-analyzes (§13), `labels()` vs `deps()` semantics (§12), `somepath` traversal
order (§13), implicit-label reality → core goldens use `--noimplicit_deps` (§13), `--cbor` deferred,
and the rev-marker fix. **Rev 8 (review A-1) corrects query semantics against live `bazel query`:
`deps()` DOES traverse `select()` condition labels + `compile_data` source files + `cargo_build_script`
macro children (§11.2); `labels()` is attr-specific and narrower, per-attr goldens (§12); pattern
entry stays `@crates`-only but traversal reaches `@rules_rust`/`@@platforms` (§13); `kind()` is an
open `"{rule_class} rule"`; plus build-order sequencing + the `expand_pattern` fork location (§10).
**Rev 9 — source-grounded self-review.** Confirmed against the Bazel implementation: the
deps/labels asymmetry is exactly `AggregatingAttributeMapper.includeSelectKeys` `true`/`false`,
the `kind` strings, `--implicit_deps` default true, and `target_compatible_with` being analysis-only.
Corrected: **`somepath` cannot match Bazel's exact path** (BFS over `getSuccessors` + arbitrary
intersection node → accept any shortest path, §13); the **`recordedInputs` wire format** needs the
`\s`/`\n`/`\0` escaping + `DIRENTS`/`DIRTREE` prefixes (§2.3); the lexicographic sort comes from
`--order_output=auto`+`StreamedFormatter`, not `--incompatible_lexicographical_output` (§13);
`lockFileVersion` is `26` here but Bazel HEAD is `28` so the reader must be version-aware (§2.1);
and a rev-8 leftover in §13's `select` bullet (still said "condition labels are not deps") is fixed.*

## 1. Decision and why

razel already reads Bazel's own `BUILD.bazel` natively. The bazel way extends that one rung:
razel **consumes crate_universe's generated `@crates`** rather than minting a razel-specific
dep graph. cargo-bazel keeps resolution, feature unification, and generation; razel learns to
*load and build its output*. Payoff: `@crates//:serde` is one set of generated targets, and
razel's action graph for it is diffable against `bazel aquery` (§8). razel does **not** build a
Cargo resolver — that is commodity and cargo-bazel owns it.

## 2. Source of truth: `MODULE.bazel.lock` (review P0; rev-3 P1-root/stale/schema, P2-fallback)

**Decision: razel reads the committed `MODULE.bazel.lock` as the source of truth for `@crates`,
and never generates or runs cargo-bazel during `razel build`.** Regeneration (Cargo.toml/Cargo.lock
changed) is the **offline** `bazel mod` / crate_universe step that rewrites the lock, committed
like `Cargo.lock`. Verified present in this workspace's lock: 242 `build_file_content` + per-crate
`urls`/`sha256`/`strip_prefix`, 159 `build_script_build` — everything razel needs, standalone.

### 2.1 The reader schema (P1-schema)

razel consumes exactly these paths; anything off-schema is a **loud error**, never a silent skip.
Pinned against this workspace's lock (`lockFileVersion: 26`, 228 `generatedRepoSpecs`):
- `lockFileVersion` — accepted set is pinned (this workspace's lock is `26`). **Source check:
  Bazel HEAD's `BazelLockFileValue.LOCK_FILE_VERSION` is already `28`** — so the format moves and
  the reader MUST be version-aware (accept the version the workspace's Bazel actually wrote; error
  loudly on an unknown one). This makes the §10 lock-coupling risk *active*, not hypothetical.
- the crate extension key is `ModuleExtensionId.toString()` =
  `<bzlFileLabel.unambiguousCanonicalForm>%<extensionName>[%<isolationKey>]`
  (`GsonTypeAdapterUtil.MODULE_EXTENSION_ID_TYPE_ADAPTER`). So the reader MUST accept the exact
  canonical-`@@` string `"@@rules_rust+//crate_universe:extensions.bzl%crate"` **and tolerate an
  optional trailing `%<module>+<name>` isolation suffix** (absent for crate_universe's non-isolated
  `crate` extension, but present if a usage is `isolate = True`). Getting this string wrong is the
  first integration-test failure, so it is named normatively.
  - `.general.generatedRepoSpecs` — repo name → spec, of two kinds (§2.2). (The sibling
    `…%_generate_repo` extension is **ignored**.)
  - `.recordedInputs` — the staleness inputs (§2.3).
- **Loud-error cases:** missing crate extension; missing root `crates` repo; a per-crate repo
  lacking `sha256`/`urls`/`build_file_content`; an unknown repo-rule id; an unresolved
  apparent→canonical repo-name mapping.

### 2.2 Materialize BOTH repo kinds — the root is first-class (P1-root)

`generatedRepoSpecs` has a **root** repo and **per-crate** repos; razel materializes both. (Repo
naming: the lock keys each per-crate spec by its extension-local name `crates__<name>-<ver>`; the
**canonical** label — what `bazel query` prints and the external dir uses — is
`@@rules_rust++crate+crates__<name>-<ver>`, per §11.3. The doc uses the canonical form.)
- **Root `@crates`** — `attributes.contents` is inline generated text `{BUILD.bazel, defs.bzl,
  alias_rules.bzl}` (no fetch). razel writes it to the repo tree, **loads `@crates//:defs.bzl`**
  (plain Starlark razel already executes — `all_crate_deps`/`aliases`), and the root
  `BUILD.bazel`'s `alias()` targets resolve `@crates//:blake3` →
  `@@rules_rust++crate+crates__blake3-1.8.2//:blake3`. Both the workspace's
  `load("@crates//:defs.bzl", …)` and every `@crates//:X` dep depend on this repo existing — so it
  is a named materialization output, not an afterthought.
- **Per-crate repos** (`crates__<name>-<ver>` in the lock) — fetch+extract+patch (§5.1) + drop in
  `build_file_content` as the package `BUILD`.

Alias resolution (`@crates//:X` → the per-crate label) runs in the loader via the root `BUILD`.

### 2.3 Stale-lock detection — the `recordedInputs` grammar (P1-stale)

`recordedInputs` is a **flat list of escaped, tagged strings** (`RepoRecordedInput`,
serialized `<PREFIX>:<id> <value>`). **Source-pinned wire format
(`RepoRecordedInput.java:238-279`):** each record is `prefix + ":" + escape(id) + " " + escape(value)`,
where `escape()` maps space→`\s`, newline→`\n`, and a **null value→`\0`**. The reader MUST unescape
these (a naïve split on `:`/space misparses paths and null values). The five prefixes:
- **`FILE`** — `FILE:<label> <value>`, value ∈ {`DIR`, `ENOENT`, lowercase-hex sha256}. e.g.
  `FILE:@@//Cargo.lock <sha>`, `FILE:@@//Cargo.toml <sha>`, one per member
  `FILE:@@//crates/<member>/Cargo.toml <sha>`. `@@//` = the workspace root. razel rehashes and
  compares each. **These gate staleness.**
- **`DIRENTS` / `DIRTREE`** — directory listing / subtree fingerprints (crate_universe can emit
  these). The reader must recognize and (slice 1) treat them like `FILE` for staleness where the
  path is a workspace path, else pass-through.
- **`ENV:<NAME> <value>`** — generator env (`CARGO_BAZEL_*`, `REPIN`); value may be `\0` (unset).
- **`REPO_MAPPING:<source_repo>,<apparent> <canonical|\0>`** — a repo-mapping entry.

**Staleness divergence, stated.** Bazel re-evaluates the extension if **any** recorded input
changed (file, env, *and* repo-mapping — `RepoRecordedInput`). razel cannot regenerate offline, so
slice 1 gates staleness on the **`FILE`/`DIR*` workspace inputs only** and treats `ENV`/`REPO_MAPPING`
as informational (they describe the generation environment, which the committed lock already
froze). This is a **deliberate, documented divergence**, not an oversight; a `FILE` mismatch →
**loud error** ("`MODULE.bazel.lock` is stale — regenerate offline"). The parser has golden tests
over a captured `recordedInputs` block including escaped values.

### 2.4 The cargo-bazel fallback is offline-only (P2-fallback)

If the lock format proves unstable (it is versioned, Bazel-internal JSON), the fallback — shelling
out to `cargo-bazel splice+generate` — is an **offline** tool that refreshes the lock-derived
snapshot. It is **never** invoked during `razel build`; build time is always lock-only and
standalone. Rejected build-time alternatives: `bazel fetch`/cargo-bazel at build time (couples to
those tools); reimplementing splice+generate (duplicates the resolver we reuse).

## 3. The flow

```
//crates/razel-cli:razel  →  dep @crates//:starlark
  1. LOCK READ    MODULE.bazel.lock → starlark's (fetch spec, build_file_content)        §5.1
  2. MATERIALIZE  fetch+extract+patch the .crate → @crates__starlark-0.14.2 source tree  §5.1
  3. LOAD         parse the build_file_content as the package's BUILD                     §5.5
  4. ANALYZE      resolve select()/target_compatible_with (§5.4); apply crate_features    §5.4-5.5
  5. ACTION GRAPH cargo_build_script run → flags file; rustc(lib) reads it via a wrapper   §5.2-5.3
  6. EXECUTE      razel-exec, topo order, content-addressed (tree outputs for OUT_DIR)     §5.2
  7. PROVIDE      the rlib flows to dependents as DepInfo; build-script edge stays intra   §4
```

Step 3 (LOAD) populates the **loading-phase target graph** that §11 makes the shared foundation;
steps 4–7 (analysis + execution) are a transform on top of it. `razel query` (Part B) reads that
loading-phase graph directly and stops before step 4 — so the build and query share the loader
and the materialized `@crates` without colliding.

## 4. Contracts (the data model)

### 4.1 `BuildScriptInfo` — a build script's effect, as a runtime flags file

NOT a cross-dependency provider fold, and NOT resolved at analysis. A build script's directives
are known only after it *runs*, so its effect is a **flags file** (schema in §6) that the
crate's own rustc reads at **exec time** through a wrapper (bazel's `process_wrapper` pattern).
It is an **intra-target action edge**: the `cargo_build_script` target's run action → the
crate's rustc action. Nothing about it lives in `DepInfo` or the DDS fold.

The crate's rustc `AnalyzedAction` declares the flags file and the `OUT_DIR` tree as inputs **by
path** — those paths are known at analysis (they are the `cargo_build_script` target's declared
outputs); only the *contents* are runtime. The wrapper reads the file and appends the flags.

### 4.2 The crate contract — `DepInfo`, with aliasing per-edge (resolves review P2-alias)

`DepInfo { libs, canon, field(projection) }` gains:
- `crate_name`: the **producer's canonical** crate name only (`--crate-name`).
- a `proc_macro` projection: the dep is a host dylib, `--extern`'d, not a target rlib (§5.3).

**Aliasing is a consumer-edge property, not a producer field.** The consuming `rust_library`
carries `aliases = aliases()`; razel resolves each `--extern` name at the **consumer's**
analysis: `extern_name = consumer_alias[dep] if present else dep.crate_name`. The same produced
crate can be imported under different names by different consumers; the producer never knows.

### 4.3 The build-script edge — `cargo_build_script` is a *target*, not a lib dep (resolves review P1-bs-shape)

crate_universe emits the build script as a distinct `cargo_build_script` target, aliases it
`:build_script_build`, and lists that alias in the crate's `deps`. So `rust_library` analysis
**must distinguish it from a normal lib dep** — passing `:build_script_build` as `--extern`
would be wrong. Contract: a `cargo_build_script` target provides a `BuildScriptRun` projection
on its `DepInfo` carrying `{flags_file path, out_dir path, build-dep rlib closure}` and **no
`libs`**. During analysis razel inspects each dep: a dep carrying `BuildScriptRun` is wired as
the intra-target build-script edge (its flags file + `OUT_DIR` become the rustc wrapper's
inputs, §4.1); every other dep is a normal `--extern` (rlib or proc-macro dylib).

### 4.4 Materialized-repo — the complete key (resolves review P2-key)

A repo spec → a source tree at `@crates__<name>-<ver>//…`. The cache key is the hash of the
**full tree-shaping input**, not just `(url, sha256, patches)`: `urls`, `sha256`,
`strip_prefix`, the **ordered byte content** of each patch (not its label), the patch strip
level/args including the per-crate **`remote_patch_strip`** field (observed on
`crates__blake3-1.8.2`), any added/linked files, the **generated `build_file_content`**, the
repo-mapping that resolves the labels inside it, and the cargo-bazel/tool version recorded in the
lock. A change in generated BUILD content with an unchanged URL/sha MUST invalidate.

This key and §2.3's staleness inputs are the same *kind* of fingerprint as
`RazelDepsEngineV2.md` §4.3 `InputVersion` (REQ-DEPSV2-013); when V2 lands, the lock reader and
`RepoFetch` should reuse that fingerprint, not invent a second one.

## 5. The pieces, designed

### 5.1 Repo materialization (the fetch seam)

Two kinds, both from the lock (§2.2):
- **Root `@crates`** — contents-only: write `attributes.contents` (`BUILD.bazel`/`defs.bzl`/
  `alias_rules.bzl`) to the repo tree. **No fetch, no network** — it is generated text. Keyed by
  the content hash.
- **Per-crate `@crates__<name>-<ver>`** — `fetch.rs` today extracts repo-rule specs ("no
  network"); the design adds a **realize** step behind the same spec type. A `RepoFetch` `Action`
  downloads the `.crate` (from `urls`, sha verified), extracts, applies `strip_prefix` + patches,
  drops in `build_file_content` as the package `BUILD`, content-addressed by §4.4's key.
- **Interim:** a `--crate-repo-cache=<bazel external>` mode that reads Bazel's already-fetched
  repos, deferring download/extract while §5.2–5.5 land. It is **dev-only and NOT parity-gating** —
  reading Bazel's tree masks bugs in `RepoFetch`, patch application, and the §4.4 key. The pure
  `RepoFetch` path MUST be the one under the parity goldens **by rung 4** (the full `@crates`).

### 5.2 `cargo_build_script` (native)

Three actions, a flags file, and a rustc wrapper.

1. **Compile** `build.rs` → a `rust_binary` (`<crate>_bs`) with the host toolchain (§5.3 — for
   host==target this is just the single toolchain).
2. **Run** — a `CargoBuildScriptRun` `AnalyzedAction`: `argv = [bs bin]`, `inputs = [bin, crate
   srcs/build-dep rlibs, declared data]`, `outputs = [flags-file (§6), OUT_DIR tree]`, `env =`
   the allowlisted Cargo contract: `CARGO_PKG_*`, `CARGO_FEATURE_<F>` per feature, `OUT_DIR`,
   `TARGET`, `HOST`, `OPT_LEVEL`, `CARGO_CFG_*`, **the cc toolchain `CC`/`AR`/`CFLAGS`**, and the
   `DEP_<LINKS>_*` published by `links`-crate build-dep deps (§6 `metadata`). default-deny env, so
   this allowlist *is* the script's world (scripts that read outside it or subprocess freely are
   the §10 long tail). **`OUT_DIR` is a directory of unknown files** → needs tree-output capture
   the executor lacks today (walk + content-address; §10).
3. **Consume at exec time** — the crate's rustc runs through a wrapper that reads the flags file
   and appends `--cfg`/`-l`/`-L`/`-C link-arg`/env + points `OUT_DIR` at the staged tree. Edge
   wiring is §4.3.

**`cargo_build_script` has its OWN attr contract (review P1-bs-attrs)** — distinct from
`rust_library` (§5.5), and split by phase:

| attr | phase | verdict |
|------|-------|---------|
| `srcs`, `crate_root` | compile (action 1) | the `build.rs` sources / root |
| `deps` | compile (action 1) | rlibs to compile the **host** build-script bin (e.g. `@crates__cc…`) — `--extern`, NOT run inputs |
| `crate_features` | both | `--cfg feature` to compile the bin **and** `CARGO_FEATURE_<F>` in the run env |
| `version`, `pkg_name`, `cargo_toml_env_vars`, `rustc_env_files` | run (action 2) | `CARGO_PKG_*` and env-file values in the run env (§5.5, §6.2) |
| `data`, `compile_data` | run (action 2) | files staged into the run action's `inputs` (e.g. things the script reads) |
| `link_deps` | run (action 2) | the `links`-crate build-deps whose `DEP_<LINKS>_*` metadata (§6) is injected into THIS script's env — the cross-build-script channel, not `--extern` |
| `rustc_flags` | compile (action 1) | passthrough to the bin compile |
| any other | — | **loud error** |

So action 1 (`deps`/`srcs`/`crate_root`/`crate_features`/`rustc_flags`) builds the host bin;
action 2 (`data`/`link_deps`/env attrs) runs it. The blake3 shape is implementable from this
without guessing.

**`rerun-if-*` — slice-1 simplification (resolves review P1-rerun).** Dynamic dep discovery is
deferred. Slice 1 keys the run action **statically** on the full input set (build.rs + all crate
package files + declared data + the env allowlist), so first run and re-runs key identically
(conservative over-invalidation, never stale). `rerun-if-env-changed=X` adds `X` to the env
allowlist (hence the key). `rerun-if-changed` directives are **parsed and recorded but not yet
used to narrow** the watch set; narrowing is a later two-phase/dynamic-dependency model (§10),
not a correctness gap.

### 5.3 `rust_proc_macro` — slice 1 is single-toolchain (resolves review P1-procmacro)

**Decision: slice 1 implements a `rust_proc_macro` native rule using the single current
toolchain and the host dylib suffix** (`--crate-type proc-macro` → `lib<name>.{dylib,so}`,
dependents `--extern <name>=<dylib>`). **No second `bin_dir` arm, no config-keyed cache** — razel
has no transitions and for host==target none are needed. The exec/target configuration split is
**out of scope** (cross-compilation only) and tracked in §10; nothing in §7 or §9 should assume
it.

### 5.4 `select()` + `target_compatible_with` (resolves review P2-compat)

- `selects.rs` resolves `select()` against `config_setting`s; the new work is a **condition
  source** synthesized from the configured target triple so `@platforms//cpu:*`/`os:*` /
  `@rules_rust//rust/platform:*` arms resolve. A subset of constraints (those the graph actually
  gates on) is enough; expand as goldens demand.
- **`target_compatible_with` has observable behavior:** razel evaluates it against the platform.
  If it resolves to `@platforms//:incompatible` (the generated `//conditions:default` arm), the
  target is **incompatible**: razel produces **no actions** for it, it is **skipped** in
  wildcard/transitive builds (matching Bazel), and an **explicit** request for it is a **loud
  error** (`target X incompatible with the target platform`). Parity expectation: the target is
  absent from both razel's and Bazel's built set on that platform.
- **Incompatible *dep* of a compatible target (review P2-incompat-dep):** if a compatible target,
  after `select()` resolution, still has a direct dep that is incompatible, razel **fails loudly**
  — it never silently drops the edge. The correct graph selects such deps away via the dep's own
  `select()`; an edge that survives to an incompatible target means either a select/platform bug
  or a genuinely unbuildable target, and both must surface, not hide. (Matches Bazel: a compatible
  target cannot depend on an incompatible one.)

### 5.5 `rust_library` attribute surface — the full contract (resolves review P1-attrs)

Every attribute crate_universe emits gets one of three verdicts; **an unrecognized attribute is
a loud error, never a silent skip.**

| attr | verdict |
|------|---------|
| `crate_root`, `srcs` | implemented — the rustc crate root + sources (`srcs` may be a `glob()`, §5.6) |
| `crate_name` | implemented — `--crate-name` (producer-canonical; §4.2) |
| `edition` | implemented — `--edition` (present on every generated target; was missing from rev 3) |
| `crate_features` | implemented — `--cfg feature="X"` + `CARGO_FEATURE_X` to build scripts |
| `deps` | implemented — `--extern` rlibs, **except** `:build_script_build` (the §4.3 edge) |
| `proc_macro_deps` | implemented — `--extern` proc-macro dylibs (§5.3) |
| `aliases` | implemented — consumer-edge `--extern <alias>=<rlib>` (§4.2) |
| `rustc_flags` | implemented — passthrough |
| `rustc_env`, `rustc_env_files` | implemented — `rustc_env` is literal env on the rustc action; `rustc_env_files` are env-files read by the wrapper at exec time (§6.2) |
| `compile_data` | implemented — `include_str!`/`include_bytes!` inputs |
| `version`, `pkg_name` | implemented — literal `CARGO_PKG_VERSION` / `CARGO_PKG_NAME` env |
| `cargo_toml_env_vars` (loaded macro + `:cargo_toml_env_vars` target) | implemented — a native target emitting an env-file of `CARGO_PKG_*` from the crate's `Cargo.toml`; referenced via `rustc_env_files` (§6.2) |
| `link_deps` | implemented — `links`-crate native link flags + `DEP_*` (§6.1) **propagate via a `DepInfo` projection to the consuming `rust_binary`'s final link**; NOT applied at the rlib compile (rules_rust's posture). For an rlib (rung 1) they are recorded + propagated, not applied. |
| `target_compatible_with` | implemented — §5.4 |
| `cargo_build_script` (the macro/target) | implemented — §4.3, §5.2 |
| (always) | `--cap-lints allow` — a dep must not fail on its own lints |
| `data` | **ignored at compile** (runtime/test data; no compile effect) — recorded parity deviation |
| `tags` (`["cargo-bazel", "manual", "noclippy", …]`) | **ignored for the action graph** — Bazel parity is "no effect on actions". Present on *every* generated target, so this MUST be in the allow-set or the loud-error trips universally. |
| `visibility` | **ignored** — `@crates` deps are referenced by label; razel does not enforce visibility for external crates |
| any other (genuinely unknown) attr | **loud error** — unsupported; do not silently compile |

### 5.6 The generated-BUILD load surface (resolves review A P1-load-surface)

> **Shared-loader obligation (added with Part B, §11).** Loading a target — workspace or
> `@crates` — MUST yield a **§11 loading-phase node** (its raw attrs, unresolved `select()`s, and
> label-edges) *before* analysis, not just an `AnalyzedTarget`. The build then transforms it
> (§5.5); `razel query` reads it as-is. Implementing Part A's loader to discard the raw layer
> would silently block Part B, so this retention is a Part A requirement regardless of which ships
> first.

A per-crate `build_file_content` is more than `rust_library`/`cargo_build_script` *targets* — it
**loads modules and uses builtins** razel must provide. Today `rust_rules::module()` synthesizes
only `@rules_rust//rust:defs.bzl`. The generated blake3 BUILD also does:

```python
load("@rules_rust//cargo:defs.bzl", "cargo_build_script", "cargo_toml_env_vars")
load("@rules_rust//crate_universe/private:selects.bzl", "selects")
```

plus `glob()`, `alias()`, and `select()` in attrs. Required loader surface:

| Load / construct | razel home |
|------------------|------------|
| `@rules_rust//cargo:defs.bzl` | a **synthetic module** exposing the natives `cargo_build_script` (§5.2) and `cargo_toml_env_vars` (§6.2) — new, alongside the existing `rust:defs.bzl` module |
| `@rules_rust//crate_universe/private:selects.bzl` (`selects`) | a `selects` helper over `select()` (`with_or`-style); a passthrough stub suffices if it only constructs the arms `selects.rs` already resolves |
| `glob()` in `srcs`/`compile_data`/`data` | `glob.rs`, run against the **materialized external crate tree** (§5.1), not the workspace |
| `alias()` (`:build_script_build`, root `@crates//:X`) | existing alias resolution (`deps.rs`) — §2.2, §4.3 |

Without §5.6 an implementer assumes §5.5's attrs are the only new surface and misses the `cargo`
defs module entirely — the package would fail to *load* before any attr verdict applies.

## 6. Build-script & env file schemas — normative (resolves review P2-format, P2-env)

### 6.1 The build-script flags file

The boundary between the build-script runner (§5.2 step 2) and the rustc wrapper (step 3); it
MUST be precise, with golden tests on the parser.

- **Input grammar:** the runner reads the script's stdout. Accept **both** `cargo:KEY=VALUE`
  (pre-1.77) and `cargo::KEY=VALUE` (1.77+). One directive per line.
- **Recognized directives**, preserving **emission order** and **duplicates** (link order is
  significant): `rustc-cfg`, `rustc-env`, `rustc-link-lib`, `rustc-link-search`, `rustc-link-arg`,
  `rustc-flags`, `rustc-cdylib-link-arg`.
- **`rustc-flags` is tokenized at parse time**, not by the wrapper — the runner splits it into
  individual rustc args by rustc's own rules (whitespace-separated, no shell quoting) and stores
  the **already-split arg list**. The wrapper never re-parses or shell-quotes, eliminating the
  tab/space/quoting ambiguity the review flagged.
- **`metadata=K=V`** (and, pre-1.77, any **non-reserved** `cargo:K=V` from a `links` script) →
  recorded and republished to dependents' build scripts as `DEP_<LINKS_UPPER>_K` (the §5.2
  channel). A pre-1.77 key that *is* a reserved directive name is treated as that directive.
- **`warning=…`** → stderr WARNING (not in the file). **`error=…`** → **fails the run action**.
- **`rerun-if-changed` / `rerun-if-env-changed`** → recorded (§5.2 slice-1: not yet narrowing).
- **Unknown reserved `cargo::<key>`** → recorded + one parity-deviation log line; never silently
  dropped, never fatal.
- **File format — structured, not `kind\tvalue`:** one JSON object per line,
  `{"kind": "<directive>", "args": ["<already-tokenized>", …]}`, in emission order. JSON handles
  values containing tabs/spaces/`=`; paths inside `args` are normalized exec-root-relative. The
  wrapper maps each `kind` to its rustc flag(s) verbatim from `args` — no tokenization, no quoting.

**The wrapper's normative `kind` → rustc mapping** (golden-tested, ≥1 blake3 line per kind once
execution parity lands):

| `kind` | rustc effect |
|--------|--------------|
| `rustc-cfg` | `--cfg <args…>` |
| `rustc-env` | env injection on the rustc process (NOT argv) |
| `rustc-link-lib` | `-l <args…>` |
| `rustc-link-search` | `-L <args…>` |
| `rustc-link-arg`, `rustc-cdylib-link-arg` | `-C link-arg=<arg>` (one per arg) |
| `rustc-flags` | the pre-split `args` appended verbatim (runner already tokenized; §6.1) |

### 6.2 The env-file format (`cargo_toml_env_vars` / `rustc_env_files`)

`cargo_toml_env_vars` emits an env-file consumed via `rustc_env_files` (§5.5) by both normal rustc
actions and build-script runs.

- **Format:** newline-delimited `KEY=VALUE`; `VALUE` is the raw Cargo.toml-derived string, no
  shell quoting (the wrapper sets env directly, not via a shell). Keys are `CARGO_PKG_*`
  (`VERSION`, `NAME`, `AUTHORS`, `DESCRIPTION`, `REPOSITORY`, `LICENSE`, the `VERSION_{MAJOR,…}`
  splits).
- **Precedence:** literal `rustc_env` (and explicit `version`/`pkg_name`, §5.5) **override**
  env-file entries; among multiple `rustc_env_files`, **last wins**. Stated so the order is not
  left to the implementer.

## 7. Integration seams (reconciled with §4–§6)

| Piece | Home in razel |
|-------|----------------|
| lock read + repo materialization | a `MODULE.bazel.lock` reader (§2) + `fetch.rs` realize + a `RepoFetch` action in `razel-exec` |
| generated-BUILD load surface | a synthetic `@rules_rust//cargo:defs.bzl` module (`cargo_build_script`, `cargo_toml_env_vars`) + `crate_universe/private:selects.bzl` + `glob()` on external trees + `alias()` — §5.6 |
| build-script run + flags file | `CargoBuildScriptRun` in `rust_rules.rs`; the flags file (§6); the **intra-target edge** (§4.3) — **not** a DDS provider fold |
| rustc wrapper (exec-time flags) | a small wrapper binary the rustc action invokes (`process_wrapper` analogue) |
| proc-macro | `rust_rules.rs` `rust_proc_macro`, single toolchain + dylib suffix (§5.3) — **no `bin_dir` arm** |
| select + target_compatible_with | `selects.rs` (triple→condition source) + incompatible-target handling (§5.4) |
| rust_library attrs | `native_rust_library` attr surface → `rustc()` argv (§5.5) |
| crate / build-script contract | `deps.rs::DepInfo` (+ `crate_name`, `proc_macro`, `BuildScriptRun`) |
| OUT_DIR / tree outputs | `razel-exec` directory capture (walk + content-address) — new (§10) |

## 8. Parity — analysis vs execution (resolves review P1-parity)

Build-script effects are runtime, so **`aquery` alone cannot gate them.** Two diffs:

- **Analysis parity (aquery).** `bazel aquery deps(@crates//:X)` → `razel_parity::normalize` →
  `diff` against razel's **static** `AnalyzedAction` graph: the action structure, deps, and the
  rustc argv **known at analysis** (everything except the flags-file-derived part). Allowlisted
  deviations expected — `process_wrapper` indirection, `-Cmetadata=<hash>`, `--extra-filename`,
  `--remap-path-prefix` — exactly as the cc goldens already carry an `-iquote` deviation. The
  gate is "no *un*documented mismatch," not byte-identity.
  - **Pinned argv shape (review A P2-argv):** razel's analyzed `Rustc` action argv is
    `[<wrapper>, --rustc=<rustc>, --flags-file=<bs flags or "">, --env-file=<env files…>, --, <rustc args…>]`
    — the wrapper is **explicit in argv**, not implied by mnemonic. The normalizer strips the
    `<wrapper> --rustc= … --` prefix (and the flags/env-file paths) to a canonical token so it
    diffs against Bazel's `process_wrapper` prefix without per-crate special-casing.
- **Execution parity (post-run).** After running the build script, diff razel's **flags file**
  (§6.1) against Bazel's equivalent. **How Bazel's artifact is obtained (review P1-exec-parity):**
  rules_rust's `cargo_build_script` runs the script through its `cargo_build_script_runner`, which
  writes the parsed directives to **declared output files** of the `*_build_script_` /
  `:build_script_build` target — concretely a `<name>.out` flags file and an `OUT_DIR` tree under
  `bazel-bin/external/<canonical crates repo>/…`. The harness captures them with
  `bazel build @crates//:<crate>__build_script_build` and reads those output files (or reads the
  `BuildInfo`/`DepVariantInfo` provider via `bazel cquery --output=starlark`). razel's flags file
  (§6.1) and that capture are the same directive set, normalized the same way, and diffed. The
  exact filenames are pinned in the harness's capture script, not inferred at compare time.

A crate without a build script needs only analysis parity; one with a build script needs both.

## 9. Milestones (the plan) — test-first (resolves review P2-milestones)

Each rung names labels, the Bazel and razel commands, the required goldens, and explicit
non-goals.

1. **blake3** — `@crates//:blake3` (aliases `@@rules_rust++crate+crates__blake3-1.8.2//:blake3` via the root repo, §2.2/§11.3).
   - Bazel: `bazel aquery 'deps(@crates//:blake3)'` (analysis golden); `bazel build @crates//:blake3__build_script_build` then read its `<name>.out` flags file + `OUT_DIR` from `bazel-bin/external/…` (execution golden, §8).
   - razel: `razel build @crates//:blake3`.
   - Goldens: analysis-parity diff (§8); execution-parity flags-file diff vs that capture; the produced rlib exists; the build script's cc-compiled SIMD `.o`s appear in `OUT_DIR`.
   - Covers: §2.2 root+per-crate materialization, §5.1 fetch, §5.6 load surface (`@rules_rust//cargo:defs.bzl`, `glob()`), §5.2 build script + **cc env** + **tree `OUT_DIR`**, §5.4 **`select()` + `target_compatible_with`** (see below), §5.5 `crate_features`/`edition`/`tags`/`cargo_toml_env_vars`.
   - **`select()` is IN-SCOPE for blake3 (corrects rev-3 review A):** blake3's generated `build_file_content` applies `target_compatible_with = select({…"//conditions:default": ["@platforms//:incompatible"]})` on both the lib and the build script (227/227 crates do), so resolving the select + confirming host-compatibility is a *precondition to analyzing blake3 at all* — it cannot be deferred to rung 3. Rung 1 does the **platform-gating** subset; rung 3 adds richer per-cfg dep selection.
   - **`link_deps` edge present, `DEP_*` deferred:** blake3's build script carries a `link_deps` edge (`@crates__rayon-core//:rayon_core`); rung 1 wires it **structurally** (the build-dep is available), but blake3 does not *consume* `DEP_*` metadata, so an empty channel is acceptable. A crate that actually reads `DEP_<links>_*` is the deferred long tail.
   - Non-goals: proc-macros; `rerun-if` narrowing (conservative re-run accepted); stale-lock validation (assume fresh); rich `DEP_*` propagation.
2. **serde_derive** — `@crates//:serde_derive`. Adds §5.3 proc-macro (host dylib, single toolchain). Non-goal: exec/target split.
3. **libc / getrandom** — adds the **richer** §5.4 `select()` (per-cfg platform deps, beyond rung-1's compatibility gating); the incompatible-target negative case (absent / loud-errors) is a required golden. Non-goal: full `@platforms` lattice.
4. **the full `@crates`** — scale: the resolved graph builds end to end; analysis parity green across it (documented deviations only).
5. **the binaries** — `razel build //crates/razel-cli:razel` with no Bazel involved, dogfooded.

## 10. Remaining open questions / risks

The decisions above close the rev-1 unknowns; what genuinely remains:

- **The loading-phase graph layer (shared with Part B, §11).** razel's IR is the analyzed graph
  today; the loader must grow a retained loading-phase layer (raw attrs + unresolved selects +
  label-edges). It is the single largest shared-foundation item and a prerequisite for both the
  faithful loader and `razel query`. Build it once.
- **Tree-output executor support.** §5.2's `OUT_DIR` and §5.1's extracted trees need
  directory capture in `razel-exec` (today `fs::copy` per declared file). Prerequisite for rung 1.
- **The rustc wrapper.** A `process_wrapper` analogue (read flags file → append) is new infra;
  small but on the rung-1 path.
- **Lock-format coupling (§2).** Schema-version-pinned reader + parity harness; cargo-bazel
  shell-out is the fallback.
- **Build-script long tail.** sys-crates needing system libraries, scripts that subprocess or
  probe outside the env allowlist, link ordering. The real cost — surfaced rung by rung.
- **Cross-compilation.** The exec/target configuration split (§5.3) is deferred; razel has no
  transitions. Out of scope until a cross target is required.
- **`rerun-if` narrowing.** The dynamic-dependency model (§5.2) is a post-correctness
  optimization, not slice 1.
- **`expand_pattern` load-only fork.** `razel-build::expand_pattern`/`discover_packages` analyze to
  enumerate today; query needs a load-only mode (§13). The landing point for the q1 ticket.

**Build-order sequencing (review A-1 P2-sequencing)** — so Part A's loader refactor and q1 do not
fork incompatible IR shapes, build the shared foundation once, in order:
1. **§11 `LoadedTarget`/`QueryNode` capture** in the loader (workspace packages first).
2. **q1 goldens** (workspace `deps`/`kind`/`label_kind`) — proves §11 on the workspace.
3. **Part A `@crates` materialization + §5.6 external load** (may overlap q2–q3).
4. **q4 `@crates` goldens** — only after the §11.2 `deps()` select-condition fix, the per-attr
   `labels()` goldens, and traversal reachability into `@rules_rust`/`@@platforms` (§13) are in.

---

# Part B — `razel query`

## 11. The shared target graph (why Part A and Part B are one design)

**The collision, if designed apart.** Part A's build wants the **analyzed** graph — rules run,
`select()` resolved to the platform, actions minted (today `razel_ir::Graph`, built from
`AnalyzedTarget`; the rdep index `affected` walks). `bazel query` wants the **loading-phase**
graph — every declared target with its **raw** attributes, `select()` **unresolved**, the full
union of label-edges (incl. implicit/toolchain edges), and **no analysis**. razel has only the
analyzed graph today. If Part A built its loader to go straight to `AnalyzedTarget` (discarding
raw attrs + unresolved selects), `razel query` would have no faithful graph to read and would
either diverge from `bazel query` or force a second load. That is the "working against each
other" this augmentation prevents.

**Decision: one loader, two layers.** The loader produces a **loading-phase target graph** as the
shared foundation:
- a node per declared target (workspace **and** `@crates`) carrying its `TargetKind`, its **raw
  attribute values** (including the literal `select()` expressions, unresolved), and
- the **label-edge set** = the union of every label-valued attribute (`deps`, `srcs` labels,
  `proc_macro_deps`, `data`, the `:build_script_build` alias, …), with implicit/toolchain edges
  **flagged** so `--[no]implicit_deps` can include/exclude them.

Part A's analysis (`AnalyzedTarget` → `razel_ir::Graph`, §3 steps 4–7) is a **transform on top**
of this layer. `razel query` (Part B) **reads the loading-phase layer directly and never triggers
analysis** — fast and `bazel query`-faithful. The two share the loader, the materialized `@crates`
(§2/§5.1), `selects.rs`, `expand_pattern`, and `TargetKind`; they do not collide because query is
loading-phase-read-only and the build is the analysis transform.

**Implementation reality (an §10-class gap).** Today the IR is the analyzed graph; the
loading-phase layer (raw attrs + unresolved selects retained through load) is **new**. It is the
one piece Part A's loader must retain *whether or not query ships first* — once query exists it is
the only faithful source. crate_universe's `@crates` loading (§2.2, §5.5/§5.6) populates
loading-phase nodes for the external targets, so `query @crates//…` is nearly free once the layer
exists.

### 11.1 The `LoadedTarget` / `RawAttr` schema (review 55-3 P1-schema)

The loading-phase node is a **serializable, de-Starlark'd** value captured at load — NOT a Starlark
`Value` (those are heap/lifetime-bound, frozen in module heaps, not `Send`/snapshot-friendly, and
query must outlive the load and stringify stably for `attr()`):

The query graph node is a **`QueryNode`**, not only a rule target — Bazel `deps()` emits source
and generated **file** labels too, and `kind()` classifies them (review 55-4 P1-nodes):

```
QueryNode =
  | Target(LoadedTarget)             // a rule target (incl. `alias`)
  | SourceFile { label }             // a package source file
  | GeneratedFile { label, by: CanonicalLabel }  // an output → its generating rule
  | Implicit { label, rule_class }   // a synthesized implicit/toolchain node (§13)

LoadedTarget {
  label:       CanonicalLabel,       // identity: the bzlmod-canonical form (§11.3)
  repo, package,
  rule_class:  String,               // "rust_library", "cargo_build_script", "alias", … — the
                                     //   QUERY-facing kind, retained at load (NOT the coarse TargetKind)
  kind:        TargetKind,           // the build's coarse Library/Binary/Test (action minting only)
  attrs:       Map<AttrName, RawAttr>,
  edges:       Vec<Edge>,            // §11.2
}
RawAttr =                            // the closed, serializable attr model
  | Str(String) | Int(i64) | Bool(bool) | None
  | List(Vec<RawAttr>) | Tuple(Vec<RawAttr>)   // Tuple: `selects.with_or` condition groups pre-desugar
  | Dict(Vec<(RawAttr /*key may be a Label*/, RawAttr)>)
  | Label(CanonicalLabel)
  | Select { arms: Vec<(CanonicalLabel /*condition*/, RawAttr)>, default: Option<Box<RawAttr>> }
  | Concat(Vec<RawAttr>)             // models `list + select(...)`
```

- **Captured at load, before freeze:** the loader snapshots each attr into `RawAttr`; no Starlark
  heap escapes into query. `QueryNode` is `Send` + snapshot-friendly (interoperates with the
  `RazelDepsEngineV2` snapshot model). `rule_class` is the rule macro the target was declared with.
- **Label extraction** walks `List`/`Tuple`/`Dict`(keys+values)/`Label`/`Select` recursively to
  build `edges` (§11.2). `None` extracts nothing.
- **Canonical stringification (for `attr()`/`--output=build`)** is normative, so goldens aren't
  "whatever the first impl printed": Starlark-`repr`-shaped — `List`=`[a, b]`, `Tuple`=`(a, b)`,
  `Dict`=`{k: v}` in **source/insertion order**, `Label`=its canonical `@repo//pkg:name`, `Select`
  =`select({cond: v, …}, default=…)` with arms in source order, `Concat`=`a + b`, `None`=`None`,
  `Str` double-quoted with `\"`/`\\`/`\n` escaping. `attr(name, regex, x)` runs the regex over this
  string; an unrepresentable value is a loud error, never a silent skip.

### 11.2 Edge kinds and traversal (review 55-3 P1-edges; **corrected against Bazel per review A-1**)

`edges` is typed; `deps()` traverses the union below. **All rules here are verified against
`bazel query 'deps(@@rules_rust++crate+crates__blake3-1.8.2//:blake3, 1)' --noimplicit_deps`.**

| Edge kind | source | traversed by `deps`/`rdeps`? |
|-----------|--------|------------------------------|
| `Rule` | a dep-attr label → a rule target (`deps`, **`rustc_env_files` → the `cargo_toml_env_vars` rule**, …) | yes |
| `Alias` | `alias(actual=…)` — `@crates//:blake3` → `@@rules_rust++crate+crates__blake3-1.8.2//:blake3` (verified, §11.3) | yes, **transparently to the actual**; both labels appear in `--output` |
| `SourceFile` | a `srcs`/`hdrs`/`data`/**`compile_data`** label that is a source file (blake3's `compile_data` `glob()` contributes ~80, e.g. `:src/platform.rs`, `:build.rs`) | yes (Bazel `deps` includes source files) |
| `GeneratedFile` | a label that is another rule's output | yes → the generating rule |
| `ConfigSetting` | **a `select()` CONDITION label** (`@rules_rust//rust/platform:*`, `@@platforms//:incompatible`) | **yes — verified: `deps()` includes condition labels** |
| `Implicit` / `Toolchain` | rule-implicit / toolchain edges (§13) | only with `--implicit_deps` (default **on**, §12) |

**`deps()` over a `select()`-valued attr = condition labels + all arm values + the default**
(unresolved). My rev-7 claim that condition labels are *not* deps was **wrong**: `deps(blake3,1)`
includes the eight `@rules_rust//rust/platform:*` `config_setting`s and `@@platforms//:incompatible`.
**Source confirms the mechanism (not just the output):** `AggregatingAttributeMapper.visitLabels`
is called with `includeSelectKeys = true` (`AggregatingAttributeMapper.java:96-109` via
`LabelVisitationUtils.visitRule`), and `visitLabelsInSelect` visits each condition key **except the
synthetic `//conditions:default`** (`Selector.isDefaultConditionLabel`). So razel's `ConfigSetting`
edges = the real condition labels (not `//conditions:default`) + every arm value + the default
arm's value. Every `@crates` rust target carries this `target_compatible_with` select (227/227),
so q4 goldens require it. `labels()` is the separate, narrower view (§12).

**Macro children are loading-phase nodes.** `cargo_build_script` macro-expands at load into
`:_bs` (`cargo_build_script`), `:_bs-` (`cargo_build_script_runfiles`), `:_bs_` (`rust_binary`),
and the `:build_script_build` alias; `deps()` traverses into them (Bazel parity), so the native
`cargo_build_script` rule (§5.6) MUST declare those child `QueryNode`s at load. `labels("deps")`
shows only the `:build_script_build` alias (§12) — the raw attr, not the expansion.

### 11.3 Repo identity at the query boundary (review 55-4 P1-repo)

Verified against `bazel query 'deps(@crates//:blake3, 1)' --output=label_kind`:
```
alias rule        @crates//:blake3
rust_library rule @@rules_rust++crate+crates__blake3-1.8.2//:blake3
```
So:
- **Canonical identity** of a per-crate target is the bzlmod-canonical
  `@@rules_rust++crate+crates__<name>-<ver>//:<crate>` (NOT `@crates__<name>-<ver>`). That is the
  `LoadedTarget.label` and what `--output` prints for the actual; the `@crates//:x` apparent label
  prints for the alias node.
- **razel query accepts BOTH** the apparent (`@crates//:x`, `@crates//...`) **and** the canonical
  (`@@rules_rust++crate+crates__…//…`) as query literals — so a label a user sees in `deps()`
  output is composable back into `kind(...)`, `rdeps(...)`, `somepath(...)`. Any other `@repo//…`
  remains a loud error in v1.
- The q4 golden normalizer compares razel's labels to Bazel's verbatim in this canonical form; no
  per-name remapping invented.

## 12. `razel query` — syntax

A subset of `bazel query`'s language over the §11 graph. v1 names the operators that map onto the
graph razel has; the rest are deferred (named, not silently dropped).

**Target patterns** (via `expand_pattern`, shared with the build): `//pkg:target`, `//pkg:all`,
`//pkg/...`, `//...`, and the `@crates//…` forms — `@crates//:x`, `@crates//pkg:all`,
`@crates//...`. Any other `@repo//…` is a **loud error** in v1 (only `@crates` is materialized).

**v1 operators:**
| expr | meaning |
|------|---------|
| `deps(x[, depth])` | forward transitive closure over the label-edges (unbounded by default) |
| `rdeps(universe, x[, depth])` | reverse closure within `universe` (the §11 reverse edges) |
| `kind(regex, x)` | filter by the **kind string** (§ below), not the coarse `TargetKind` |
| `somepath(x, y)` / `allpaths(x, y)` | one path / all-paths node-set x→y over the edge set |
| `filter(regex, x)` | filter the set by a regex over labels |
| `attr(name, regex, x)` | filter by the canonical stringification of a `RawAttr` (§11.1) |
| `labels(attr, x)` | the **raw** label literals of `attr` (§ below) |
| set algebra | `x + y`, `x - y`, `x intersect y`, `x union y`, `let v = e in e`, `( … )` |

**`kind()` / `--output=label_kind` strings (review 55-4 P1-kind + A-1 P2-kind), verified against
Bazel — an OPEN set, not a closed enum:** any `Target(LoadedTarget)` prints `"<rule_class> rule"`
where `rule_class` is the loaded rule's **registered name** — so `"rust_library rule"`, `"alias
rule"`, `"cargo_build_script rule"`, `"cargo_toml_env_vars rule"`, `"cargo_build_script_runfiles
rule"`, `"config_setting rule"`, `"constraint_value rule"` all fall out automatically. A
`SourceFile` node prints `"source file"`, a `GeneratedFile` node `"generated file"`. `label_kind`
prints `"<kind> <label>"`; `kind(regex, x)` runs the regex over it — the 3-value action `TargetKind`
is **not** what query filters on. Goldens are per-rung, not per-crate.

**`labels(attr, x)` is attr-specific and NARROWER than `deps()` (review 55-4 P2 + A-1 P1).**
It returns the **label values of that one attr's `RawAttr`**, NOT edge-resolved and NOT the deps
closure. **Source confirms the exact reason:** `LabelsFunction` → `BlazeTargetAccessor` calls
`getReachableLabels(attr, /*includeSelectKeys=*/false)` (`BlazeTargetAccessor.java:81`) — the *same*
`AggregatingAttributeMapper` as `deps()` but with the select-keys boolean **false**. So the
deps/labels asymmetry is literally that one parameter: `deps()` = `includeSelectKeys=true`,
`labels()` = `false`. Verified output: `labels(deps, blake3)` → the dep labels incl. the
`:build_script_build` **alias** (unresolved); `labels(target_compatible_with, blake3)` →
**`@@platforms//:incompatible` only** (default arm value, **no condition labels**). Because the
result is attr-specific, the contract is **per-attr goldens** (at minimum `labels(deps,…)` and
`labels(target_compatible_with,…)` on blake3), not one recursive walk.

**Flags / output (v1):** `--output=label` (default) / `label_kind`. **`--[no]implicit_deps`
defaults to `--implicit_deps` (true)** — Bazel 9.1.1's verified default — but the **core q-goldens
run with `--noimplicit_deps`** (§13) to decouple from implicit-label fidelity, which is its own
rung. `--cbor` is **deferred** (§13), not a v1 surface.

**Deferred (named, with reason):**
- `tests(x)` — needs `test_suite` expansion + suite tags + `manual` handling + finer kind
  classification than `Library/Binary/Test`. (Own rung.)
- `--output=build` — needs a stable renderer over the **macro-expanded loaded** target (`RawAttr`,
  §11.1), not source text. `--output=package` — package metadata + deterministic formatting.
  `--output=graph` — graphviz formatting. `--output=proto`/`xml`. (Each its own rung, §13.)
- `cquery` (the *configured/analyzed* graph — a separate verb over Part A's analyzed layer),
  `buildfiles()`/`loadfiles()`, `siblings()`, `visible()`, `rbuildfiles()`.

## 13. `razel query` — execution

`cmd_query` (a new CLI verb): **parse → evaluate over the §11 loading-phase graph → format**, with
**no analysis**.

- **Parser** — recursive-descent over §12's grammar → an expression AST (word vs `"quoted"`
  tokens, `let` bindings, depth args). Golden-tested.
- **Loading without analysis (review 55-3 P1-pattern; A-1 P2-fork).** Query pattern expansion
  **loads packages but never analyzes them** — it walks the same BUILD/BUILD.bazel resolution +
  `.bazelignore` rules the build uses to enumerate the §11 `QueryNode`s, stopping before §3 step 4.
  The concrete fork target is **`razel-build::expand_pattern` / `discover_packages`**
  (`crates/razel-build/src/lib.rs:90,125`), which today *analyzes* to enumerate — parameterize it
  with a load-only mode (an §10 prerequisite). Package-load errors are reported, not swallowed.
- **Traversal reachability — pattern entry ≠ traversal closure (review A-1 P1-reach).** Query
  **pattern entry points** stay `@crates`-only (other `@repo//…` patterns → loud error, §12), but
  **`deps()`/`rdeps()` traversal may leave `@crates`**: verified, `deps(@crates//:blake3)` reaches
  `@rules_rust//rust/platform:*` and `@@platforms//:incompatible`. When traversal follows an edge
  into `@rules_rust//…` or `@@platforms//…`, razel loads that target from the workspace's existing
  external resolution (MODULE.bazel / the bazel `external/` tree), the same source the build loader
  uses; an edge into a repo that is neither workspace, materialized-`@crates`, nor a resolvable
  external is a loud error. So q4 `deps` goldens need `@rules_rust` + `@@platforms` loadable, not
  just `@crates`.
- **Query MAY materialize, but never analyze (review 55-4 P1-materialize).** "No analysis" ≠ "no
  materialization": loading an `@crates` package needs its source tree + `build_file_content`, so
  query runs the **same lock-only `RepoFetch` path** (§5.1) on demand, materializing exactly the
  per-crate repos its traversal touches (`deps(@crates//:x)` realizes x's repo + the repos it walks
  into; `@crates//...` realizes the set the pattern enumerates). Query first runs §2.3 stale-lock
  detection (loud error if stale), same as the build. Network posture = the build's (`RepoFetch`
  fetches, or the dev-cache interim reads Bazel's `external/`; the interim is **not** q4-parity-gating,
  §5.1). It never triggers §3-step-4 analysis or a daemon (slice 1).
- **Adjacency is the §11 loading graph, NOT the analyzed IR (review 55-3 P1-graph).** `razel_ir::Graph`
  is files+actions+targets (output-sensitive, no unresolved-select edges); query must **not** run on
  it. Query maintains its **own** forward/reverse adjacency over the `LoadedTarget` label-edges
  (§11.2) — reusing the *bidirectional-adjacency pattern* `affected` proved, not the index itself.
  `deps`/`rdeps` are BFS over that adjacency; `somepath`/`allpaths` graph search; `kind`/`filter`/
  `attr` predicates over kind/`RawAttr`; set ops; `let` an environment. Cycle-safe; `rdeps` bounded
  by its `universe`.
- **`select()` UNRESOLVED** — `deps()` over a `Select`/`Concat` (§11.1) yields the **condition
  labels + all arms' values + the default** (§11.2; source: `AggregatingAttributeMapper` with
  `includeSelectKeys=true`, excluding only the synthetic `//conditions:default` key). `labels()`
  is the narrower view — arms' values only, `includeSelectKeys=false` (§12). Nested selects recurse;
  `selects.with_or` desugars to arms at load. Query never asks `selects.rs` to resolve; the build
  does — one subsystem, two callers (§14).
- **Implicit/toolchain edges — labels and the slice-1 posture (review 55-3 + 55-4 P2-implicit).**
  razel's native rules have no Starlark implicit attrs and invoke resolved paths (`/usr/bin/rustc`,
  `/usr/bin/cc`), so they have **no toolchain target *labels*** to emit — the implicit deps Bazel
  shows (`@@rules_rust++…//rust/toolchain:…`, `@@bazel_tools//tools/cpp:…`, the process-wrapper,
  the allocator) are **not reproducible by label in slice 1**. Therefore: the **core q-goldens run
  with `--noimplicit_deps`** (the reproducible explicit graph); matching Bazel's implicit labels is
  a **named later rung** with an allowlisted-deviation list, and until then `--implicit_deps` (the
  flag default, on) emits only the implicit `QueryNode`s razel can faithfully synthesize (initially
  none for native rust, so it equals `--noimplicit_deps` plus a logged deviation). crate_universe
  targets' implicit deps that appear *in their `build_file_content`* (real label edges) are honored
  normally; only rule-injected toolchain edges are the gap.
- **Output + normalization (review 55-3 P2-output, 55-4 P2-cbor; source-corrected).** v1:
  `label`/`label_kind` only, text. Results → **stdout** (query output IS data); progress → **stderr**.
  `--cbor` is **deferred** to the output-formats rung (it would otherwise mint an unschematized
  public wire contract). Goldens: razel emits a **lexicographically-sorted** label set. **Source
  mechanism (corrected):** the sort comes from `--order_output=auto` (default) + a `StreamedFormatter`
  (`LabelOutputFormatter` is one) → `QueryOutputUtils.lexicographicallySortOutput()` — *not* from
  `--incompatible_lexicographical_output` as rev 6–8 claimed; that flag is a separate switch. Labels
  in canonical form (§11.3); **regex dialect = Rust `regex` crate** (allowlisted deviation from
  Bazel's Java regex).
- **`somepath`/`allpaths` — source-corrected (review 55-4 P2-somepath).** rev-6's "razel's pinned
  edge order matches Bazel" was **wrong**: Bazel's `SomePathFunction` does a BFS via
  `Digraph.getShortestPath` over `getSuccessors()` order and returns a path to **an arbitrary node
  in the from∩to intersection** (`result.iterator().next()`) — i.e. *a* shortest path, with the
  choice among equal-length paths driven by internal successor ordering. So **exact-sequence parity
  is not achievable**; the `somepath` golden must accept **any valid shortest path** (verify it is a
  real path of minimal length), and only assert an exact sequence when the shortest path is unique.
  razel still fixes its *own* edge order (sorted, `Rule→Alias→SourceFile→GeneratedFile→Implicit`) for
  internal determinism, but that is for razel's stability, not Bazel match. `allpaths` returns the
  from→to subgraph **node-set** (Bazel: forward-closure ∩ reverse-closure), compared as a set —
  order-independent, so it *is* a clean golden. `depth` counts traversed edges (start = depth 0).
- **CLI / daemon routing (review 55-3 P2-cli).** Slice 1 is **CLI-local-only**: a fresh load,
  no daemon, no RPC — `stdout`/`stderr` as above. A daemon-backed `query` (answering over the last
  committed snapshot via the wire surface) is a follow-up that rides the `RazelDepsEngineV2` snapshot
  model; named here so the local path is not mistaken for the final public surface.
- **Naming: `razel query` ≠ V2 `EngineQuery` (review A-1 P2-naming).** `RazelDepsEngineV2.md`'s
  `QueryRequest`/`EngineQuery` are **structured engine queries over snapshots** (a programmatic
  API); the §12 `razel query` is the **Bazel-query-expression language** over the loading graph.
  They are different verbs. The future daemon path exposes the snapshot query (a `QuerySnapshot`-style
  surface), NOT the §12 expression parser — do not conflate them in the wire API.

**Query milestones (its own ladder, gated by `bazel query` goldens — re-tiered per review 55-3):**
- **q1** — patterns + `kind` + `--output=label,label_kind` over the workspace graph (proves the
  loader-without-analysis + the §11 graph + normalization).
- **q2** — `deps`/`rdeps` + set ops + `--implicit_deps` on/off.
- **q3** — `somepath`/`allpaths` + `attr`/`labels` (raw-attr predicates).
- **q4** — the same over `@crates//…`. Requires (review A-1): the §11.2 `deps()` **select-condition
  edges**, **traversal reachability** into `@rules_rust`/`@@platforms` (§13), the `cargo_build_script`
  **macro children** as loading nodes (§11.2/§5.6), `compile_data` glob `SourceFile` edges (§11.2),
  and **per-attr `labels()` goldens** (§12). Shares Part A's materialization + alias traversal.
- **q5+** — `tests()`, `--output=build`/`package`/`graph` (each with its own renderer + ordering).

Each rung diffs `razel query <expr>` against `bazel query <expr>` (with the named deviations) — a
new golden kind alongside the aquery goldens.

## 14. Shared surfaces — the non-conflict contract

The reason §11–§14 live in this doc: every surface both features touch has one owner and a stated
split, so a change is made **once**.

| Surface | Part A — build (analysis) | Part B — query (loading-phase) | The contract |
|---------|---------------------------|--------------------------------|--------------|
| loader | produces loading-phase graph, then analyzes | reads loading-phase graph, no analysis | one loader; the §11 layer is retained for both |
| target graph | `razel_ir::Graph` (files+actions+targets; analyzed) | its **own** label-edge adjacency over §11 nodes | **separate adjacencies**; query reuses the bidirectional-index *pattern* (§13), not the IR — the IR is the wrong graph for query |
| `select()` / `selects.rs` | resolves to the configured platform (§5.4) | reads it **unresolved** (all arms) | one subsystem; resolution is build-only |
| `DepInfo` / edges | analyzed `DepInfo` (`--extern`, `BuildScriptRun` §4.3) | typed raw edges (`Rule`/`Alias`/`SourceFile`/… §11.2) | the loading-phase edge is raw; analysis *interprets* it (query shows what Bazel shows) |
| `@crates` materialization (§2, §5.1) | builds + executes the targets | reads the loading-phase nodes | one materialization; query reads, build builds |
| `expand_pattern` | analyzes packages to enumerate | **load-only fork** — enumerate §11 nodes, no analysis (§13) | one resolver, parameterized by an analyze/load-only mode |
| `TargetKind` | action minting | `kind()` filter | shared |
| `target_compatible_with` (§5.4) | skips/errors the incompatible target | shows it as a node + its raw select | build resolves; query reports the raw form |
| CLI stream discipline | summary→stderr, data→stdout | results→stdout, progress→stderr | one convention (already established) |

If a future change touches any row it is changed once, for both callers — the whole reason this is
one design, not two.
