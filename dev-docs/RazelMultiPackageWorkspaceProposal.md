# Razel Multi-Package Workspace Proposal — autogen BUILD from native manifests

Status: proposal / scoping. Companion to `dev-docs/GrazelProposal.md` (Model G)
and `dev-docs/GrazelForecast.md` (D8 "more languages", Gc5 "adapters lower into
the contract").

## 0. What this is

Goal: point Razel at a tree that already has native package manifests
(`Cargo.toml`, `pyproject.toml`, `package.json`, `go.mod`, `CMakeLists.txt`)
and **derive the implied build/test targets and their dependency edges** —
so we get a true, incremental test+build graph without hand-authoring BUILD.

Framing that fits the existing engine: a manifest is **not** a new build
language. It is an **EDB-fact producer** — a thin adapter that lowers a manifest
+ source layout into the same `Target / Attr / SrcFile / DepEdge` facts the
BUILD/`.bzl` adapter already feeds the DDS (`RazelV2Contracts.md` §0,
`DdsQuerySystem.md` §0). This is exactly `GrazelForecast.md` Gc5/D8: BUILD files,
Cargo, npm, etc. are *all* front-end adapters into one canonical contract. So
this adds **no new core commitment** — it's a new family of loaders behind the
manifest-registry seam.

**Motivating fixtures already in the tree** (use these as the conformance anchors):

- `grip-pyrolyze-dev/astichi` — one repo, *two* ecosystems + a cross-language
  edge: a hatchling Python package (`src/astichi`, ~16 subpkgs, `tests/`) **and**
  a PyO3 Rust `cdylib` (`native_engine/`, crate `astichi_native_engine`). The
  Python wheel depends on the Rust extension's `.so`. It also has a **custom
  codegen hook** (`hatch_build.py`) — the hard case (see gotchas).
- The grip JS packages — a clean first-party DAG via `file:../` links:
  `@owebeeone/grip-core ← @owebeeone/grip-react ← grip-react-demo`, and
  `@owebeeone/grip-core ← @owebeeone/grip-vue` (peer-deps on react/vue).

---

## Q1 — Has anyone done this? Yes, extensively — but fragmented, and split across two problems

The space is mature, but it is **not one tool**. Two distinct problems get solved
by *different* mechanisms, and conflating them is the usual mistake:

- **(A) Third-party resolution** — read a *lockfile*, materialize external deps
  as repos/targets. Deterministic, version-pinned.
- **(B) First-party generation** — read *source layout + import scans*, emit the
  package's own library/binary/test targets and their internal edges.

| Ecosystem | (A) third-party tool | (B) first-party generator | Maturity | Files-in-repo? |
|---|---|---|---|---|
| Go | (go.mod native) | **Gazelle** (native) | Gold standard | yes |
| Rust | **`crate_universe`** (rules_rust); `cargo-raze` *archived* → migrate; `reindeer` for Buck2 | `gazelle_rust` (Calsign; scans `use` paths) | Good (A), decent (B) | yes |
| Python | `pip.parse` / `pip_parse` (rules_python) | `rules_python/gazelle` + `gazelle_python_manifest` (import→distribution map) | Good | yes |
| JS/TS | `npm_translate_lock` (rules_js, pnpm lock) | aspect `rules_ts` + `rules_nodejs_gazelle` (BenchSci) | Good (A), patchy (B) | yes |
| C/C++ (CMake) | — | **none faithful**; `rules_foreign_cc` *wraps* CMake as an opaque action | Weak — the outlier | n/a |

Two framework-level data points worth internalizing:

- **Gazelle** (`bazel-contrib/bazel-gazelle`) is *the* canonical BUILD generator
  and the extension framework everyone plugs into: scan sources → infer deps →
  write/update `BUILD.bazel`, **checked into the repo**. Native Go + protobuf;
  language plugins for the rest.
- **Pants** is the philosophical opposite and the closest sibling to **Grazel**:
  near-zero BUILD boilerplate via **dependency inference** (resolve internal +
  external deps from imports), with `tailor` to generate the little BUILD that
  remains. This is the Model-G bet (`GrazelProposal.md` §1, §4.2) already proven
  to work at scale by someone else.

**Takeaways for Razel:**

1. No single tool spans all 5 ecosystems with one autogen — production setups
   **compose** per-language generators. Our manifest-registry seam is the right
   shape for that.
2. The mature pattern is **lock-driven for (A), scan-driven for (B)**, both
   emitting facts. That maps 1:1 onto DDS EDB facts.
3. **CMake is the outlier** — no robust translator exists, for a real reason
   (Turing-complete configure step). Everyone *wraps* it. Plan for wrap-first.
4. The Gazelle vs Pants split (emit-BUILD-to-disk vs infer-into-engine) is
   precisely our `Gc5` (BUILD-adapter) vs `Gc4` (`.razel`/DDS-native) choice. We
   can do **both**: DDS-native primary, BUILD emission as an export/debug view.

---

## Q2 — The plan

### 2.1 One shape for every ecosystem: the *manifest adapter*

Each ecosystem gets a small, deterministic adapter with the **same contract**:

```
manifest(s) + source tree  ──►  EDB facts
                                  ├─ Target   (lib / bin / test, one per inferred target)
                                  ├─ SrcFile  (globbed sources owned by each target)
                                  ├─ DepEdge   first-party  → internal Label
                                  └─ DepEdge   third-party  → external @repo ref
```

Two sub-passes, mirroring the prior-art split:

- **Pass A (lock → external):** parse the lockfile (`Cargo.lock`, `uv.lock`/
  `requirements`, `package-lock`/`pnpm-lock`, `go.sum`) → one external-dep fact
  per pinned crate/dist/module. Version-pinned, no source scan.
- **Pass B (layout + scan → first-party):** apply the ecosystem's layout
  convention + a light import scan to emit the repo's own targets and their
  internal edges.

Constraints (inherit from `GrazelProposal.md` G-G4): inference must be
**deterministic, provenance-carrying, explainable** — every emitted fact records
which manifest line / source token produced it. No arbitrary I/O in an adapter.

**Output is a view choice, not an architecture choice:** primary path lowers
straight into the DDS (Grazel-native, no BUILD on disk, agent-queryable); a
`razel export build` can materialize `BUILD.bazel` for Bazel compat / diffing,
Gazelle-style. Same facts, two renderings.

### 2.2 Manifest → BUILD target mapping (per ecosystem)

| Ecosystem | source of truth | first-party targets (Pass B) | external deps (Pass A) | tests |
|---|---|---|---|---|
| **Cargo** | `[package]`/`[lib]`/`[[bin]]`, `Cargo.lock`, `src/` convention | `rust_library` (src/lib.rs), `rust_binary` per `[[bin]]`/`src/bin/*`, `crate-type` → cdylib/staticlib | `crate_universe`-style: lock → `@crates//<name>` | `rust_test` per `#[cfg(test)]` + `tests/*.rs` integration |
| **PyPI/pyproject** | `[project]`, `[tool.hatch/setuptools…]`, `src/` layout | `py_library` per importable subpkg (or one per top pkg), entry-points → `py_binary` | `[project.dependencies]` + lock → `@pip//<dist>` | `py_test` per `tests/test_*.py` (or per dir) |
| **npm** | `package.json` (`main`/`exports`, `bin`, `workspaces`), lock | `js_library`/`ts_project` per package; `bin` → `js_binary` | `dependencies`+lock → `@npm//<pkg>`; `peerDependencies` → provided | `*_test` per test runner config (jest/vitest) |
| **Go** | `go.mod` + package dirs + import scan | `go_library` per dir, `go_binary` for `package main` | `require` + `go.sum` → `@gazelle`-style repos | `go_test` per `*_test.go` (native Gazelle) |
| **CMake** | `CMakeLists.txt` — **best machine source = File API codemodel JSON**, not the text | wrap-first: one `foreign_cc`-style opaque target; OR consume codemodel `add_library`/`add_executable` → cc targets | `find_package` → unresolved; map by hand or to system/`@repo` | `add_test`/CTest → `cc_test` if codemodel consumed |

### 2.3 Per-ecosystem gotchas (the part that actually bites)

**Cargo**
- **`build.rs`** = arbitrary code that emits sources/flags. Opaque to inference;
  must be declared or run inside a sandboxed action. (astichi's `native_engine`
  is clean here, but general repos aren't.)
- **Feature unification** is workspace-global in Cargo but per-target in Bazel —
  the classic mismatch. `select()` only approximates it.
- **proc-macros** build for the host/exec platform, not target — needs the
  exec-config seam (`GrazelForecast.md` D6/D12).
- `dev-dependencies` are test-only; `[workspace]` vs package root; target-cfg
  (`[target.'cfg(...)'.dependencies]`) → `select()`.

**PyPI / pyproject**
- **import-name ≠ distribution-name** (`import yaml` ← `PyYAML`). This is the
  whole reason `gazelle_python_manifest` exists; we need the same dist↔module
  index. Critical for Pass-B edge resolution.
- **Build-backend zoo** (hatchling / setuptools / poetry / pdm / flit) — parse
  PEP 621 `[project]` first; backend-specific tables are secondary.
- **Dynamic metadata / custom hooks** — **astichi has `hatch_build.py`** (a
  custom wheel build hook) and `[tool.hatch.build.targets.wheel.hooks.custom]`.
  This is the hard boundary: it can synthesize files at build time. Same class as
  `build.rs` — declare its inputs/outputs or wrap it; do not pretend to infer it.
- Native extensions cross ecosystems (astichi's PyO3 `.so`) — see §2.4.
- `[project.optional-dependencies]` (extras) → config/`select()`, like features.

**npm**
- **Workspace links**: `file:../grip-core` and `workspace:*` are *first-party*
  edges → internal Labels, not registry fetches (the grip case).
- **`peerDependencies`** (grip-react→react, grip-vue→vue) are provided-by-
  consumer, not owned — emit as `provided`/visibility, not a hard dep.
- Lockfile dialect differs (npm vs pnpm vs yarn); pnpm's is the most structured.
- **Phantom deps** / hoisting: a file can import a package not in its own
  `package.json`. Scan must reconcile against the declared set.
- ESM/CJS dual + TS project references → target granularity decisions.

**Go** — the easy one. go.mod + import scan is deterministic (Gazelle proves it).
Watch: build tags, cgo (re-enters C/C++), generated code, `internal/`
visibility (maps *nicely* to Bazel visibility).

**CMake** — the outlier; do not try to parse the text.
- Configure is **Turing-complete** (generator expressions, `find_package`,
  conditionals) — no stable target graph from the file itself.
- Best machine-readable source is the **CMake File API codemodel JSON** (run
  configure once, read the JSON), or `cmake --graphviz`. Even then `find_package`
  resolution is environment-dependent.
- **Plan: wrap-first** (`rules_foreign_cc`-style opaque action over the native
  build), and *optionally* consume codemodel JSON later for real cc targets.
  Don't block the other four ecosystems on this.

### 2.4 Inter-package dependency resolution (the `grip-react → grip-core` question)

This is the payoff. Mechanism:

1. **Build a workspace name→target index** across all adapters: every produced
   target registers the names it satisfies — crate name (`astichi_native_engine`),
   distribution + import names (`astichi`), npm package (`@owebeeone/grip-core`),
   Go import path.
2. **Resolve each declared dep edge** against that index:
   - name in index → **internal `DepEdge`** to that Label
     (`@owebeeone/grip-react`'s `@owebeeone/grip-core` → `//grip-core:lib`).
   - not in index → **external `@repo` ref** (Pass A).
   - `file:` / `workspace:` / path deps → internal Labels directly, no lookup.
3. **Cross-language edges** fall out of the same index: astichi's Python wheel
   declares a runtime need for the native module → resolves to the
   `astichi_native_engine` cdylib target's output. One index, language-agnostic.

Worked: grip resolves to a pure DAG —
`//grip-core:lib ← //grip-react:lib ← //grip-react-demo:app`, plus
`//grip-core:lib ← //grip-vue:lib`; react/vue are external `peer` refs, not edges.

**Gotchas on the edges:**
- **Single-version-per-workspace.** Bazel/Razel is one version of a name per
  workspace; npm `^0.2.0` ranges and nested `node_modules` versions collapse to
  the one workspace version. Fine for a monorepo; **flag loudly** if two packages
  pin incompatible majors.
- **Mono vs multi-repo.** Today grip is sibling checkouts. Cross-repo edges ride
  the **`@repo` → local-checkout** seam (`GrazelForecast.md` D7) — resolve a
  `@grip_core//...` external to a path-mapped local dir, no fetch.
- **Provenance** must survive resolution: every edge records the manifest line it
  came from, so "why does grip-react depend on grip-core?" is answerable in the
  DDS (this is the F17/F21 agent-query payoff).

### 2.5 Sequencing & validation

Order by *determinism × value × already-in-tree*, not by ecosystem popularity:

1. **Cargo + pyproject first** — because `astichi` exercises *both plus the
   cross-language seam and a custom build hook* in one repo. It is the ideal
   first fixture: hardest realistic case, already on disk.
2. **npm second** — the grip DAG is the clean first-party-edge fixture
   (`file:` links, peer-deps, multi-package).
3. **Go** — cheap, do when a Go target appears (Gazelle algorithm is known-good).
4. **CMake** — wrap-only first; codemodel-JSON ingestion is a later, optional arc.

**Validation = conformance, the Razel way.** The generated graph is correct iff
it reproduces each manifest's *own* build/test outcome. For astichi the anchors
already exist: `uv run pytest` (the `tests/` suite) and the wheel's native-import
smoke test (`from astichi.lower_engine.native import load_native_extension`).
A generated `py_test`/`rust_test` graph that reproduces those *is* the proof —
no separate oracle needed.

### 2.6 Cross-cutting gotchas (one list)

- **Build scripts / codegen hooks are the hard wall** — `build.rs`,
  `hatch_build.py` custom hooks, npm `postinstall`, CMake configure. You cannot
  infer their I/O. Policy: **declare inputs/outputs or sandbox-wrap; never guess.**
- **Config/feature/extras unification** — Cargo features, py extras, npm optional
  deps, CMake options all want `select()` and all mismatch global-vs-per-target.
- **Name ≠ name** — import-name vs distribution-name (Python), package-name vs
  crate-name (Rust), scope vs dir (npm). The name→target index must carry *all*
  aliases or edges silently misresolve.
- **Lockfile dialects** — one parser per (ecosystem × tool); pin which lock is
  authoritative.
- **Test granularity** — per-file vs per-dir test targets is a policy knob
  (`SELF.razel` default in Model G); pick one, make it overridable.

---

## Bottom line

- **Q1:** Yes — heavily, but as a *fragmented composition* of per-language tools
  (Gazelle + extensions for first-party; `crate_universe`/`pip_parse`/
  `npm_translate_lock` for third-party), with **Pants** as the inference-first
  sibling closest to Grazel. **No** single cross-ecosystem autogen exists, and
  **CMake has no faithful translator** — everyone wraps it.
- **Q2:** Add a family of **manifest adapters** behind the existing
  manifest-registry seam, each lowering `manifest + layout + scan` into DDS EDB
  facts via two passes (lock→external, scan→first-party). Resolve inter-package
  edges through one workspace-wide **name→target index** (handles the
  `grip-react → grip-core` and Python-wheel → Rust-cdylib cases uniformly).
  Start on `astichi` (Cargo + pyproject + cross-language + custom hook = hardest
  realistic case, already on disk), then the grip npm DAG; defer Go (easy) and
  CMake (wrap-first). This is `GrazelForecast.md` Gc5/D8 made concrete — **no new
  core commitment, just new loaders.**

---

## Sources (prior art)

- [bazel-contrib/bazel-gazelle](https://github.com/bazel-contrib/bazel-gazelle) — the canonical BUILD generator + extension framework
- [rules_python/gazelle](https://pkg.go.dev/github.com/bazelbuild/rules_python/gazelle) — Python BUILD generation
- [Calsign/gazelle_rust](https://github.com/Calsign/gazelle_rust) — Rust first-party generation (scans `use` paths)
- [google/cargo-raze](https://github.com/google/cargo-raze) — Cargo→BUILD (archived; migrate to `crate_universe` in rules_rust)
- [Scaling Rust workspaces with Bazel — Tweag](https://www.tweag.io/blog/2023-07-27-building-rust-workspace-with-bazel/) — crate_universe in practice
- [Dependency inference: Pants's special sauce](https://www.pantsbuild.org/blog/2022/10/27/why-dependency-inference) — the inference-first alternative (closest to Grazel)
- [Pants vs Bazel](https://www.pantsbuild.org/blog/2021/11/18/pants-vs-bazel) — autogen/inference vs hand-written metadata
- [Bazel module extensions (EngFlow)](https://blog.engflow.com/2025/10/14/writing-bazel-rules-module-extensions/) — how Maven/Cargo/NPM integrate into the module system
