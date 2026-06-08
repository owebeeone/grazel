# Bazel Input-Surface Constraints (the fixed input "language")

Companion to `ArchFundamentals.md`. Where the Fundamentals (F1–F23) are what *any*
build tool must **do** — architecture- and language-agnostic — this doc is what razel
must **accept**: the fixed input surface imposed by the goal *"consume unmodified Bazel
`BUILD`/`.bzl` files."*

**These are constraints, not choices.** razel does not get to design them; compatibility
fixes them. They don't make the tool *good* (that's the Fundamentals); they make it
*compatible*. They are also, in principle, **replaceable** — a different build tool could
accept a wholly different surface (see `ThoughtExp-CleanSlateBuildSurface.md`). That
replaceability is the key architectural fact: **the constraints concentrate at the
front-end; the Fundamentals govern the engine; the boundary between them — the lowering
to an internal representation — is razel's most important seam.**

> Architectural consequence: design the internal representation (the IR the engine
> consumes) to be **surface-agnostic**, so the Bazel surface is *a* front-end, not *the*
> tool. razel already has the start of this seam (`AnalyzedTarget` → `wire_to_ir` →
> `razel-ir::Graph`); the constraints below stop at that lowering line.

Current razel coverage per item lives in `RazelStatus.md`; this doc states *what is
fixed*, not how complete we are.

---

## C1. The description language (syntax + evaluation semantics)
- **Starlark**, in Bazel's `BUILD`/`.bzl` dialect: the grammar, the type set, and the
  evaluation rules. Fixed down to behavior razel can't alter.
- **`.bzl` loading**: `load("<label>", "sym", …)` resolution and ordering; modules are
  evaluated once and their symbols frozen/shared.
- **Macros** (plain `def`s that call rules) and **rules** (`rule()`), evaluated in the
  package's scope.
- Constraint on the front-end: razel must *embed an evaluator with these exact
  semantics* — not a lookalike. (Mechanism is the architecture's choice; the *semantics*
  are fixed.)

## C2. Naming & packaging
- **Labels**: `//pkg:name`, `:name`, `//pkg/sub:name`, `@repo//pkg:name`; the package =
  directory-with-a-`BUILD` model; one target namespace per package.
- **Visibility** + `package_group` semantics (even if enforcement is deferred, the
  *vocabulary* must parse).
- Path/label canonicalization rules (what `:x` vs `//p:x` mean in context).

## C3. Native BUILD builtins (always-available, no `load`)
A fixed set the description may use directly: `glob`, `select`, `package`,
`package_group`, `licenses`, `exports_files`, `filegroup`, `genrule`, `alias`,
`config_setting`, `test_suite`, `environment`, `existing_rules`, … Each has fixed
argument shapes and semantics razel must honor (e.g. `glob` include/exclude patterns,
`genrule`'s `cmd`/`outs`/`$(location)`/make-variables).

## C4. The rule-authoring API (the Bazel Starlark API)
The surface `.bzl` files call to *define* rules — fixed names and contracts:
- `rule(implementation, attrs, …)`, the **`attr.*`** type constructors, **`ctx`** (`attr`,
  `file(s)`, `executable`, `outputs`, `label`, `actions.run/run_shell/write/...`, `args()`),
- **`provider()`** + provider instances + `target[Provider]` indexing,
- **`depset`** (members + `transitive` + traversal order), **`Label`**, **`native.*`**,
- **`aspect()`**, **`select`**/`config_setting` matching, **transitions**, toolchain APIs.
- Constraint: real `.bzl` (incl. `@rules_*`, `@bazel_skylib`) is written against these;
  to evaluate them razel must provide these names with faithful behavior.

## C5. The ruleset `load()` surface (the names BUILDs import)
- `@rules_cc//cc:*.bzl` (`cc_library`/`cc_binary`/`cc_test`), `@rules_python//…`,
  `@rules_rust//…`, `@rules_shell//…`, `@bazel_skylib//…`, `@local_config_*//…`, etc.
- Constraint: BUILDs `load()` these by exact path+symbol; razel must resolve them (to
  native shims and/or by evaluating real `.bzl`). The *names and their rule semantics*
  are fixed; the engine ships none of them itself (cf. Fundamental F15).

## C6. Per-language build conventions / heuristics
Each language's rules encode a fixed compile/link model razel must reproduce:
- **cc**: `srcs`/`hdrs`/`deps`/`copts`/`defines`/`includes`; `#include` resolution
  (quote/system, workspace-relative `-iquote`); compile→archive→link; toolchain flag shape.
- **rust**: crate model, `--extern <name>=<rlib>`, editions, `crate_root`.
- **py**: source collection, `PYTHONPATH`/import roots, launcher/runfiles, `main`.
- **sh**: script-as-executable + `data`/runfiles.
- Constraint: these are the *meaning* of "build a `cc_library`" etc., fixed by Bazel's
  rules; razel's rule bodies (native or evaluated) must match outputs/semantics, not just
  accept the attributes.

## C7. Configuration, platforms, toolchains
- `config_setting` + `select({...})` matching against flags/`--define`/`--//flag`,
  `constraint_setting`/`constraint_value`/`platform`, toolchain types and resolution,
  configuration transitions (`cfg = "exec"`/custom). Fixed vocabulary + matching rules.

## C8. External-dependency declaration
- `MODULE.bazel` / `bazel_dep` / module extensions (and legacy `WORKSPACE`): the fixed
  way a project names its external repos and versions. Constraint: razel must read these
  to know what `@repo//` resolves to.

## C9. The command-line surface
- The `bazel <command> [flags] <targets>` grammar, target patterns (`//pkg:all`,
  `//...`), and the flag set. Already inventoried + parsed (`razel-cli` + the generated
  flag table; see `RazelStatus.md` §H). Fixed: razel must accept what Bazel accepts.

---

## How this constrains the architecture

- **The front-end is large and fixed.** C1–C9 dictate a substantial *loading + analysis*
  layer whose job is to faithfully evaluate the Bazel surface and **lower** it to the
  internal representation. The candidate architectures differ mostly in *how that
  front-end is structured* (the Dialect debate) — but all must produce the same lowered
  graph.
- **The lowering boundary is the seam.** Everything below it (the IR + engine + execution)
  answers to the **Fundamentals**, not to Bazel; it should not know what `cc_library`
  means, only what an action/target/provider is. Keeping that boundary clean is what makes
  C-surface evolution (or replacement) possible without touching the engine.
- **Two satisfaction targets, one architecture.** A candidate architecture must satisfy
  **both** `ArchFundamentals.md` (engine correctness/perf/extensibility) **and** these
  constraints (front-end fidelity). They pull on different parts of the design; the
  architecture proposals must show both are met.

*This is the fixed input surface. The Fundamentals are timeless; these constraints are
Bazel's, and recorded as such so the engine never absorbs them.*
