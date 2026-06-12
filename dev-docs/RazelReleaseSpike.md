# RazelReleaseSpike (V3sh1) — the gryth-unlock side hustle (goldens-first)

**Designation: V3sh1 — "side hustle 1" of `RazelV3Plan.md`, SUBORDINATE to it, superseding
nothing.** V3's invariants bind here unchanged (gates incl. the C3c ratchet, TDD,
roll-build, the doc map); this doc REPRIORITIZES within them. In particular the js/ts
rules are a V3 §4 LANGUAGE TRACK (`rules_shims_js.bzl` over the generic API — the
java_defs.bzl pattern; engine gaps the track finds become core tickets) — NOT a new native
module; the "no rules_js.rs ever" ratchet holds. V3Plan §4 carries the one-line amendment
(track candidate order: js/ts first, for gryth; go later).

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
- `razel build`/`run`/`test` work on cpp-tutorial + a js/ts binary+test golden.
- A gryth-shaped TS/NPM server (razel-native module, npm-lock deps) builds, runs and
  tests via razel.
- TF sweep ≥ 455/835 throughout (the no-regress floor).

## §5 The step-by-step plan (exit conditions and all)

Process invariants for every step: roll-build (each lands green: `cargo test --workspace`
+ gates + sentinels), TF sweep ≥455 at every bank, goldens always diffed in
`--strict_bazel`, commit + tag `razelV3/<step>`, round delta to the checkpoint-4 précis,
debts to RazelGaps. Track G leads; B interleaves where marked.
The test regime governing all of this is `RazelPublicSurfaces.md §6` (the T0–T5 pyramid):
T2 (engine battery) guards S1 onward; T0/T1 (wire goldens + service-contract transcripts)
land WITH the S3 server skeleton — no skeleton without its harness; T3 (strict-mode
examples goldens) lands with S4; T4 is the continuous TF floor; T5 (gryth acceptance
end-to-end) closes the gryth-bootstrap bar.

**S0 (G0) — PLAN 0: the grazel seam (mechanical crate restructure, zero semantics).**
*Added 2026-06-12 (Gianni): "unleash the gryth work" starts here — the grazel binary
exists from day one so gryth-dev pins its shape, and every later step lands in final
form.* Scope (PublicSurfaces §1d made real; cheaper than §1d assumed — razel-daemon is
ALREADY a lib, and razel-cli is a 750-line main over the bazel_flags lib):
- `razel-cli` gains a `[lib]` target: verb dispatch, flag parsing, daemon dialing,
  output rendering move into the library; `main.rs` becomes the thin `razel` bin.
- New `grazel` bin crate linking the SAME razel-cli lib: razel's CLI surface verbatim
  (shared parser — identical behavior by construction) plus the grazel-namespaced verb
  stub (`grazel node status` → "not implemented" is enough; the binary and namespace
  are the deliverable, not the node).
