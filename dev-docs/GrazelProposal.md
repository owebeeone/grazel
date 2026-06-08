# Grazel Proposal - Model G for Razel

Status: proposal / clean-slate product surface

This document follows `ThoughtExp-CleanSlateBuildSurface.md`. It assumes the
Razel engine still answers to `ArchFundamentals.md` F1-F23, but it treats the
user-facing product as something broader than a Bazel-compatible command and a
BUILD-file parser.

The proposal is deliberately not "BUILD files, but smaller." The core claim is:

> Razel SHOULD expose a typed canonical build contract. Humans and agents SHOULD
> edit sparse declarative descriptors close to the source tree. Razel MUST lower
> those descriptors, deterministic inference, and compatibility adapters into the
> same canonical contract before analysis and execution.

Call this model **Grazel**: graph-contract Razel, or Model G.

---

## 1. Product thesis

Bazel's source of truth is usually a package-local BUILD file plus `.bzl` rule
logic. That surface scales better than Make, but it still tends to create large
maintenance files: every target grouping, exception, dependency, generated file,
language convention, config branch, and local policy lands in the same place.

In an agent-assisted workflow, that is not ideal. An agent editing `foo.cc`
should not usually need to patch a 900-line BUILD file in the same change. It
should be able to ask:

- What build facts are known about `foo.cc`?
- Which target owns it?
- Which provider contract does it require?
- Why does it compile with this flag?
- What descriptor must change to add a local exception?
- What tests are affected if this dependency changes?

Grazel's answer is a sparse, provenance-rich build surface:

```text
pkg/
  SELF.razel          # package intent, defaults, grouping, visibility
  foo.cc
  foo.cc.razel        # only facts/exceptions for foo.cc
  bar.cc
  include/foo.h
```

The important rule is that these are **descriptors**, not programs. They declare
facts and intent. Razel composes them into a normalized contract.

---

## 2. Design goals

**G-G1 - Locality.** File-specific build metadata SHOULD live beside the file
when that metadata is genuinely local to the file.

**G-G2 - Sparse defaults.** Common package policy SHOULD be declared once in
`SELF.razel`, and common workspace policy SHOULD be declared once at the
workspace layer.

**G-G3 - Canonical contract.** The engine MUST consume a typed, normalized build
contract, not ad hoc descriptor files.

**G-G4 - Deterministic inference.** Razel MAY infer obvious facts, but inferred
facts MUST be deterministic, provenance-carrying, explainable, and reproducible
from declared inputs.

**G-G5 - Agent transactions.** MCP SHOULD expose schema, graph, provenance,
validation, and transactional edit operations. MCP MUST NOT become the build
language or hidden source of truth.

**G-G6 - Compatibility without contamination.** Bazel BUILD/`.bzl` support
SHOULD remain a front-end adapter that lowers into the same contract. The core
contract MUST NOT absorb Bazel-specific packaging or Starlark assumptions.

**G-G7 - No scattered imperative logic.** Sidecars and package descriptors MUST
NOT perform arbitrary I/O, run loops, mutate global package state, or define
procedural target generators.

---

## 3. Non-goals

- Grazel is not a proposal to delete Bazel compatibility.
- Grazel is not a proposal to make MCP the source of truth.
- Grazel is not an AI-only inferred build system.
- Grazel is not a general scripting language with a `.razel` extension.
- Grazel is not a single workspace manifest that centralizes all build policy.

---

## 4. The coherent model

The pieces only work if they have distinct authority:

```text
source tree
  + sparse descriptors
  + rule-pack schemas
  + deterministic inference
  + compatibility adapters
      -> composed fact set
      -> canonical build contract
      -> configured target graph
      -> action graph
      -> execution / query / MCP views
```

The authored inputs are descriptors and source files. The canonical build
contract is the engine input. MCP is an interaction layer over both; it can
propose and apply descriptor edits, but it cannot create hidden build state.

### 4.1 Authored descriptor layers

Grazel has three authored descriptor scopes.

1. **Workspace descriptor**

   Declares workspace-wide defaults: rule packs, language profiles, dependency
   policy, repository mappings, organization-wide visibility defaults, and named
   build profiles.

2. **Directory descriptor: `SELF.razel`**

   Declares package intent: default language settings, target grouping, exported
   headers, package visibility, ownership, and package-local policy.

3. **Source sidecar: `<source>.razel`**

   Declares facts and exceptions for one source-like subject: file role,
   special compiler args, special deps, generated-source metadata, exclusions,
   and local provider requirements.

The sidecar subject is implied by the filename. A `foo.cc.razel` file SHOULD
describe `foo.cc`; it SHOULD NOT define unrelated targets.

### 4.2 Deterministic inference

Inference is a compiler pass, not an oracle. It MAY derive facts such as:

