# RazelReleaseSpike — the gryth-unlock pivot (goldens-first)

*2026-06-12. Decision (Gianni): pivot — not abandon — from TF coverage to a usable-subset
release, because the real goal is unblocked development of the iroh/razel-powered gryth
server (rust + ts stack). TF stays the depth verifier; the `third-party/examples` tree
(bazelbuild/examples) is the LOW BAR and the new goldens corpus. "If I can have a limping
along razel, I can make progress on that path which does not depend on a full TF capable
builder."*

## §1 The needs ladder (Gianni's list, re-graded against gryth)

1. **Linking** — cc AND rust executables (the registered `c++-link-executable`
   action_config + File-ification; rust binary path beside the existing lib golden).
   Highest perceived-value-per-effort on the board.
2. **The parallel action executor + `--jobs`/`-j`** — execution-phase parallelism in
   razel-build (the flag parses today; the work is the scheduling loop). The
   content-digest action-cache key (registered) lands here too. NOTE the mapping of
   record: `--jobs` = execution actions; the loading pool = `--loading_phase_threads`.
3. **Runfiles → `razel test`** — runfiles is the hidden structural item (registered ❌):
   test runners, gazelle's runner rule, sh_test all stage through it. Then cc_test/rust
   test MVP (build + run + exit code + test.log shape).
4. **Cargo.lock crate fetching** — the actual gryth unlock (iroh ⇒ crates.io deps): the
   @pypi pipeline re-aimed — lockfile carries sha256s, resolve crates.io URLs, extract,
   generate BUILDs (crate_universe's shapes as ground truth, same byte-parity method).
5. **java + gazelle** — demoted to examples-tier (nothing in gryth is java; gazelle-core
   emits go). They ride the low-bar ladder: java-tutorial/java-maven and go-tutorial
   rungs, in that order, after 1–4.

## §2 Goldens-first (the examples corpus)

Small examples, easy to verify — exactly the parity-harness shape razel already has
(`razel-parity` normalization + `xtask capture-goldens` over `bazel aquery`, Phase-0
vintage). The method per example workspace:

1. **Capture**: real bazel-7.7.0 (tools/) — `aquery` for graph goldens, plus OUTPUT
   goldens for run-capable rungs (binary stdout, archive contents) — committed beside the
   example reference.
2. **Verify**: razel analyzes/builds the same workspace; graph diff via the normalizer,
   outputs byte-compared. Each rung of §1 flips one tier of examples from red to green,
   and the tier STAYS green (probe sentinels grow accordingly).
3. The TF sweep remains the depth gauge — run at banks, never regress the 455 floor.

Tiering of `third-party/examples`: `cpp-tutorial` stage1–3 + `rules/*` (analysis) first;
`query-quickstart` + `flags-parsing-tutorial` with the CLI verbs; `java-*`, `go-tutorial`
per §1.5; `bzlmod`, `frontend`, `android` out of scope for the spike.

## §3 Workspace files: MODULE.razel and .razelrc (the definition)

**The principle: a razel workspace MUST remain a valid Bazel workspace, and vice versa.**
razel consumes Bazel's files with Bazel's semantics, then applies a razel-only DELTA from
sibling files Bazel never reads. Two file pairs:

| Bazel file | razel delta file | applied |
|---|---|---|
| `.bazelrc` | `.razelrc` | after — overrides/extends |
| `MODULE.bazel` (/ WORKSPACE chain) | `MODULE.razel` | after — overrides/extends |

**Why separate files instead of razel-isms in Bazel's files:** Bazel HARD-ERRORS on
unknown flags in its rc and unknown directives in MODULE.bazel — putting razel-only
content there would break the Bazel build. The sibling-file split means: a pure-Bazel
project runs under razel with ZERO razel files (full mimicry); adopting razel features is
additive opt-in; and the project keeps building under Bazel throughout, because Bazel
ignores `.razel*` entirely. (Same layering rationale as the cache decision: Bazel-identical
below the top level, razel-owned at the seam.)

**What belongs in `.razelrc`:** engine knobs with no Bazel equivalent
(`--loading_phase_threads` defaults, fetched-external-base policy, output-root overrides,
future iroh/central-cache endpoints); overrides of `.bazelrc` lines razel must treat
differently; razel-only flags Bazel would reject. Same syntax as `.bazelrc` (command
prefixes, `--config`, `import`), same precedence ladder (system → workspace → home →
`--razelrc=`), evaluated AFTER the corresponding `.bazelrc` layer.

**What belongs in `MODULE.razel`:** dependency-layer deltas — repo overrides (pin a dep to
a vendored/fetched copy, declare a host-stub posture for a generated repo), razel rulepack
/ extension declarations as the system grows past mimicry (the glade/derivation layer,
fmt/lint capability registrations — RazelGaps' Pants-model item), and razel-native module
metadata. Syntax: Starlark, MODULE.bazel-shaped directives plus `razel_*`-prefixed ones.
razel reads MODULE.bazel first (Bazel semantics), then MODULE.razel mutates the resolution.

**Status:** `.bazelrc` parsing is the long-registered RazelGaps item (now with three live
consumers: `--enable_workspace`, `--deleted_packages`, `--loading_phase_threads`); this
section PROMOTES it into the spike (§1.2 wires it). MODULE.razel needs no implementation
until a razel-only dependency delta exists — the first real consumer is likely gryth
pinning its crate/grip deps; define-on-first-use, but the file name, placement, and
precedence are decided HERE.

## §4 Acceptance

- Examples tiers 1–2 GREEN as goldens (graph + output parity vs bazel-7.7.0) and wired
  into the probe sentinel set.
- `razel build//run/test` work on cpp-tutorial + a rust binary/test golden.
- A gryth-shaped rust crate with crates.io deps builds and tests via razel.
- TF sweep ≥ 455/835 throughout (the no-regress floor).