- The DENY RULE lands in CI: no `razel-*` crate may depend on a `grazel-*` crate or on
  iroh (dep-graph gate in xtask, same class as razel-loading's runtime-free rule).
Exit: both bins build; a fixture test asserts `grazel build`/`query` output is
byte-identical to `razel`'s (tested, not assumed from the shared lib); the deny gate is
RED-TESTED (a deliberate violation fails); suite + TF floor green.

**S1 (G1) — E-mode core.**
Scope: boundary walk-up (`MODULE.razel` joins `MODULE.bazel`/`REPO.bazel`/`WORKSPACE[.bazel]`);
`BUILD.razel` in package discovery; the XOR rule (coexistence with `BUILD[.bazel]` = loud
error); a `strict_bazel` GlobalFlags bool that makes E-packages invisible (full rc wiring
waits for S6); `.bazelignore`-guard check as a warning DURING S1 ONLY — it becomes a hard ERROR at S3
(before real gryth adoption; review fix — boundary divergence must not be warnable in
production use).
Exit: fixture workspace (MODULE.razel root, BUILD.razel packages) loads and analyzes;
coexistence errors; the same fixture under strict mode shows ZERO razel packages; suite +
TF floor green.

**S2 (G2) — npm lockfile pipeline.**
Scope: `fetch-npm`: parse the lock gryth actually uses (package-lock v3 lists literal
`node_modules/...` paths — materialization follows the lock verbatim; pnpm-lock decision
made HERE by looking at gryth-dev, not guessed); integrity hashes are sha512 → cache under
`cache/repos/v1/content_addressable/sha512/…` (same schema, new digest dir); registry
tarballs fetched, verified, extracted to the locked node_modules layout.
NON-GOALS (kept brutally narrow — review fix): NO lifecycle scripts (postinstall never
runs), NO native-addon builds (gyp), NO workspace/monorepo features beyond what gryth's
own lock uses, NO registry auth. Each is a named hole until a real consumer demands it.
Exit: tiny-lockfile fixture materializes; integrity mismatch fails loud; offline re-run =
100% cache hits; ACCEPTANCE: a real gryth package.json/lock materializes and
`node -e "require(...)"` smoke-passes.

**S3 (G3) — js/ts rules + `razel run` + rc-lite.**
Scope: razel-native rulepack AS A V3 §4 .bzl TRACK (`rules_shims_js.bzl` — registrations
only; the C3c ratchet holds): `js_binary` (node entry + node_modules dep), `ts_project`-lite
(one tsc action; inputs srcs+tsconfig+typings, outputs js); the **razel server skeleton +
the `razel run` verb as its first client** — verbs are server-API clients from day one
(`RazelPublicSurfaces.md` §1: no privileged in-process path; the wire is TAUT per its §4 —
the existing razel-wire IR grows the service messages, taut's TS backend emits the gryth
client). The skeleton rides the S0 crate shape (lib split + grazel bin + deny rule already
landed): this step adds daemon mode to the bins (razeld = `razel` in daemon mode) and
the first wired service; the grazel NODE (iroh) stays after the bootstrap bar —
gryth-dev side, track G continuation. Host node/tsc resolved like the cc
host toolchain (non-hermetic, digest-logged — same posture). **rc-lite**: the WORKSPACE
layer only of `.bazelrc` then `.razelrc` (command-scoped lines, no import/--config/system/
home yet) — enough that gryth's dev loop configures itself from files, not env vars; the
FULL ladder (all layers, `--config`, `import`, strict interplay) stays S6, which subsumes
rc-lite rather than reworking it (parse once, layer list grows).
Exit: hello `js_binary` runs with stdout golden; 2-file `ts_project` compiles with output
golden; ACCEPTANCE (the spike's heart): a gryth hello server in a razel-native module
builds and runs via `razel run`.

**S4 (B1) — cc linking + the goldens harness.**
Scope: `c++-link-executable` action_config (adopted config) + Native-path link +
File-ification of link artifacts; the examples goldens harness (capture/verify split over
per-example workspaces, strict mode wired); cpp-tutorial stage1–3 as the first corpus.
Exit: stage1–3 build AND RUN; binary stdout goldens green; normalized aquery graph parity
green (deviations documented, not silent); harness joins the probe sentinels.

**S5 (G4) — `razel test`.**
Scope: `razel test` verb (build → exec → exit-code protocol → test.log + summary line);
`js_test` (vitest/jest exec — standalone, NO runfiles).
Exit: green/red js tests behave; `razel test //...` over a gryth fixture.
**S5x (shared, TRIGGERED not scheduled) — the parallel action executor + `--jobs`/`-j`.**
Pulled forward only when the gryth dev loop MEASURES slow (review fix: not required to
prove the roadmap); otherwise lands with S8. Exit when built: independent actions
concurrent, measured wall win, `-j1` outputs byte-identical to serial.

**S6 (B2) — bazelrc + full `--strict_bazel` + discovery hardening.**
Scope: rc parsing grows from S3's rc-lite to the full ladder (system → workspace → home →
flag; command scoping; `--config`; `import`; `.razelrc` layered after each `.bazelrc`
layer); full strict semantics (rc/MODULE.razel skipped,
razel-only flags rejected); walk-up discovery shared by all verbs; `.bazelignore`.
Exit: flags-parsing-tutorial outcomes match bazel-7.7.0 (golden); TF's `.bazelrc`
consumed (`--deleted_packages` trims the census denominator — re-baseline the floor,
expected UP); strict mode becomes the harness default by construction.

**S7 (B3) — runfiles → cc_test/sh_test + `razel query`.**
Scope: runfiles staging (the registered ❌); cc_test/sh_test execution; the query verb
(deps/rdeps/pattern over the existing razel-analysis machinery).
Exit: query-quickstart goldens green (normalized output parity); a cc_test passes under
its runfiles tree; sh_test works.

**S8 (shared) — content-keyed action cache.**
Scope: action key = content digests of inputs incl. the resolved tool (the registered
toolchain-invalidation item); rebuild-without-change short-circuits.
Exit: second build of an unchanged tree = 100% action-cache hits, measured; `touch`ing a
source invalidates exactly its cone.

**S9 (B4) — java tier, then go/gazelle tier.**
Scope: javac/jar action-grade natives (java-tutorial), java-maven scope-checked at the
time; then `ctx.actions.write` + go natives (go-tutorial) and the gazelle workflow
(run-the-binary + analyze its output).
Exit: java-tutorial builds+runs golden; go-tutorial golden; gazelle round-trips on a
fixture.

**Two bars (review fix — the single bar was too large):**
- **Gryth-bootstrap bar = S0–S3:** a gryth hello server in a razel-native module builds
  and RUNS via razel, deps from the npm lock, dev loop file-configured, with the grazel
  binary shape pinned from S0. Gryth development
  STARTS here — everything after is improvement, not unlock.
- **Spike-release bar = S0–S6:** the bootstrap PLUS test verb, cc/goldens harness, full
  rc/strict/discovery — the honest Bazel story (cpp-tutorial + flags goldens strict-green,
  TF ≥455, every capability claim golden-backed). S7–S9 trail without blocking either bar.

**Decision points en route (Gianni):** gryth-dev bootstrap moment (first MODULE.razel in
that repo — after S3); pnpm vs npm
lock reality-check at S2; the TF floor re-baseline at
S6 (deleted_packages changes the denominator); java-maven scope at S9.

**Named risks:** single-lane bandwidth (tracks interleave by round unless a second session
materializes — history says don't assume it); node/tsc host non-hermeticity (accepted,
digest-logged, same as cc); npm lock-format drift (v3 assumed, verified at S2); the
goldens harness inheriting parity-normalizer gaps (deviations must be DOCUMENTED per
golden, never absorbed silently).
