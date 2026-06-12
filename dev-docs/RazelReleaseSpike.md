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

## §3b The bzlmod world — the not-locked-out doctrine

**The compatibility relation is deliberately ASYMMETRIC and one-way:** razel consumes
Bazel's world fully; Bazel never needs to know razel exists; and razel-native features
must never make a project unbuildable by Bazel (this is interop, not embrace-extend —
decision: Gianni 2026-06-12). "Not locked out" is the third leg: as the ecosystem moves
to MODULE-only (WORKSPACE off by default in 8, REMOVED in 9), razel must keep a
consumption path for every layer of the new world. The surface, itemized:

1. **The module graph.** Every dep ships its own MODULE.bazel; Bazel fetches descriptors
   transitively from a REGISTRY, runs MVS (one version per module), and gives each module
   its own repo mapping. razel's path: bzlmod resolution is a NEW FRONT-END to the
   EXISTING pipeline — descriptors are fetched files, MVS is a pure function, and the
   output is the same repo-spec lockfile the WORKSPACE extractor emits, feeding the same
   R2/R3 fetcher/materializer. The architecture already insures this: resolution and
   materialization are decoupled at the lockfile seam.
2. **The registry (BCR).** Plain HTTPS: JSON metadata + source-archive pointers + patch
   files — no Bazel binary anywhere in the protocol. No lockout vector; razel consumes it
   directly. MODULE.razel may declare registry OVERRIDES (mirrors, a future
   iroh-distributed registry/cache — a razel-native feature that costs Bazel nothing).
3. **Repo mapping / canonical names.** bzlmod's apparent-vs-canonical split
   (`@foo` → `@@foo+1.2.3`, per-module mappings) is a semantic razel must ADOPT, not
   approximate — label identity, error messages, and `dep[P]` provider identity all key
   on it. This is engine work (the labels layer), flagged now so it lands with the bzlmod
   front-end rather than as a retrofit.
4. **Module extensions** (pip, crate_universe, go_deps…) are the hard 20%: arbitrary
   Starlark over repository_ctx. Strategy: FAITHFUL PER-ECOSYSTEM PIPELINES verified
   against bazel ground truth (@pypi done; Cargo.lock next — §1.4), because they cover
   the corpus that matters; the GENERIC repository_ctx executor remains the L7 endgame
   and the ultimate lockout insurance for the long tail.
5. **MODULE.bazel.lock.** Bazel's bzlmod lockfile; razel reads it when present (it pins
   the resolution razel would otherwise compute) and razel's own lockfile stays the
   sibling artifact (`razel-lock.json`, already in the workspace root per the round-36
   placement decision).
6. **Boundary discovery.** The walk-up rule (nearest MODULE.bazel / REPO.bazel /
   WORKSPACE[.bazel]) + `.bazelignore` + nested-boundary skipping in package discovery —
   all spike-tier items (§1.2 adjacency); razel currently takes roots explicitly.

The MODULE.razel position in this world: applied AFTER bzlmod resolution as the delta —
version/registry overrides, razel-only modules (glade, gryth rulepacks), host-stub
postures. A MODULE.razel-bearing project still resolves identically under Bazel, which
never reads it.

## §3c BUILD.razel — the competing designs (decision pending)

The package level is where the delta-file pattern gets DANGEROUS, because BUILD files are
not just content — they are PACKAGE BOUNDARY MARKERS, and the two engines must agree on
package structure or label resolution itself diverges (subpackage shadowing, glob
boundaries). Four candidate designs, with their failure modes:

- **(A) Full overlay** — BUILD.razel can add targets AND override attrs of
  Bazel-declared targets. Maximum power; silently forks the graph the moment an override
  lands (the two engines build different things from the same tree while both "work").
  Rejected as default posture: it is the embrace-extend shape from the inside.
- **(B) Additive-only sibling — REJECTED (Gianni's scope argument, 2026-06-12).** The
  kill: references must flow one way (a `.bazel` file may NEVER see a `.razel`-declared
  target, or the project stops being Bazel-buildable), which forces razel-only targets
  into a SECOND SCOPE — and then `//x/y:all`, `/...`, `test_suite` expansion and `query`
  either mean different graphs per engine (the same label expression, engine-dependent)
  or need new label syntax to address the second scope (forking the deepest shared
  contract there is). Either way it is design A with extra steps.
- **(C) No BUILD.razel: in-band loaded rules** — razel-native features are ordinary
  Starlark rules `load()`ed in the SHARED BUILD file from a razel-provided module whose
  BAZEL-side implementation degrades gracefully (no-op or genuinely portable impls) — a
  `razel_compat` module, publishable to the BCR. One source of truth per package; the
  whole BUILD-tooling ecosystem (gazelle, buildozer, buildifier, IDEs) sees everything;
  the Bazel build keeps working because the loaded shim makes razel-isms legal Bazel.
- **(D) No package-level mechanism at all** — razel deltas stop at module level;
  third-party BUILD adjustments use the EXISTING repo-patch machinery (MODULE.razel
  overrides + patch files — the same mechanism Bazel itself uses for its own deps).

**DECISION: C + D; A and B rejected — there is no BUILD.razel.** C's clinching property
is namespace CONGRUENCE: shim-loaded razel-native targets EXIST under Bazel (as degraded
no-ops), so `:all`/`/...`/query/test_suite enumerate the SAME target set in both engines.
Two obligations on the compat shim follow: (1) Bazel-side implementations must be CHEAPLY
BUILDABLE no-ops — `:all` stays green under Bazel, not merely parseable; (2) razel-native
targets carry a standard `razel-only` tag so Bazel users can exclude them with stock
`--build_tag_filters` — filtering inside the contract, never a second scope. First-party
cases always have C (you own the BUILD); third-party trees have D (repo patches). The
escape-hatch role B was reserved for is covered by D.

## §4 Acceptance

- Examples tiers 1–2 GREEN as goldens (graph + output parity vs bazel-7.7.0) and wired
  into the probe sentinel set.
- `razel build//run/test` work on cpp-tutorial + a rust binary/test golden.
- A gryth-shaped rust crate with crates.io deps builds and tests via razel.
- TF sweep ≥ 455/835 throughout (the no-regress floor).
