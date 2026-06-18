# RazelRustParityPlan — make razel's rust action graph faithful to Bazel

*2026-06-16, rev 1. Plan. Owner: RR.*

*Supersedes the optimistic **P3.11/P3.12** of [`RazelCrateUniversePlan.md`](RazelCrateUniversePlan.md)
(which assumed razel's rust argv was already near-Bazel and parity needed "~250 + a normalizer") and
absorbs the deferred external-`@crates` integration. The design contract is unchanged —
[`RazelCrateUniverseDesign.md`](RazelCrateUniverseDesign.md) §4–§8; this file says what to build, in
what order, how it's gated, and how big each step is, to close the parity gap that the build-script
implementation roll (P3.6→P3.10) left latent.*

## 1. Why this plan exists — a missing gate, not a missed flag

Bazel parity was always the rule, and the reference was always present: the rust golden was captured
in `496374a` ("Phase 2 — rust parity harness, rules_rust golden captured") and has shown Bazel's rich
rustc invocation (`process_wrapper`, `--flag=value`, `--target`/`--sysroot`, the hashed-output model)
the whole time. The gap went uncaught because **rust parity was never gated**:

- **cc and java each have a parity test** (`graph_parity.rs`, `java_graph_parity.rs` — they run on
  every gate). **Rust had the golden captured but no test consuming it.** Nothing ever diffed razel's
  rust graph against Bazel's.
- **razel's lean rustc argv is original** (`19321b7` "rust rules") — a minimal-viable shape
  (`--edition --crate-type lib --crate-name <root> -o <rlib>`) written before the parity harness. The
  whole crate-universe roll (P3.2→P3.10) built on it under **unit gates that assert razel's OWN shape**
  (`rust_rules::tests::p3*` pin `--crate-name withbs`, `argv[0]=rustc`). Those tests didn't just miss
  the gap — they entrenched the lean argv as "correct" and made it green.
- The plan's P3.11 estimate inherited that false confidence.

**Lesson (the discipline this plan enforces): the parity gate must LEAD, not trail.** A rust parity
test wired early — even red — would have shown the gap at P3.6, and every step would have built toward
the golden. So step **A1 wires the gate first and lets it be red**; the diff it prints is the work-list
the rest of the plan shrinks.

## 2. Guiding principle + deviation policy

**Bazel parity is the guiding rule (RR).** Where razel diverges from Bazel on anything that shapes the
compile, the fix is to **change razel to match Bazel**, not to allowlist the difference away.

A **deviation** is admissible only when it is one of:
1. a **normalized value** the harness already tokenizes — a content hash razel can't reproduce
   (`--codegen=metadata=-<hash>`, `extra-filename=-<hash>`), or a host/exec-volatile path
   (`--remap-path-prefix=${…}`, `--sysroot=…/<repo>/…`, the toolchain `-L`); or
2. a **whole action razel intentionally does not model** (the Bazel infra mnemonics) — handled by the
   existing `razel_parity::diff` `omit`-list, **logged in `Report::omitted`, never silently dropped**.

Every deviation is documented at its site (like the cc `CppModuleMap` omit). "Allowlist the flag" is
the last resort, not the first — if razel *can* emit the Bazel flag deterministically, it must.

## 3. The gap (verified against the captured golden)

For `//corpus/rust/build_script:withbs`, after the wrapper-prefix strip, the crate's rustc:

| concern | razel emits (today, `19321b7`/P3.6) | rules_rust / Bazel (golden) |
|---|---|---|
| flag syntax | `--crate-name withbs` (space) | `--crate-name=withbs` (joined) |
| crate type | `--crate-type lib` | `--crate-type=rlib` |
| output | `-o <dir>/libwithbs.rlib` | `--out-dir=<dir>` + `--codegen=extra-filename=-<hash>` + `--codegen=metadata=-<hash>` |
| toolchain | *(none)* | `--target=`, `--sysroot=`, `-L <toolchain>` |
| standard | *(none)* | `--emit=dep-info,link`, `--error-format=`, `--color=`, `-Cembed-bitcode=`, `--codegen=opt-level/debuginfo/strip` |
| hermeticity | *(none)* | `--remap-path-prefix=${pwd|exec_root|output_base}=.` (×3) |

razel emits ~6 semantic tokens; Bazel ~20. The gap is **structural** (syntax + output model + flag
set), so closing it is a real rework, not a normalizer tweak.

**Reference assets (already captured, reusable):** the normalized local golden
`parity/corpus/rust/build_script/golden.txt` (`ff81cd5`); the full external closure raw at
`/tmp/blake3_aquery.txt` (11093 lines: 96 Rustc, 11 CargoBuildScriptRun, 23 ExtractCargoTomlEnvVars,
the cc SIMD CppCompile/CppArchive). The implementation pipeline (P3.6→P3.10: compile → parse → run →
wrapper(write/read) → edge) is built + unit-tested; this plan makes its *output* Bazel-faithful.

## 4. How to read a step

Test-first (`AGENTS.md`). Each step keeps the standing gates green and shrinks the parity diff:

- **rust-aq** (NEW — the driver this plan adds): `crates/razel-loading/tests/rust_graph_parity.rs`
  renders razel's `AnalyzedAction` set for the rust corpus case → the wrapper-prefix canonicalizer →
  `razel_parity::{parse_golden,diff}` vs `parity/corpus/rust/<case>/golden.txt`. RED until A6; the diff
  is the per-step work-list.
- **per-step (tiered, velocity):** `cargo test -p razel-loading --lib` + the two carve-out sentinels
  (`--test graph_parity --test java_graph_parity`) + the relevant `rust_graph_parity` case. NOT
  `--workspace` per step (see the razel-roll-gate discipline).
- **xp** = execution-parity golden — diff razel's build-script flags-file + the produced rlib vs
  Bazel's `<name>.out` (§6.1/§8).
- **WS** = `cargo test --workspace` (phase tags only) · **G** = `cargo xtask gates` ·
  **PL** = `cargo xtask perfgate`.

The only standing reds are the 2 cc/java carve-outs; `rust_graph_parity` joins them as a documented
carve-out from A1 until it greens at A6.

## 5. Phase A — the gate, then a faithful argv (local build-script case)

Prove razel's argv against the LOCAL golden before the external closure — the local case isolates the
argv rework from the (separate) external-resolution work of Phase B.

- **A1 — wire the rust parity gate (the driver)** · ~150 · dep: — · §8
  New `rust_graph_parity.rs` (mirrors `graph_parity.rs`): analyze `//corpus/rust/build_script:withbs`
  (+ `:build_script_build`), render, normalize, `diff` vs the golden. Add the **wrapper-prefix
  canonicalizer** to `razel-parity`: strip a process-wrapper prefix (`…/process_wrapper … --` OR
  `razel-process-wrapper rustc … --`) + a leading rustc-binary token → the bare rustc args; applied to
  BOTH sides pre-diff (no-op for cc/java — their argv has no wrapper). **Lands RED**, committed as a
  documented carve-out. Gate: the test compiles, runs, prints the diff.

- **A2 — argv syntax + crate-type** · ~120 · dep: A1 · §5.5
  Rework the `rust_library`/`rust_binary`/`cargo_build_script` rustc argv to rules_rust's `--flag=value`
  joined syntax (`--crate-name=`, `--edition=`, `--crate-type=rlib`, `--codegen=…`) with the crate_root
  as a leading positional. **Update the `p3*` unit tests** that pin the lean shape (they were the
  entrenchment — rewrite them to the faithful shape, not delete the coverage). Gate: rust-aq diff
  shrinks; lib unit tests green.

- **A3 — the rules_rust standard flag set** · ~150 · dep: A2 · §5.3
  Emit the deterministic flags razel omits: `--target=<host-triple>`, `--sysroot=<toolchain>`,
  `-L <toolchain-lib>`, `--emit=dep-info,link`, `--error-format=human`, `--color=always`,
  `-Cembed-bitcode=no`, `--codegen=opt-level/debuginfo/strip` (from the compilation mode). Toolchain
  paths normalize to `<repo>`. Gate: rust-aq shrinks.

- **A4 — `--remap-path-prefix` (hermeticity)** · ~60 · dep: A3 · §8
  Emit the three `--remap-path-prefix=${pwd|exec_root|output_base}=.`; the abs paths normalize. Gate:
  rust-aq shrinks.

- **A5 — the hashed-output model (the ripple)** · ~250 · dep: A2 · §5.5/§4.3
  Replace `-o <rlib>` with `--out-dir=<dir>` + `--codegen=extra-filename=-<hash>` +
  `--codegen=metadata=-<hash>`. razel mints a deterministic per-crate metadata hash (its own —
  normalized to `-<hash>` in the diff); the rlib becomes `lib<name>-<hash>.rlib`. **Ripple — thread the
  hashed path through:** dependents' `--extern <name>=<…-hash.rlib>`, `default_info`, and the
  build-script edge inputs (P3.10). The largest step — it changes the output IDENTITY. Gate: rust-aq
  shrinks; the dep/extern tests stay green.

- **A6 — deviation allowlist + GREEN** · ~100 · dep: A1–A5 · §8
  Finalize the **minimal, documented, logged** deviation set per §2: the normalized values + the
  mnemonic `omit`-list for the Bazel infra actions
  (`Symlink`/`RunfilesTree`/`RepoMappingManifest`/`SourceSymlinkManifest`/`SymlinkTree`/`ExecutableSymlink`).
  `rust_graph_parity` flips **GREEN** (documented deviations only). Also wire `rust/transitive` → aq
  green (the original ungated baseline — close it too). **Milestone A: rust analysis-parity on the
  build-script edge + the transitive baseline.** Gate: rust-aq GREEN.

- **A7 — execution parity (local)** · ~150 · dep: A6, P3.9 · §6.1/§8
  `razel build //corpus/rust/build_script:withbs` → the build-script flags-file + the rlib; diff the
  flags-file vs Bazel's `<name>.out` (xp). The pure-rust build script needs no cc env, so the cc
  `CC`/`AR`/`CFLAGS` leg (deferred P3.8d leg 2) is NOT exercised here — it lands at B4. Gate: xp green
  (flags-file matches; rlib exists).

## 6. Phase B — the external `@crates//:blake3` integration (Milestone-1)

With razel's argv proven faithful locally, apply it to the real closure. Phase B is the larger,
previously-undeferred Milestone-1 work the P3.11 probe surfaced (`razel build @crates//:blake3` →
`unknown target: blake3`).

- **B1 — resolve `@crates//:blake3` in the build path** · ~250 · dep: A6 · §2.2/§5.6
  Fix `unknown target`: wire external `@crates` repo + the `:build_script_build`/crate aliases into the
  build/analysis path (distinct from `query` v1, which defers external `@crates//` to `q4` — §13). Load
  the versioned crate repo's BUILD + resolve the alias. Gate: `razel build @crates//:blake3` gets past
  target resolution into analysis.

- **B2 — the full closure analyzes** · ~iterative · dep: B1 · §5
  Drive razel on `deps(@crates//:blake3)` (~20 crates, 11 build scripts, cc SIMD, proc-macro deps);
  close the gaps the closure surfaces (`proc_macro_deps` → P4.1, the cc edge for SIMD, the per-crate
  `crate_features`/`target_compatible_with` arms) until razel analyzes the whole graph. Each gap is a
  sub-step; `log` any coverage cap (no silent truncation). Gate: razel produces the full analyzed
  action set without error.

- **B3 — blake3 analysis parity** · ~150 · dep: A6, B2 · §8 *(was P3.11)*
  Commit the normalized `parity/corpus/rust/crate_blake3/golden.txt` (from the captured aquery);
  `rust_graph_parity` over blake3 with the SAME canonicalizer + deviation allowlist proven in Phase A.
  Gate: blake3 aq green (documented deviations only).

- **B4 — blake3 execution parity** · ~150 · dep: A7, B3 · §8 *(was P3.12)*
  The cc env leg (deferred P3.8d leg 2): `CC`/`AR`/`CFLAGS` from `razel-cc-toolchain` for the
  build-script run (blake3's SIMD `.o`s). `razel build @crates//:blake3` → the rlib + the SIMD `.o`s in
  `OUT_DIR`; diff the flags-file. Gate: xp green; rlib + SIMD `.o`s present.

## 7. Definition of done

- **Milestone A (analysis parity, local):** `rust_graph_parity` green on `build_script` + `transitive`,
  documented deviations only; razel's rust rustc argv is rules_rust-faithful.
- **Milestone-1 (the original DoD):** `razel build @crates//:blake3` → the rlib, with **aq green**
  (B3) and **xp green** (B4) — razel reproduces Bazel's blake3 action graph + the SIMD `.o`s.

## 8. Relationship to the existing plan + the discipline going forward

- This plan **replaces P3.11/P3.12** in `RazelCrateUniversePlan.md` (the realistic decomposition) and
  **carries the deferred legs**: P3.8d leg 2 (cc env) rides B4; `DEP_<LINKS>_*` stays P4.5;
  `proc_macro_deps` stays P4.1 (B2 surfaces where it's first needed).
- **The gate leads.** `rust_graph_parity` exists from A1 and is RED until A6 — the inverted order is
  the point. No further rust action work lands without the parity diff as its arbiter. The root cause
  here was a parity rule with no parity gate for rust; this plan installs the gate and never lets the
  unit tests stand in for it again.