- `foo.cc` is a C++ implementation source.
- `foo.h` is a C++ header.
- `*_test.cc` is probably a test source.
- `#include "pkg/foo.h"` suggests a dependency on a provider exporting that header.
- A package default says `*.cc` belongs to target `core`.

Inference MUST record provenance. A user or agent must be able to ask why a fact
exists and get a precise answer: descriptor line, rule-pack inference pass, input
file, and config dimension.

Inference MUST NOT be nondeterministic. If it scans a directory, that directory
listing is an analysis input. If it parses imports, those source files are
analysis inputs. This preserves F1, F2, F4, and F10.

### 4.3 Canonical build contract

The canonical build contract is the normalized graph-facing model. It is not
intended to be pleasant hand-authored syntax. It MUST contain at least:

- Packages and package identity.
- Source units and source roles.
- Targets and configured target identity.
- Typed dependency edges.
- Provider contracts produced and required.
- Declared inputs and outputs.
- Toolchain and platform constraints.
- Configuration dimensions and selected values.
- Action declarations after rule lowering.
- Provenance for every nontrivial fact.

This contract is the handoff between product surfaces and engine fundamentals.
Bazel BUILD files, `.razel` descriptors, inference, and MCP views all meet here.

The contract MAY be persisted as a generated snapshot for debugging, review, or
remote cache coordination. If persisted and committed, it MUST be checked by CI
against regeneration. It SHOULD NOT become the normal hand-edited source.

---

## 5. Descriptor shape

The exact syntax is an open decision. The examples below are illustrative. The
semantics are more important than the spelling.

### 5.1 `SELF.razel`

```razel
package {
  visibility = ["//app/..."]
  owner = "team-core"
}

cxx.defaults {
  standard = "c++20"
  warnings = "strict"
}

target "core" {
  kind = "cxx.library"
  sources = files("*.cc", except = ["*_test.cc"])
  public_headers = files("include/**/*.h")
}

target "core_test" {
  kind = "cxx.test"
  sources = files("*_test.cc")
  deps = [":core"]
}
```

This file says what the package is trying to publish. It does not list every
file unless listing every file is itself meaningful.

### 5.2 Source sidecar

```razel
# foo.cc.razel
source {
  role = "implementation"
  cxx.copts += ["-Wno-unused-parameter"]
  requires += [provider("cxx.headers", from = "//libs/compat")]
}
```

The sidecar only patches facts about `foo.cc`. It does not create a new target
or mutate some unrelated package default.

### 5.3 Workspace policy

```razel
workspace {
  rule_packs = [
    "razel:cxx@1",
    "razel:python@1"
  ]
}

profile "linux-release" {
  platform = "linux-x86_64"
  mode = "opt"
}

dependency_policy {
  external_resolution = "locked"
}
```

Workspace policy is where organization-level defaults belong. It prevents every
package from spelling the same warning mode, sanitizer mode, platform set, or
external dependency rule.

---

## 6. Composition rules

Sparse files only stay maintainable if composition is rigorous.

### 6.1 Fact keys

Every descriptor statement lowers to one or more typed facts:

```text
Fact {
  subject: Source("//pkg:foo.cc") | Package("//pkg") | Target("//pkg:core")
  field:   "cxx.copts" | "visibility" | "sources" | ...
  value:   typed value
  source:  provenance record
}
```

### 6.2 Merge classes

Fields MUST declare their merge behavior in schema:

- **Scalar.** One assignment is allowed. Conflicting assignments are errors.
- **Set.** Values merge by deterministic set union.
- **Ordered list.** Values merge only through explicit append/prepend operations.
- **Overrideable scalar.** A narrower scope may override a broader default only
  with an explicit `override` operation.
- **Derived.** Inference may propose a value, but authored facts may refine or
  reject it.

There MUST NOT be a general "last file wins" rule. That would make sparse
metadata order-sensitive and difficult to explain.

### 6.3 Precedence

The normal precedence stack is:

```text
rule-pack defaults
  < workspace descriptor
  < SELF.razel package descriptor
  < source sidecar
  < transaction-local proposed edit
```

Inference does not simply sit "above" or "below" authored facts. It produces
derived facts with provenance and schema-defined merge behavior. If inference and
authored facts disagree, the result MUST be either a deterministic refinement or
a validation error.

### 6.4 Conflicts

Conflicts are product features, not parser failures. Razel SHOULD report:

- the subject,
- the conflicting field,
- both values,
- both provenance records,
- the schema rule that made them incompatible,
- suggested edits when the fix is mechanical.

This is essential for agent workflows. An agent should be able to ask Razel how
to make the patch valid instead of guessing at descriptor semantics.

---

## 7. Configuration and conditionals

Grazel still needs configuration. It should not recreate `select` as stringly
typed map keys.

