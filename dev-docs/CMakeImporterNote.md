# CMake importer — configure-as-tracked-node

*Companion to `RazelPlan.md` (§ importers / D6) and `BazelRustFeasStudy.md`. Records the
finding from the ingestion spike: how razel should ingest CMake projects, and why the
generated IR stays fresh like a native BUILD instead of going stale like a one-shot export.
Spike artifacts live in `cmake-ingest-spike/`; CMake source (for the File API contract +
fixtures) is cloned shallow at `cmake/`.*

---

## 1. Finding: run it, don't parse it

CMake's listfile language is imperative (variables, `if()`, `macro()`, `include()`, generator
expressions). Static parsing of `CMakeLists.txt` means reimplementing CMake's evaluator — dead
end. The parser exists in the clone (`cmake/Source/cmListFileLexer.*`, `cmListFileCache.*`) if
ever wanted, but the importer should instead **run the configure step and read the resolved
graph** via CMake's **File API** (stable, documented contract: `cmake/Help/manual/cmake-file-api.7.rst`).

Drop a query file, run `cmake -S <src> -B <build>`, and CMake emits per-target JSON with
sources / defines / includes / link-edges **already resolved** — conditionals taken, genexps
evaluated, dep edges baked. This is the path `RazelPlan.md:136` already locked
("CMake-file-api, post-resolution, flags baked, lands at the ActionNode layer").

**Proof (`cmake-ingest-spike/`):** a sample project built to defeat static parsing — a
variable-assembled source list, an `if(SAMPLE_FAST)`, and a `$<...>` genexp. The File API
resolved all of it (`mathlib` → `add.c`+`fast.c`, `defines=[SAMPLE_FAST=1]`, `app`→`:mathlib`),
and `cmake_to_build.py` translated the reply into `cc_library`/`cc_binary`. Full pipeline:
`CMakeLists.txt → cmake configure → File API JSON → BUILD`.

## 2. The refinement: configure is a dirty-tracked node, not a transpile

The objection to a File API import is that it's a **per-configuration snapshot**, not a portable
`select()`-bearing BUILD. The resolution: don't make it portable — make it **incremental**. The
configure step becomes a first-class cached node in razel's engine:

- **inputs** = the file set CMake read during configure
- **output** = the resolved IR (the action subgraph)
- **invalidation** = the input fingerprint

Edit `CMakeLists.txt` → configure-node dirties → IR re-derives → downstream actions rebuild.
Identical behaviour to a native BUILD whose analysis result is cached on the BUILD file's hash.

This is **not** bolt-on bookkeeping: CMake already computes its own re-configure trigger set and
no-ops configure when nothing changed. The importer just consumes that set into razel's engine
instead of into Ninja/Make. CMake exposes it two independent ways (both verified in the spike):

1. **File API `cmakeFiles` object** — every file read, tagged `PROJECT-INPUT` / `isExternal` / `isGenerated`.
2. **`CMAKE_MAKEFILE_DEPENDS`** (in `<build>/CMakeFiles/Makefile.cmake`) — CMake's own re-configure trigger list.

## 3. Input classification — and why "per-config" is the right granularity

| Input bucket (from the `cmakeFiles` object) | Dependency kind | Handling |
|---|---|---|
| `CMakeLists.txt` (PROJECT-INPUT) | in-repo source | fingerprint → retrigger configure |
| `…/Modules/*.cmake` (isExternal, isCMake) | toolchain files | fingerprint, or fold into a "cmake version" key |
| `CMakeCCompiler.cmake`, `CMakeSystem.cmake` (isGenerated) | **cached probe results** | CMake snapshots system state *into a file* — the "non-file" dep becomes a file dep |
| `CMakeCache.txt` | the `-D` option set | this is the **config key** |

The last row dissolves the snapshot objection. A snapshot isn't a defect — it's a node **keyed by
`CMakeCache.txt`**. Each configuration (Debug/Release, platform, option set) → distinct cache →
distinct configure-node → distinct IR fragment, all coexisting in the engine. That is exactly
Bazel's configured-target model (one target under N configurations), not something to engineer away.

## 4. Honest residuals (neither novel — both are CMake's existing footguns)

- **`file(GLOB)` without `CONFIGURE_DEPENDS`** — the one genuine correctness gap. CMake evaluates
  the glob at configure time and does *not* list the directory as a trigger, so a *new* source
  file won't dirty the node. Identical to Bazel's `glob()` hazard; solved the same way: watch the
  globbed dir, or always run a (cheap, no-op) configure on any in-tree change.
- **Out-of-band system state** — e.g. installing a lib that `find_package` would now resolve. Only
  invalidates if CMake re-probes. Re-running configure closes it; not re-running is the staleness
  CMake users already live with.

Cost: a configure is seconds, not free — but incremental (CMake no-ops most of it) and paid only
when a declared input changes. Same economics as Bazel re-reading a BUILD on change.

## 5. Where this lands in the plan

Refines `RazelPlan.md:136`. The plan says the importer "lands directly at the ActionNode layer."
More precisely: a **configure node sits *above* the action subgraph**, and that node is itself
dirty-tracked. So CMake import is not a one-shot transpile that rots — it's a **live projection**
maintained by the same incremental engine as everything else. That is the D6 thesis ("the engine
is the product; importers produce the IR") landing where it should.

## 6. Pointers / corpus

- File API contract: `cmake/Help/manual/cmake-file-api.7.rst` (codemodel-v2, cmakeFiles-v1).
- Ingestion fixtures shipped in the clone: ~1687 `CMakeLists.txt` + ~8060 `.cmake` under `cmake/Tests/`
  — a conformance corpus, the same way the plan uses bazel/buck2 suites as anchors.
- Spike: `cmake-ingest-spike/sample/` (project), `cmake_to_build.py` (translator), `build/.cmake/api/v1/` (queries+reply).

## 7. Next (not yet built)

- Wire the invalidation loop: fingerprint the input set from the `cmakeFiles` object, re-run
  configure on change, re-derive IR. (Spike currently proves run→BUILD, not yet the dirty loop.)
- Exercise against a real project (custom commands, generated files, `find_package`) — pull one
  from `cmake/Tests/` so it hits the parts the toy sample doesn't.
