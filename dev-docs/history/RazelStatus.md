# Razel Status — Bazel BUILD-file feature compatibility

**Goal:** evaluate and build **unmodified Bazel `BUILD`/`.bzl` files** by resolving
`load()` to native rule implementations (no Bazel runtime, no `cc_common`), for the
self-contained slice of the ecosystem (no external-repo fetching). This doc tracks
what's needed, current status, completeness, and a parallelizable ordering.

Legend: ✅ done · 🟡 partial · ❌ missing/not started. *Completeness* = rough % of
the Bazel-faithful behavior razel implements for that feature.

---

## 0. One-line summary

razel parses BUILD/.bzl, resolves `load()` to native cc/rust/py/sh rule families,
evaluates custom `rule()`s (inline **and** loaded `.bzl`, post freeze-fix), runs a
content-addressed, sandboxed, incremental build, and accepts Bazel-syntax CLI flags.
It builds the real Bazel C++ tutorial (3 stages) and the simplest bazelbuild-examples
custom rules (`empty`, `actions_write`) green. The frontier is the **analysis engine**
(provider flow, schema-driven deps, two-phase) and **external repos** (not started).

---

## 1. Feature matrix

### A. Loading & Starlark evaluation
| Feature | Status | % | Works | Missing / broken |
|---|---|---|---|---|
| BUILD/.bzl parse + eval (starlark-rust) | ✅ | 95 | full Starlark, `Dialect::Extended` | — |
| stdlib globals (print/map/filter/debug/json/partial) | ✅ | 80 | the common set wired | `pprint`/`pstr`/typing/record/enum/`set` not enabled |
| macros (plain `def` wrapping rules) | ✅ | 90 | inline + loaded `.bzl` | — |
| `load()` → project `//pkg:f.bzl` / `:f.bzl` | ✅ | 85 | recursive, frozen-module cache | repo-relative `@//` forms |
| `load()` → `@rules_cc` / `@rules_python` / `@rules_rust` / `@rules_shell` | ✅ | 70 | ruleset registry → native rules | only the common rule names |
| `load()` → `@bazel_skylib` | 🟡 | 40 | rules (`bzl_library`/`build_test`/flags/`expand_template`/`copy_file`) as no-ops | `lib` helpers (`selects`/`paths`/`sets`) not provided |
| `load()` → `@local_config_*` (cuda/rocm) | 🟡 | 30 | `if_<x>_is_configured` → not-configured | other generated symbols |
| `load()` → arbitrary `@repo//` (absl/xla/tsl/llvm/…) | ❌ | 0 | — | no repo fetch, no path-map — **hard wall** |

### B. Native BUILD builtins
| Feature | Status | % | Works | Missing / broken |
|---|---|---|---|---|
| `glob(include, exclude)` | ✅ | 80 | recursive, package-relative | `allow_empty`, symlink edge cases |
| `select({...})` | 🟡 | 30 | picks `//conditions:default` else first | no real `config_setting` matching |
| `package`/`package_group`/`licenses`/`exports_files` | ✅ | 60 | recognized no-ops | visibility/license not enforced |
| `alias`/`config_setting`/`test_suite` | 🟡 | 40 | recorded as placeholder targets | `alias` doesn't forward; `config_setting` not matched |
| `filegroup` | 🟡 | 60 | forwards `srcs` as outputs | no transitive data |
| `genrule` (`cmd`, `outs`, `$(location)`, make-vars) | ❌ | 0 | — | not implemented |

### C. Bazel Starlark API (for real `.bzl`)
| Feature | Status | % | Works | Missing / broken |
|---|---|---|---|---|
| `Label("//pkg:name")` | 🟡 | 50 | `.package/.name/.workspace_root/.workspace_name` (main repo) | external-repo labels, methods |
| `native.*` namespace | 🟡 | 25 | `package_name`/`repository_name`/`glob` | `existing_rules`, native rule calls, `package_relative_label` |
| `attr.*` schema | 🟡 | 20 | namespace present (string/int/bool/label/label_list/output/…) | **descriptors are ignored** — no defaults, no typing, no implicit deps |
| `provider()` + instances + `dep[Info]` | ❌ | 0 | — | **the AE keystone** (cross-target provider flow) |
| `depset` | ✅ | 70 | `depset(direct, transitive=[…]).to_list()`, deduped; `DefaultInfo(files=)` accepts | no `order` semantics; members stored as paths not values |
| `ctx.actions.run/write/declare_file/args` | 🟡 | 60 | run/write (real), declare_file, `args().add/add_all` | `ctx.actions.expand_template`, `run_shell`, param files |
| `ctx.outputs` / `ctx.files` / `ctx.executable` | 🟡 | 40 | outputs (string attrs→Files), files (list attrs), executable (stub) | schema-driven; executable resolution; `ctx.outputs` from `attr.output` only |
| `File` value (`.path/.short_path/.basename/…`) | ✅ | 70 | the common fields | `root`, `owner`, `is_directory` |
| `ctx.attr.<dep>` as Target (`.label`, `dep[Info]`) | ❌ | 0 | deps surfaced as `struct(files=)` only | Target objects carrying providers |