Configuration SHOULD be a typed dimension model:

```razel
when profile.mode == "debug" {
  cxx.copts += ["-DDEBUG"]
}

when platform.os == "linux" {
  cxx.linkopts += ["-pthread"]
}
```

The expression language, if any, MUST be:

- total,
- side-effect-free,
- bounded,
- typed against known config dimensions,
- unable to inspect arbitrary filesystem state,
- unable to create new targets procedurally.

This gives descriptors enough expressiveness for real variation without making
them a general interpreter.

---

## 8. Rules and extension

Grazel still needs user-extensible build logic. The extension point is rule-pack
schema plus deterministic lowering, not arbitrary descriptor execution.

A rule pack MAY contribute:

- descriptor schemas,
- provider schemas,
- target kinds,
- validation rules,
- deterministic inference passes,
- lowering from canonical contract facts to actions,
- MCP schema/help surfaces.

A rule pack MUST NOT require engine changes for normal language/tool support.
This preserves F15 and F16.

Rule packs are where real build expertise lives. Descriptors say "this package
publishes a C++ library"; the C++ rule pack knows how to compile, archive, link,
publish `CxxInfo`, and validate header visibility.

---

## 9. MCP endpoint

Razel SHOULD have an MCP endpoint in this model, but MCP is a protocol surface,
not the build surface.

The endpoint SHOULD expose operations like:

- `schema.describe`: list descriptor schemas, fields, merge classes, and examples.
- `graph.resolve`: lower descriptors and inference into the canonical contract.
- `graph.query`: deps, rdeps, owners, affected targets, affected tests.
- `graph.explain`: explain why a target, edge, action, flag, or provider exists.
- `provenance.get`: map contract facts back to descriptor lines and inference passes.
- `transaction.propose`: submit descriptor edits without applying them.
- `transaction.validate`: return graph diff, diagnostics, and affected work.
- `transaction.apply`: apply validated edits atomically.
- `build.run` / `test.run`: execute requested targets with streaming events.

Every MCP mutation MUST correspond to a reviewable file edit or an explicit
generated snapshot update. There must be no MCP-only state.

This makes the endpoint useful for agents without giving agents a privileged,
nondeterministic route around the build contract.

---

## 10. Bazel compatibility

BUILD/`.bzl` support remains valuable, but in Grazel it is just one adapter:

```text
Bazel BUILD/.bzl -> Bazel front-end -> canonical build contract
Grazel descriptors -> descriptor front-end -> canonical build contract
MCP transaction -> descriptor edit -> descriptor front-end -> canonical build contract
```

A directory SHOULD have one primary package surface at a time:

- `BUILD` for Bazel-compatible mode.
- `SELF.razel` for Grazel-native mode.

Mixed mode MAY be allowed later, but it MUST have explicit conflict rules. The
initial design SHOULD avoid a directory where BUILD and `SELF.razel` both define
the same target authority.

This avoids contaminating the clean contract with Bazel's directory-package and
Starlark semantics while still allowing Razel to consume Bazel repositories.

---

## 11. Why the pieces work together

The model is coherent because each layer answers a different question:

| Layer | Question answered | Authority |
|---|---|---|
| Source tree | What files exist? | Authored project input |
| Sidecar | What is special about this file? | Authored local fact |
| `SELF.razel` | What does this package publish? | Authored package intent |
| Workspace descriptor | What policy applies everywhere? | Authored workspace policy |
| Inference | What facts follow mechanically? | Derived fact with provenance |
| Canonical contract | What graph does the engine see? | Normalized engine input |
| MCP | How do tools inspect and change this safely? | Transaction/query protocol |
| Bazel adapter | How do existing BUILD repos enter the model? | Compatibility projection |

The dangerous alternative is mixing these authorities. If sidecars can define
package-level generators, they become scattered BUILD files. If MCP can mutate
hidden state, the build stops being reproducible. If inference cannot explain
itself, the product becomes magical. If the canonical contract is absent, every
surface has to reimplement graph semantics.

---

## 12. Walkthroughs

### 12.1 Adding a normal C++ file

1. User adds `bar.cc`.
2. `SELF.razel` target `core` includes `files("*.cc", except = ["*_test.cc"])`.
3. Inference classifies `bar.cc` as a C++ implementation source.
4. The canonical contract records `bar.cc` as an input to `//pkg:core`.
5. `graph.explain //pkg:core --source bar.cc` reports the `SELF.razel` pattern and
   inference pass.

No BUILD file edit is needed.

### 12.2 Adding a file-specific compiler exception

1. A warning is intentionally suppressed only for `foo.cc`.
2. Agent asks MCP for the descriptor schema for C++ source facts.
3. Agent proposes `foo.cc.razel` with `cxx.copts += ["-Wno-unused-parameter"]`.
4. Razel validates the transaction, shows the graph diff, and reports affected
   targets/tests.
