# Razel Parity Harness — golden compat testing against Bazel

How razel validates its **rule representation** (the ~90% of Bazel compatibility: declaring
providers/attrs, the propagation query, and the invocation builder) by **differential testing
against Bazel itself**: Bazel is the spec, `aquery`/`cquery` expose its output, we diff.

**Three principles:**
1. **Bazel is the oracle.** We don't author "expected" outputs by hand (we'd just encode our
   own misunderstanding). The expected output is *Bazel's actual output* for the same `BUILD`.
2. **Goldens are captured once and checked in.** Bazel is a golden-*authoring* tool, never a
   test-*run* dependency. The suite runs with **no bazel, no JDK, no toolchains, no network.**
3. **Decoupled from the engine.** We compare the **declared** action graph + providers
   (analysis-time), so the harness needs **neither the engine/IVM nor the real compilers** —
   it tests the rule representation in isolation.

---

## 1. The corpus — a large set of independent case trees
Each case is a minimal, self-contained tree exercising **one** feature:
```
corpus/cc/transitive_link/
  BUILD              # the smallest rule call(s) that exercise the feature
  *.bzl  src/…       # inputs (sources, headers)
  golden.json        # CHECKED IN — the normalized expected action graph + providers
  meta.toml          # pins: bazel version · ruleset versions · platform · norm-spec version
```
- One feature per case (e.g. `cc/basic`, `cc/transitive_link`, `cc/select`, `cc/feature_opt`
  [Constrain], `rust/extern`, `rust/proc_macro` [exec instance], `java/exports`,
  `java/abi_jar` [compile-vs-runtime cutoff]). One failure → one case → one finding.
- **Scope the corpus to the V2-covered subset** (exec/host yes; general transitions,
  tree-artifacts, runfiles **no** — those are reserved). As a capability un-defers, add cases.

## 2. The golden — `golden.json` (normalized, canonical)
The declared output, *already normalized* (so the test is exact equality and the golden is
human-reviewable in a PR):
```json
{ "actions": [ { "mnemonic": "CppCompile",
                 "argv":    ["<cc>","-c","src/util.cc","-iquote","<exec>","-O2","-o","<bin>/util.o"],
                 "inputs":  ["src/util.cc","src/util.h","src/base.h"],
                 "outputs": ["<bin>/util.o"],
                 "env":     {"PWD":"<exec>"} } , … ],
  "providers": { "//pkg:util": { "CcInfo": { "headers":["util.h","base.h"],
                                             "link":["<bin>/libutil.a","<bin>/libbase.a"] } } } }
```
`actions` come from `bazel aquery`; `providers` from `bazel cquery --output=starlark`.

## 3. The capture xtask — the only thing that touches bazel
`cargo xtask capture-goldens [case…]` (run on add / edit / refresh only):
1. Assemble a **shared bazel workspace** (a `MODULE.bazel` + the pinned rulesets, set up
   **once** — *not vendored per case folder*) with each case mounted as a package.
2. `bazel aquery //case:* --output=jsonproto` → the action graph;
   `bazel cquery //case:* --output=starlark --starlark:expr='<per-provider field dump>'`
   → the providers. (Built-in commands — `AqueryProcessor`/`CqueryProcessor`; **no source
   instrumentation**.)
