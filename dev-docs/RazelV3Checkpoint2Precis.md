# RazelV3 Checkpoint 2 — Précis

*2026-06-10, at `razelV3/l6-cc-common-compile` (31 razelV3 tags on top of checkpoint 1's 133).
Companion to `RazelV3Plan.md` (the supervisor contract) and `RazelV2Checkpoint1Precis.md`
(checkpoint 1 + the per-round deltas for rounds 1–10). Same question as checkpoint 1: how far is
razel from building a complex real codebase (TensorFlow as the yardstick) — measured against
where checkpoint 1 said we were.*

## 1. What changed since checkpoint 1 (grounded inventory)

**Code:** ~25.4k LOC Rust (was ~19.8k), 60 green test binaries (was 53), 2.2k test LOC, 688
lines of host-repo Starlark (a new artifact class — see §3), same three enforced gates, plus a
probe harness (`xtask probe`) with **5 must-pass sentinels** (was 0 — the sentinel set IS the
capability ratchet now).

**Checkpoint 1's honest statement was:** "a validated architecture spike … has just crossed the
threshold of *loading* (not yet instantiating) real upstream rules. Nothing real has been built
through a real upstream ruleset yet."

**Checkpoint 2's honest statement:** razel now **analyzes real upstream rules end-to-end**.
Specifically:

- **`rules_rust`'s `rust_library` analyzes to completion** (sentinel #5, must-pass): the real
  1,781-line `rust.bzl` + 2,000-line `rustc.bzl` walk `rustc_compile_action` →
  `collect_inputs` → `construct_arguments` → `establish_cc_info` and return providers, against
  a real crates.io package (tinyjson, rules_rust's own bootstrap dep).
- **`cc_common.compile()` exists and runs real compile command lines** — implemented in *host
  Starlark* over the generic four-move API (see §3), not in Rust.
- **The TF probe is inside rules_cc's real `cc_binary_impl`, building `protoc`** — through
  protobuf's macro layer, toolchain resolution (`find_cc_toolchain`), and `cc_library_impl`'s
  full attribute/context assembly.
- **The engine subsystems checkpoint 1 marked ❌ are now real**: `select()` resolves against
  declared `config_setting`s (values/define_values/groups/aliases/constraints, deferred to attr
  consumption — Bazel's actual model, not first-branch); cross-package provider flow
  (freeze-and-harvest with sound cross-heap references); demand-driven analysis (dep-loaded
  packages defer **all** decls; targets analyze when demanded — Bazel's model, and the fix for
  two real bugs: false cycles and re-entrancy mis-canonicalization); cross-repo label semantics
  (lexical binding at the call site, `Label` values with `relative()`, bare-`@repo` canon);
  `ctx.toolchains`.

**What "analyzes" does NOT mean:** nothing upstream-built has *executed* yet. The actions are
real argv (real clang/rustc command lines from the Constrain engine), but no upstream-analyzed
action has been run and its output checked. That's the L3/L4 golden gap — the next ratchet.

## 2. The distance to TensorFlow, revised

Checkpoint 1's table, updated (✅ have · 🟡 partial/stub · ❌ not started):

| Subsystem | Was | Now | Notes |
|---|---|---|---|
| Loading: real external repos, load graphs | ✅/🟡 | ✅ | rules_rust/rules_cc/protobuf/skylib graphs walk; session-wide .bzl cache (identity); bare/legacy load forms; still no bzlmod/WORKSPACE eval (vendored-only, by design) |
| `rule()`/attrs schema | 🟡 | ✅/🟡 | type defaults, label-kind default resolution, implicit attrs via schema; `cfg=`/`providers=` still absorbed |
| `ctx` surface | 🟡 | ✅/🟡 | label/attr/files/file/executable/outputs/actions/toolchains/var/build_setting_value real; fragments/runfiles/expand_location absorb |
| Providers define/construct/flow | ✅/❌ | ✅ | capture-from-return, `dep[P]`, `P in dep`, implicit DefaultInfo, cross-package identity — all real |
| Configuration: select/config_setting | ❌ | ✅/🟡 | real resolution incl. groups+aliases+host constraints; flag_values/build settings still unmodeled (loud) |
| Toolchain resolution | ❌ | 🟡 | `ctx.toolchains` rows real (rust full, cc host-true scalars); `rule(toolchains=)` declarative resolution still absent |
| Transitions / aspects / exec groups | ❌ | ❌ | stubs absorb; unchanged — aspects are the proto pipeline's gate (L5) |
| Repository rules / fetch | ❌ | ❌→host | unchanged as a subsystem; in practice replaced by vendoring + 13 host-materialized repos (the generated-repo pattern covers TF's configure outputs) |
| Native rule fidelity | 🟡 | 🟡+ | genrule is real (Make vars, tools=, cmd_bash); cc fidelity now rides rules_cc itself rather than razel's toy shim |
| Execution at scale | ❌ | ❌ | unchanged; nothing upstream-analyzed has run |

**Calibration:** checkpoint 1 said "the remaining semantic surface is roughly 10× the code that
exists today." Rounds 1–10 added ~30% more code and closed the *engine-shaped* half of that
surface — the part with crisp semantics (labels, selects, providers, demand analysis). What
remains is differently shaped: **cc_common/link is a member-by-member walk** (compile() took one
session; link/linking_context is comparable), **aspects** are a real subsystem (proto needs
them), and **execution goldens** are verification work, not semantics. The 10× was the right
order of magnitude; the remaining multiple is smaller but the work is lumpier — fewer, bigger
pieces.

## 3. The architectural result worth recording

Checkpoint 1's plan assumed cc_common would be Rust ("absorb→real over the Constrain §8c
engine"). It landed differently and better: **`cc_common.compile` is a `.bzl` function** in
`@cc_compatibility_proxy` (a razel host repo), calling `razel_build.command_line("cc", …)` +
`razel_build.action` — the same four-move API razel's own bundled rules use. The engine grew
exactly one language-neutral primitive to enable it: `razel_host_absorb_with(overrides)` — an
absorbing value with named real members.

This validates the design ruling from checkpoint 1 ("no per-language Rust ever; languages are
`.bzl` + registrations") at the hardest point available: Bazel's most complex native module is
expressible as host Starlark over the generic engine. The pattern generalizes:

- **Host repos are razel's answer to generated repos AND native modules.** 13 materialized
  repos (688 lines of Starlark) replace TF's configure-time repository rules
  (local_config_cuda/rocm/sycl/tensorrt, python_version_repo, …) and Bazel's built-in packages
  (@bazel_tools//tools/cpp). They are data, reviewable, and grow member-by-member as probes
  demand.
- **Absorb semantics are now Bazel-shaped**: falsy, empty-iterating, empty-membership,
  len()==0, slice→absorb. An absorbed unknown behaves like an *empty* thing, which is what real
  rule code expects of optional machinery it probes with `if`/`hasattr`/`in`.

Two engine-level semantic fixes this session matter beyond the probe that surfaced them, both
Bazel-fidelity wins: **demand-driven everything** (dep-loaded packages defer every decl; eager
native drives manufactured false dependency cycles) and **decl-origin package context**
(cross-package demand chains re-entering a package mid-load now canonicalize `:dep` attrs
against the decl's own package, not the demander's).

## 4. Debts created this session (loud, registered)

- Args param-files are recorded as no-ops (inline argv kept) — must become real `@file`
  emission at the L3 run-golden.
- Provider instances don't track declared fields: unset fields read as `None` (Bazel errors on
  undeclared access). `CcInfo()` correctness is restored via `init=` defaults; the general case
  is a registered gap.
- `get_artifact_name_for_category` is a name passthrough (category objects absorb) — lib
  naming (`libX.a`/`.dylib`) lands with the link goldens.
- `compilation_mode_opts`/rust toolchain scalars are host-pinned constants (rustc 1.95.0,
  clang, macOS SDK sysroot via xcrun) — right values, not yet *configured* values.
- DefaultInfo synthesis coerces stray path strings to File defensively — the string/File seam
  is closed at to_list/dep-fields/ctx.files, but the coercion masks any remaining producer.

## 5. The next rungs (in order)

1. **Finish the protoc walk (L6-cc):** the current frontier is trivial (`CcInfo in dep` with a
   None dep item in cc_binary's runtimes path); behind it, cc_common's link-stage members
   (`create_linking_context`, `link`, linking_context traversal) grow in host Starlark exactly
   as compile() did.
2. **L3/L4 goldens — run something upstream-analyzed.** Two candidates, both close: execute the
   tinyjson `rust_library` action set (rustc is real, paths are real), and an abseil leaf
   `cc_library` (clang command lines are real). This converts "analyzes" to "builds" and makes
   the param-file/lib-naming debts due.
3. **Proto pipeline (L5):** proto_library analyzes today through the deferred-native fix; the
   gate is aspects (`proto_lang_toolchain` + the cc/py aspect chain). First real new subsystem
   since checkpoint 1.
4. **TF full-tree load driver:** a packages-loaded/targets-analyzed metric over
   `//tensorflow/core/...` to replace single-target probes with a coverage curve — the
   checkpoint-3 yardstick.

## 6. Risks, revised

Checkpoint 1's top risk — "integration surface, nothing built yet" — is half-retired: the
surface integrates (real rulesets run). The live risks now:

- **Silent-wrong absorption.** Falsy-Absorb made far more real code *run*; every `if absorbed:`
  that skips is a place Bazel might not skip. The mitigation is unchanged and working — goldens
  + loud errors at typed boundaries — but the absorb surface is bigger now, and only run-goldens
  (rung 2) actually discharge it.
- **cc_common semantic depth.** compile() was expressible in ~100 lines of host Starlark
  because the Constrain engine already does feature configuration. link + linking_context
  carries library-to-link structures with more internal shape; if it stops fitting the
  absorb-with pattern, the fallback is real value types — more Rust, same seam.
- **Aspects.** Genuinely new machinery with no spike yet; sized as a subsystem, not a probe
  step.
- **Single-host pinning.** The toolchain rows encode this Mac. Fine for the TF yardstick;
  becomes a debt the moment goldens need to run anywhere else.