5. The applied change is a small sidecar next to the source.

No package descriptor churn is needed.

### 12.3 Resolving an inferred dependency

1. Inference sees `#include "compat/foo.h"` in `foo.cc`.
2. Two providers could satisfy that header.
3. Razel emits an ambiguity diagnostic with both candidate providers.
4. Agent proposes a sidecar `requires` fact for `foo.cc`.
5. The canonical contract now has an explicit typed dependency edge.

Inference helped discover the missing fact, but the final graph is explicit.

### 12.4 Moving a file

1. `foo.cc` moves to `pkg2/foo.cc`.
2. Its sidecar moves with it.
3. The old package loses the source through its `SELF.razel` pattern.
4. The new package either accepts it through an existing pattern or reports that
   no target owns it.

The local exception does not remain buried in a stale package-level file.

---

## 13. Risks and controls

**Risk: descriptor fragmentation.**

Control: sidecars may only describe their subject. Package-level intent stays in
`SELF.razel`. Razel MUST provide "show all facts for target/source" and "show all
sidecars affecting package" queries.

**Risk: inference feels magical.**

Control: inferred facts always carry provenance. Ambiguous inference becomes a
diagnostic or proposed edit, not silent graph mutation.

**Risk: `SELF.razel` becomes the new god file.**

Control: keep it declarative and package-scoped. Encourage patterns and defaults
instead of long file lists. Move file-specific exceptions to sidecars.

**Risk: expression language grows into Starlark.**

Control: expressions are typed, total, bounded, and config-only. Rule-pack
lowering is the extension point for real logic.

**Risk: agents spray sidecars everywhere.**

Control: MCP validation SHOULD prefer the narrowest correct edit, but it SHOULD
also suggest package-level changes when multiple sidecars repeat the same fact.

**Risk: compatibility and native modes conflict.**

Control: initial design SHOULD require one primary package surface per directory.
Mixed mode is an explicit future decision.

---

## 14. Requirement and test hooks

These are proposal requirements, not current implementation commitments.

| ID | Requirement | Test hook |
|---|---|---|
| G-R1 | Descriptor parsing MUST produce typed facts with provenance. | G-T1 parse `SELF.razel` and `foo.cc.razel`; assert fact set and source spans. |
| G-R2 | Composition MUST be deterministic and schema-driven. | G-T2 compose workspace, package, sidecar, and inference facts in varied file order; assert identical contract. |
| G-R3 | Conflicting scalar facts MUST fail with both provenance records. | G-T3 create conflicting visibility defaults; assert diagnostic fields. |
| G-R4 | Inference MUST use declared scan inputs and record provenance. | G-T4 infer source role and include dependency; assert input set and explanation. |
| G-R5 | Sidecars MUST NOT define unrelated targets. | G-T5 parse invalid `foo.cc.razel` that mutates `bar.cc`; assert validation error. |
| G-R6 | MCP mutations MUST apply as file transactions. | G-T6 propose sidecar edit; validate graph diff; apply; assert file diff and no hidden state. |
| G-R7 | Bazel and Grazel front-ends MUST lower to the same contract shape. | G-T7 lower equivalent BUILD and `SELF.razel` packages; compare normalized contract fields. |
| G-R8 | Explainability MUST map target facts back to descriptors or inference. | G-T8 query why `foo.cc` has a compiler flag; assert sidecar span. |

---

## 15. Open decisions

1. **Descriptor syntax.** Use a Starlark-like declarative subset, TOML/YAML, YIDL,
   or a generated schema syntax?
2. **Snapshot policy.** Should the canonical contract be persisted only for debug,
   or should some repos commit a lock/snapshot for review and CI drift checks?
3. **Inference default.** Which inference passes are enabled by default, and which
   require explicit package opt-in?
4. **Mixed package mode.** Can BUILD and `SELF.razel` coexist in one directory,
   and if so, which surface owns target authority?
5. **Sidecar density.** When should repeated sidecar facts be promoted into
   `SELF.razel` defaults, and should Razel diagnose over-fragmentation?
6. **Provider resolution.** How much provider selection may be inferred before a
   user-authored fact is required?
7. **Rule-pack trust.** What is the sandbox and versioning model for contributed
   inference and lowering plugins?

---

## 16. Bottom line

Grazel is strongest if it is understood as one product model, not several ideas:

- sparse descriptors provide locality,
- deterministic inference removes noise,
- schema-driven composition prevents fragmentation,
- the canonical contract preserves build-system rigor,
- MCP gives agents a safe transactional interface,
- Bazel compatibility remains an adapter, not the architectural center.

The build graph is still explicit after lowering. The authored surface is simply
less monolithic and more aligned with how humans and agents actually make changes.