### D. Native rule families
| Feature | Status | % | Works | Missing / broken |
|---|---|---|---|---|
| `cc_library` / `cc_binary` | ✅ | 70 | compile/archive/link, hdrs+deps+copts/defines/includes (transitive 1-hop) | `cc_test`, transitive >1-hop deps, toolchain resolution, `linkstatic`, PIC |
| `rust_library` / `rust_binary` | ✅ | 50 | rustc compile/link, `--extern` direct deps | transitive crates, edition propagation, proc-macros, `rust_test` |
| `py_library` / `py_binary` / `py_test` | ✅ | 55 | launcher + PYTHONPATH, transitive srcs | zip/par packaging, `imports`, runfiles tree |
| `sh_binary` / `sh_test` | ✅ | 50 | script-as-executable | `data` runfiles, deps wiring |
| custom `rule()` | 🟡 | 40 | inline **+ loaded `.bzl`** (freezable), ctx.actions, DefaultInfo | schema-driven deps, provider flow, two-phase, `ctx.executable` |

### E. Dependency / analysis model
| Feature | Status | % | Works | Missing / broken |
|---|---|---|---|---|
| `deps` kwarg → dep graph | ✅ | 70 | deps-first execution; cross-package on-demand load | only the `deps` attr (not arbitrary label attrs) |
| transitive provider propagation | 🟡 | 30 | cc hdrs/cflags 1-hop | general N-hop providers, `depset`-backed |
| schema-driven implicit deps (label attrs → deps) | ❌ | 0 | — | needed for `ctx.attr/files/executable` |
| **two-phase** load-then-analyze (forward refs) | ❌ | 0 | eval-order analysis only | forward references fail (e.g. `actions_run`) |
| `config_setting` + real `select` resolution | ❌ | 5 | default branch only | flag/platform matching, `--define`/`--//flag` |
| visibility / `package_group` enforcement | ❌ | 0 | parsed, ignored | — |
| toolchain resolution / platforms / transitions | ❌ | 0 | host pinned | `--platforms`, `cfg="exec"`, `find_*_toolchain` |

### F. Execution & build
| Feature | Status | % | Works | Missing / broken |
|---|---|---|---|---|
| content-addressed action cache (blake3) | ✅ | 90 | hit→0-exec, `Cached`/`Built` | GC, remote |
| sandbox + declared-input enforcement (F12) | ✅ | 80 | symlink/hardlink, seatbelt (macOS) | Linux namespaces, Windows job objects |
| incremental (Skyframe-lite, early-cutoff) | ✅ | 75 | engine-backed | finer-grained invalidation |
| **parallel scheduler** (`-j`/`--jobs`) | ❌ | 0 | sequential | concurrent action execution |
| OS adapter (unix/windows transport, materialize) | ✅ | 70 | builds + runs on macOS/Linux/Windows | full Windows test fixtures |

### G. External repos
| Feature | Status | % | Works | Missing / broken |
|---|---|---|---|---|
| native ruleset shims (`@rules_cc`/skylib/config) | 🟡 | 40 | the common ones | breadth |
| `@repo` → local-path mapping (no fetch) | ❌ | 0 | — | the cheap way past the wall (point `@absl` at a checkout) |
| repo fetching (`bazel_dep`/`http_archive`) | ❌ | 0 | — | full package-manager scope |

### H. CLI (Bazel-compatible)
| Feature | Status | % | Works | Missing / broken |
|---|---|---|---|---|
| Bazel-syntax flag parser (data-driven, 1033-flag inventory) | ✅ | 85 | `=`/space/`--no`/short/`--`, diagnose-or-silent, drift-gate | — |
| honored build flags (`-c`/`--copt`/`--cxxopt`/`--linkopt`/`--define`/`--disk_cache`) | ✅ | 70 | wired to engine | per-target `copts` precedence nuances |
| target patterns | 🟡 | 30 | single `//pkg:name` or bare name | `//pkg:all`, `//...`, multi-target |
| `-k/--keep_going` | ❌ | 0 | bails on first failure | failure accumulation |
| `query` / `affected` | 🟡 | 40 | `affected` (rdeps) | `bazel query` expr language |
| daemon / `subscribe` (BEP-like) | ✅ | 60 | warm analysis, streaming | flags not passed over the wire |

### I. Wire / infra (supporting)
| Feature | Status | % | Works | Missing / broken |
|---|---|---|---|---|
| taut IR → generated types + CBOR runtime (v0.4.0) | ✅ | 90 | codegen + runtime + corpus drift-gate | services not generated |
| conformance / golden corpus | ✅ | 80 | cross-language byte parity | broader vectors |

