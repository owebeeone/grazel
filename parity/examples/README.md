# parity/examples — the examples-as-goldens corpus (S4b harness)

Captured by `cargo xtask examples --capture` (bazel-touching, authoring-only);
verified by `cargo xtask examples` (razel-only; graph in Adopt-Bazel mode + run
stdout in Native, each in default AND `--strict_bazel`).

**Oracle version: bazel 9.1.1 (homebrew), captured 2026-06-12.** The public-claim pin
(`RazelPublicSurfaces.md` §3) names bazel-7.7.0 (the TF-era oracle); a 7.7.0 binary is
not present on this machine. OPEN DECISION (Gianni): re-pin the examples tier to
current bazel, or fetch 7.7.0 and re-capture. Several graph deviations below are
9.x-era artifacts.

## Status (S4 complete, 2026-06-12)

- **stdout goldens: GREEN** (all 3 stages; `<TIME>`-masked ctime line).
- **graph goldens: GREEN** (all 3 stages, default + strict) — every action key
  matches (Phase E: faithful cc_binary via the c++-link-executable action_config;
  CppLink argv byte-matches incl. multi-lib); ONE documented argv deviation, logged
  every run: CppCompile carries bazel's module-graph-derived bzlmod -iquote set
  (external/rules_cc+, external/bazel_tools + bin twins) which razel grows with the
  bzlmod arc. The harness IS a probe sentinel (`xtask probe` fails on regression).

## Graph deviation classes (the S4 ticket feed — documented, never silent)

1. **Path model:** the examples load the NEW per-rule rules_cc layout
   (`@rules_cc//cc:cc_binary.bzl`); razel's @rules_cc shim serves those names from
   the NATIVE rules, so analysis emits native-shaped outputs (`main/hello-world.cc.o`)
   even under `CcToolchainMode::AdoptBazel` — the bundled faithful defs (bazel-out/
   `<cfg>`/bin + `_objs/` + dotd files) only back the old `cc:defs.bzl` entry. Fix =
   route the per-rule .bzl paths onto the adopted config (the spike's
   "c++-link-executable action_config" work).
2. **Unmodeled binary-target machinery** (golden-only actions): runfiles
   (`SourceSymlinkManifest`/`SymlinkTree`/`RepoMappingManifest`), `CcStrip` +
   `.dwp` `FileWrite`, build-info (`TranslateBuildInfo`/`Symlink redacted_file.h`),
   and @bazel_tools `link_extra_lib`/`malloc`/`empty_lib` externals. Each needs a
   model-or-omit decision with rationale; none may be silently dropped.
3. `CppModuleMap` is already a documented omit (matches the corpus posture).
