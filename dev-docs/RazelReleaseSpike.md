# RazelReleaseSpike — the gryth-unlock pivot (goldens-first)

*2026-06-12. Decision (Gianni): pivot — not abandon — from TF coverage to a usable-subset
release, because the real goal is unblocked development of the iroh/razel-powered gryth
server (ts/npm stack — rust is RAZEL'S implementation language, not gryth's). TF stays the depth verifier; the `third-party/examples` tree
(bazelbuild/examples) is the LOW BAR and the new goldens corpus. "If I can have a limping
along razel, I can make progress on that path which does not depend on a full TF capable
builder."*

## §1 Two tracks (re-laddered 2026-06-12 — the goal is GRYTH UNLOCKED, verified honest)

**The decoupling mechanism that lets the tracks run simultaneously:** every bazel-compat
claim is verified by `--strict_bazel` goldens (§3d) and the TF sweep floor (≥455) — a
gryth-track engine change that bends Bazel semantics surfaces as a strict-mode diff at the
next bank, not as drift. The mode matrix (§3c) + the C3c gate partition the code surfaces;
the honest risk to simultaneity is BANDWIDTH (the previous "parallel lane" never
materialized) — default cadence is interleaved-by-round, gryth track leads.

**Track G — gryth unlock (leads). Corrected 2026-06-12: gryth is TS/NPM** (rust is what
razel itself is written in — cargo-built, self-hosting is not a goal here). Consequence:
razel-native js/ts rules will be SIMPLE razel shapes, not aspect-rules_js mimicry — which
makes E-mode the day-1 grammar for gryth (its BUILD files use razel-native rules, so
bazel-grammar start would need shims for no benefit; sovereignty is the honest mode).

- **G1. E-mode core** — BUILD.razel/MODULE.razel filename + boundary-walk support; XOR
  rule; guards + strict-mode interplay can trail. Cheap (same evaluator, new filenames) —
  and now load-bearing for everything after it.
- **G2. npm lockfile pipeline** — the @pypi method's THIRD application: package-lock.json
  / pnpm-lock.yaml carry integrity hashes; fetch tarballs from registry.npmjs.org,
  verify, materialize node_modules (razel-native layout first; aspect-rules_js shapes are
  a track-B concern if ever needed). Acceptance: gryth-ui's dep tree materializes.
- **G3. js/ts rules + `razel run`** — razel-native `ts_project`-lite (tsc action),
  `js_binary` (node runner); `razel run` lands here (the dev loop is build-run-test).
  Acceptance: a gryth hello server runs via razel.
- **G4. `razel test` (vitest/jest exec) + the parallel executor/`--jobs`** — js test
  runners exec standalone (no runfiles prerequisite — that chain stays track B); the
  executor is shared infra scheduled here.
- **G5 (later).** Glade/derivation targets, iroh-distributed cache/registry, content-key
  action cache for dev-loop incrementality. Rust-ecosystem support (rules_rust binaries,
  Cargo.lock pipeline) moves to track B's ladder — it serves bazel-compat corpora, not
  gryth.

**Track B — bazel compat (interleaves; the examples tree is its scoreboard):**
- **B1. cc linking** (`c++-link-executable` action_config + File-ification) +
  cpp-tutorial stage1–3 goldens — the strict-mode goldens HARNESS bring-up rides here.
- **B2. bazelrc parsing + `--strict_bazel` plumbing + walk-up root discovery +
  `.bazelignore`** — the CLI verbs (`build`/`run`/`test`/`query`) become real here, serving
  BOTH tracks.
- **B3. Runfiles → cc_test/sh_test; `query` verb** (query-quickstart golden).
- **B4. java tier (java-tutorial/java-maven), then go/gazelle tier (go-tutorial).**

Shared acceptance: TF sweep ≥455 at every bank (both tracks); examples goldens
strict-green per B-rung; gryth-repo-builds per G-rung.
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

**(E) Razel-native segregation (Gianni, 2026-06-12 — the "fully independent" mode):**
  BUILD.razel EXISTS, but only as a package's SOLE grammar — `BUILD.razel` XOR
  `BUILD[.bazel]`, coexistence is an ERROR. Razel-native targets live in packages Bazel
  provably cannot see (no shared labels to fork — the property B lacked). Three rules
  make it sound: (1) mutual exclusion per package; (2) the BOUNDARY GUARD — razel-native
  packages live either in razel-native MODULES (no Bazel grammar anywhere; MODULE.razel
  joins razel's boundary walk-up) or under `.bazelignore`d subtrees of dual workspaces
  (Bazel formally blind ⇒ no glob/boundary divergence; razel CHECKS this), and (3) the
  dependency arrow — razel-native modules may depend on bazel-grammar ones, never the
  reverse (structurally enforced by 1+2, not just by policy). This is gryth's mode: the
  server is razel-native (glade/derivation targets first-class, no shim ceremony), atop
  bazel-grammar deps.

**DECISION: C + D + E; A and B rejected.** The package-mode matrix: pure-bazel (mimicry) ·
dual-augmented (C: compat shim, congruent namespace, `razel-only` tags) · razel-native
(E: BUILD.razel sole grammar, boundary-guarded) · third-party (D: repo patches). C's clinching property
is namespace CONGRUENCE: shim-loaded razel-native targets EXIST under Bazel (as degraded
no-ops), so `:all`/`/...`/query/test_suite enumerate the SAME target set in both engines.
Two obligations on the compat shim follow: (1) Bazel-side implementations must be CHEAPLY
BUILDABLE no-ops — `:all` stays green under Bazel, not merely parseable; (2) razel-native
targets carry a standard `razel-only` tag so Bazel users can exclude them with stock
`--build_tag_filters` — filtering inside the contract, never a second scope. First-party
cases always have C (you own the BUILD); third-party trees have D (repo patches). The
escape-hatch role B was reserved for is covered by D.

## §3d `--strict_bazel` mode (the parity oracle switch)

**Definition (Gianni, 2026-06-12): under `--strict_bazel` razel behaves as if it WERE
Bazel** — the razel delta layer is switched off wholesale so golden comparisons against
real Bazel are apples-to-apples:

- `.razelrc` and `MODULE.razel`: not read.
- Razel-native packages (BUILD.razel, mode E): invisible — exactly Bazel's view (not
  packages at all; the boundary-guard `.bazelignore` congruence makes this safe).
- Compat-shim rules (mode C): evaluate their BAZEL-side degraded semantics, not their
  razel semantics — strict mode's contract is "produce what Bazel would produce,"
  graph- and byte-comparable.
- Razel-only flags: rejected (as Bazel would reject them).

**Uses:** the goldens harness ALWAYS diffs in strict mode (a delta is a razel BUG by
definition); CI parity gates; and a user-facing "would Bazel agree?" check. Spelling:
`--strict_bazel` (Bazel flag convention), `--strict-bazel` accepted as alias. Plumbing:
a GlobalFlags mode consulted at the delta-layer seams (rc loading, module resolution,
package discovery, shim dispatch) — lands with §1.2's bazelrc work.

## §4 Acceptance

- Examples tiers 1–2 GREEN as goldens (graph + output parity vs bazel-7.7.0) and wired
  into the probe sentinel set.
- `razel build//run/test` work on cpp-tutorial + a rust binary/test golden.
- A gryth-shaped rust crate with crates.io deps builds and tests via razel.
- TF sweep ≥ 455/835 throughout (the no-regress floor).