---

## 2. Success factors (definition of "done" per pillar)

- **Custom rule()**: a loaded `.bzl` rule with `attr.label_list` deps, returning a
  custom `provider`, consumed via `dep[Info]`, builds green (e.g. `rules/depsets`).
- **Native cc/rust/py/sh**: N-hop transitive deps + `select` + `cc_test` build green.
- **CLI**: `razel build //...` builds all targets, `-k` accumulates, `-j` parallelizes.
- **External**: `@repo`-mapped real repo (e.g. abseil via local checkout) builds green.
- **Ecosystem proof**: a real self-contained Bazel repo (no megaproject deps) builds.

---

## 3. Ordering (phased roadmap)

Each phase is a coherent capability; later phases depend on earlier ones.

- **P1 — AE core: provider flow.** `provider()` + instances + `dep[Info]`, backed by a
  `'v`-scoped target→providers store (`eval.extra`). *Unblocks*: providers, `depsets`
  end-to-end. **Prereq for everything in D/C.**
- **P2 — ctx Targets + schema-driven deps.** `attr.*` descriptors carry kind; `rule()`
  captures schema; label/label_list attrs become deps; `ctx.attr.<x>` = Target objects,
  `ctx.files`/`ctx.executable` resolved. *Unblocks*: `attributes`, executable deps.
- **P3 — two-phase analysis.** Load-then-analyze (defer impls via `eval.extra`),
  topological deps-first. *Unblocks*: forward refs (`actions_run`), robust graphs.
- **P4 — `config_setting` + real `select`.** Flag/platform matching, `--define`/`--//`.
- **P5 — `genrule` + make-vars + `$(location)`.** The last common BUILD builtin.
- **P6 — parallel scheduler (`-j`).** Concurrent action execution; `-k` accumulation.
- **P7 — target patterns** `//pkg:all` / `//...` + multi-target.
- **P8 — `@repo` local-path mapping.** Point external repos at checkouts (no fetch).
- **P9 — toolchains / platforms / transitions.** (Large; needed for real diversity.)
- **NEVER (by design):** `cc_common`/native analysis runtime, full repo fetching.

---

## 4. Parallel plan (what can fan out)

Dependency DAG of the tracks (→ = "must precede"):

```
            ┌─────────────── P1 provider flow ───────────────┐
            │  (eval.extra 'v-scoped target→providers store)  │
            └───────┬──────────────────────┬──────────────────┘
                    ▼                       ▼
            P2 ctx Targets +         (P1 alone unblocks
            schema-driven deps        provider/depset examples)
                    ▼
            P3 two-phase analysis ──► actions_run, robust graphs

   Independent tracks (no AE dependency — parallelizable NOW):
     T-a  P5 genrule + make-vars + $(location)
     T-b  P6 parallel scheduler (-j) + -k accumulation
     T-c  P7 target patterns //pkg:all, //...
     T-d  P8 @repo local-path mapping
     T-e  P4 config_setting + real select   (touches loading; light coupling)
     T-f  native-rule depth: cc_test, N-hop transitive, rust/py runfiles
```

**Parallelizable immediately (independent files/areas, fan-out friendly):**
- **T-a genrule**, **T-b scheduler**, **T-c patterns**, **T-d repo-mapping** are each
  largely disjoint (razel-loading rules vs razel-engine/exec vs razel-cli vs loader).
  These mirror the earlier rust/py/sh fan-out: one agent per track, own module(s) +
  tests, small shared-seam merges.
- **Caveat — the AE spine (P1→P2→P3) is sequential**, and all three touch the same
  hot file (`razel-loading/src/rules.rs`): the `ctx`/`rule()`/invoke flow. Fanning
  these out would collide. Do the spine in one focused track; fan out T-a…T-f around it.

**Recommended shape:** 1 sequential "AE spine" track (P1→P2→P3) + up to ~4 parallel
agents on T-a/T-b/T-c/T-d (worktree-isolated, disjoint crates). T-e/T-f as capacity allows.

---

## 5. Honest caveats

- Completeness %s are behavioral estimates, not test coverage.
- "Green" everywhere means **builds + runs**, but at **single-hop deps, host toolchain,
  no select matching** — the depth columns (transitive, toolchains, select) are the gap.
- The two hard walls remain **by design**: the Bazel native analysis runtime
  (`cc_common`/`depset`-as-providers internals/toolchain resolution) and external-repo
  *fetching*. razel's thesis is to shim/native around them, not reimplement them — so
  full TensorFlow-class builds are explicitly **out of scope**; self-contained real
  repos are the target.

---

*Generated from session state through commit `f6a8dc8` (razel-ae/depset). Update as
phases land.*