3. **Normalize** through the shared lib (§5), write `golden.json` + `meta.toml` (the pins).
A small per-provider-type `--starlark` expression is the only glue (dump `CcInfo`'s fields,
`JavaInfo`'s, …).

## 4. The hermetic test runner — no bazel, no toolchain
For each case: `razel-adapter-bazel` evaluates `BUILD`/`.bzl` → `razel-rulepack` produces
declared actions + providers → **normalize (same lib §5)** → assert `== golden.json`. Pure
analysis: **no bazel, no JDK, no clang/rustc/javac, no sandbox, no network.** A mismatch is a
precise structural diff (which action's argv/inputs, which provider field). The runner is
**read-only** against goldens — refreshing requires the capture xtask (bazel).

> This is *also* the fix for the ~28 toolchain-gated `exists(){return}` skips: the rule
> representation is tested on its *declared* output, with **Bazel as the oracle** instead of
> self-authored expectations.

## 5. The shared normalization spec — the one piece of "matching logic"
Used by **both** capture and the runner; **versioned** (`meta.toml` records it, so a
normalization change shows up as a reviewable golden refresh). Canonicalizes:
- **Paths** → placeholders: `<exec>` (exec root), `<bin>` (output), `<src>` (workspace),
  `<ext:repo>` (external repo); always workspace-relative, `/`-separated.
- **argv** → **preserve order** (a command line is order-significant — link order!). Only
  canonicalize *path tokens* inside argv, and sort *explicitly-allowlisted* set-like flag
  groups (e.g. repeated `-D`); **never blanket-sort argv** (that would mask real ordering
  bugs). The allowlist is part of the spec and reviewed.
- **env** → sorted by key; strip volatile (`TMPDIR`, absolute paths) via the placeholders.
- **digests / timestamps / output hashes** → stripped (compared structurally, not by content).
- **Files** → canonical `(repo, path)` form.
- **Drop Bazel-internal artifacts** not in razel's model (middleman actions, runfiles trees,
  symlink actions) — the **comparison scope** is explicit: we compare `{mnemonic, argv,
  inputs, outputs, env, providers}`; we document what is intentionally ignored.

## 6. What it validates (the bonus) — and what it doesn't
**Validates the *whole* rule representation, end to end, against Bazel:**
- **`Args`/invocation builder** — argv matches Bazel's *post-analysis* command line.
- **`Constrain`** — because Bazel's argv is *after* feature-config resolution, **matching argv
  validates razel's feature solver against Bazel's** (a free check of the one new construct).
- **propagation folds** — `cquery` provider fields match (CcInfo.link order, JavaInfo's
  compile-vs-runtime closures, the `exports` re-propagation).
- **per-edge / cross-instance** — `--extern name=` aliases, the proc-macro exec-instance
  output path.
**Does NOT cover (tested separately):** the engine/IVM (its own incremental/early-cutoff
tests), execution (sandbox/CAS — a small toolchain-gated integration set, *not* the parity
floor), the CLI/daemon.

## 7. Caveats / risks (so the diff is real, not flaky)
- **argv-order significance** (§5) is the subtle part — get the set-like allowlist wrong and
  you either mask bugs (over-sort) or get false diffs (under-sort).
- **aquery is post-analysis**: config/toolchain/transitions already applied → **pin the
  platform** in `meta.toml`; keep cases in the V2-covered subset.
- **Comparison scope** must be documented (what's compared vs ignored) so a "pass" means what
  we think it means.
- **Version drift**: a bazel/ruleset bump → re-capture → the golden diff is the *visible*
  record of what changed (a feature, not a silent break).
- **Capture setup**: a pinned bazel binary + fetched rulesets (the one-time cost; §plan).

---

## 8. Why this is the right architecture
- **Strongest compat signal possible** — differential against the reference implementation;
  we cannot fool ourselves with hand-written expectations.
- **Hermetic, fast, portable** — CI needs only the repo (goldens + a Rust toolchain).
- **Decoupled** — proves the compat-critical 90% *before* the engine exists, so engine work
  is wiring a known-good rule representation, not debugging fidelity and incrementality at once.
- **Reviewable** — a golden diff in a PR *is* the semantic change ("did cc's lowering drift").
- **Growable** — add a feature = add one case folder + `capture-goldens` once.

*The crates this tests (`razel-dds` / `razel-rulepack` / `razel-adapter-bazel`) + the corpus
are built FIRST and decoupled from the engine — see the reshaped `RazelV2FinalArchProposalPlan.md`.*

---

## 9. Empirical grounding — a real `bazel 9.1.1 aquery`/`cquery` probe (validated)
A `cc_library` probe (`/tmp/razel-parity-probe`, bazel 9.1.1) confirmed the strategy works and
grounded the normalization spec in *actual* output. Findings to bake in:
- **`cc_library //:util` emits THREE actions**: `CppModuleMap` (`util.cppmap` — modules/
  layering, emitted even for a basic lib), `CppCompile`, `CppArchive`. The comparison scope
  must decide per-action: razel either emits `CppModuleMap` (if it models modules) or the
  normalization **drops it** as a feature-gated extra (declare which).
- **`CppCompile` argv is post-feature-config** — the default flag set (`-U_FORTIFY_SOURCE
  -fstack-protector -Wall -Wthread-safety -std=c++17 …`) *is* the `Constrain` output. **So
  argv parity = the `Constrain` test.** razel's cc feature defaults must reproduce these.
- **`.d` is a declared output** (`-MD -MF …/util.d`) — confirms the `HeaderDiscovery` thread;
  for V2 it's just a declared output (the *pruning* of inputs from it is the deferred
  `DynamicIO` optimization).
- **Transitive header** `base.h` is an input of `util`'s compile — the propagation fold,
  observable in `aquery` inputs.
- **Host/platform-specific tokens that MUST be pinned + placeholdered** (else cross-machine
  flake): `-mmacosx-version-min=<sdk>` (machine SDK version!), `-frandom-seed=<bin-path>`, the
  `-D__DATE__/__TIMESTAMP__/__TIME__="redacted"` determinism flags. macOS archive is
  **`/usr/bin/libtool -static`**, not `ar` → the tool path is platform-specific (placeholder
  `<ar>`/`<cc>`). Pin platform in `meta.toml`; normalize these tokens.
- **Toolchain-wrapper indirection:** commands run via `cc_wrapper.sh` (cc) / `genrule-setup.sh`
  (genrule) with `external/rules_cc++cc_configure_extension+local_config_cc/…` inputs →
  normalize the wrapper + toolchain-config inputs to placeholders (`<cc_wrapper>`,
  `<cc_toolchain>/…`), or razel reproduces the wrapper. **Decide and document.**
- **Path placeholders confirmed:** `bazel-out/<config>/bin/…` → `<bin>`, `external/<repo>/…`
  → `<ext:repo>`. The `<config>` segment (`darwin_arm64-fastbuild`) is platform-derived → pin.
- **Strip:** the per-action `ActionKey:` digest (bazel-internal, not comparable).
- **`cquery` provider dump needs the provider *symbol*, not a string** — `providers(target)
  ["CcInfo"]` fails; use `target[CcInfo]` (with `CcInfo` `load`ed) in an `--output=starlark`
  file. Per-provider glue, as flagged.
- **Confirms `ArchAnalBazel`:** native `cc_library` is removed (`_removed_rule_failure`,
  `virtual_builtins_bzl/.../exports.bzl`) → must `load("@rules_cc//cc:defs.bzl", …)` +
  `bazel_dep(rules_cc)`. The corpus's capture workspace pins the ruleset versions.

**Verdict:** `aquery`/`cquery` produce exactly the golden we need; the normalization decisions
are now concrete (not assumed); first run ~16s, warm runs fast. The parity-first plan is
empirically de-risked on the make-or-break (cc) case.
