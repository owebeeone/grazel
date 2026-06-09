# Design review — `RazelHookSeam.md` (Phase C3, the per-language hook seam)

Reviewer pass: verified every §2/§4 claim against the code under
`crates/razel-loading/src/`, `crates/razel-dds/src/lib.rs`,
`crates/razel-cc-toolchain/src/lib.rs`, `xtask/src/main.rs`, plus the surrounding
plan/gaps docs. Citations are `file:line`.

## Verdict

**Execute with changes.** The three-stage shape (C3a registry → C3b resolver hook →
C3c gate-last) is sound and the C3b "hard call" (defer the generic `Toolchain` type)
is the right call. But the §2 leak inventory is **incomplete** in a way that bites C3a
directly: it omits `deps.rs` (a generic module that hardcodes the `CcInfo` fold and is
the resolution path for *four* native languages), and it under-states the dep-struct
field-name pinning. Both are must-fix before C3a is scoped, because they change what
"iterate the registry generically" actually has to do. Fix the inventory and the
registry's field-name mapping story, and the plan is executable.

## Strengths

- **Staging order is correct, and "gate last" is justified by the code.** The existing
  `xtask gates` (`xtask/src/main.rs:165`–`181`) is a pure substring scan that fails on
  *any* match. Landing a language-literal gate before C3a/C3b evict the leaks would
  trip on the ~40 live `CcInfo`/`JavaInfo`/`"cc"` occurrences (confirmed by grep across
  `state/engine/dds/dialect/deps/rules.rs`). Gate-last is not a preference; it is forced
  by the scanner's all-or-nothing semantics. Good.
- **C3b option (ii) over (i) is the honest call.** The two toolchains genuinely do not
  share a type: cc is a `FeatureConfig` evaluated from Starlark
  (`razel-cc-toolchain/src/lib.rs:30`, `:226`), java has *no toolchain object at all* —
  `java_defs.bzl` builds argv as a literal template and rides `razel_build.action`
  directly (`java_defs.bzl:40`–`69`), never calling `command_line`. A generic
  `Toolchain { fn command_line() }` trait today would be a one-member abstraction whose
  only implementor is cc. Deferring it to Phase D (where a second *command-line-shaped*
  toolchain arrives) is correct, and the doc names the residual debt honestly (§4 ii,
  §8).
- **The "registration, not core edit" invariant is real and already half-built.**
  `ruleset_modules` (`rules.rs:130`–`167`) is exactly the table the doc points to: rust,
  py, sh all already land as one row + one module each. The §6 allowlist boundary
  (`ruleset_modules` + per-language modules) is the right line.
- **AD2 framing is consistent with the codebase.** The per-`Session` registry mirrors
  `Session::resolved_cc`/`host_cc` (`state.rs:122`, `:133`) and the existing AD2 gate's
  F13 rationale (`xtask/src/main.rs:144`–`154`). No ambient-state regression.

## Concerns (ranked)

### 1. The §2 inventory MISSES `deps.rs` — a generic module with a hardcoded `CcInfo` fold. [MUST-FIX before C3a]

**Issue.** `deps::resolve_dep` (`deps.rs:38`–`71`) is the dep-provider resolution path
for **all four native rulesets** (cc `native_cc.rs:37,98`; rust `rust_rules.rs:68`; py
`py_rules.rs:47`; sh records no deps). It hardcodes `ProviderTypeId::new("CcInfo")`
(`deps.rs:63`) and folds `hdrs`/`cflags` by literal name (`deps.rs:70`). The §2 table
lists only `state/engine/dds/dialect/rules`. `deps.rs` is absent.

**Why it matters.** This is not a per-language registration site — it is generic
machinery (the doc's own test for "is it a leak"). Worse, Python *abuses* the
`CcInfo.hdrs` channel to carry `.py` sources (`py_rules.rs:84` writes
`cc_provider_map(exported, ...)`; `py_rules.rs:47` reads `dep.hdrs`), so a naive C3a
that "moves `cc_provider_map` to the cc module" (§3 final bullet) will break the python
ruleset, which has no business depending on a cc helper. The inventory as written would
let C3a land believing the core is de-leaked while `deps.rs` still pins `CcInfo` and py
still depends on it.

**Recommendation.** Add `deps.rs` to the §2 table as a generic leak (the `DepInfo`
struct's `hdrs`/`cflags` + the `CcInfo` fold). Decide explicitly: either (a) `DepInfo`
becomes registry-driven like the dialect dep-struct (§3), or (b) the py "sources via
`CcInfo.hdrs`" hack gets its own provider/channel. Until then, note that the native-rule
dep path and the Starlark-rule dep path are *two* separate hardcoded folds
(`deps.rs:63` vs `dialect.rs:135`–`161`) — C3a must de-leak **both**, and the doc only
addresses the dialect one.

### 2. The dep-struct field names are pinned by the `.bzl` interface, and there are THREE non-matching name spaces. §3's "field names come from the registry, not literals" is too glib. [MUST-FIX framing before C3a]

**Issue.** §3 says the dialect dep-resolution can "iterate the registry … and project it
onto the dep struct generically (the struct's field names come from the registry, not
literals)." But the dep struct is consumed by Starlark by **literal attribute access**:
`cc_defs.bzl:23` reads `d.headers`; `java_defs.bzl:34`–`35` read `d.compile_jars` /
`d.runtime_jars`. The struct is built at `dialect.rs:163`–`168` with literal keys
`files`/`headers`/`compile_jars`/`runtime_jars`. So the dep-struct field *names* are
part of the rule↔engine ABI — a registry can supply them, but only if the registry also
stores the *dep-struct projection name*, which is **not** the provider field name:

- provider type/field: `CcInfo.hdrs` (`state.rs:52`, `dds.rs:43`, written by the
  `CcInfo(headers=…)` builtin at `dialect.rs:376`–`389`)
- dep-struct field the `.bzl` reads: `d.headers` (`dialect.rs:165`, `cc_defs.bzl:23`)

i.e. the provider field is `hdrs` but the dep-struct field is `headers`, and the builtin
kwarg is *also* `headers`. Three names, two of them colliding by luck for java
(`compile_jars`/`runtime_jars` happen to match) but **not** for cc (`hdrs` ≠ `headers`).

**Why it matters.** "Iterate the registry generically" is feasible, but only if the
registry schema carries `(provider_ty, provider_field, fold_policy, dep_struct_name)`
per projected field — a richer record than §3's `provider → fields → kind/policy`
sketch (`RazelHookSeam.md:48`–`50`). If C3a is scoped as "just move the names into data"
(§3 calls it "mechanical, low-risk"), it will under-model this and either (a) silently
rename `d.headers` → `d.hdrs` and break `cc_defs.bzl`, or (b) hardcode the
provider→struct mapping after all, which is the leak it set out to remove.

**Recommendation.** Make the registry's projection record explicit in §3:
each registered provider field declares its dep-struct projection name + fold flavour.
Note in the doc that this is a small ABI between the registry and the bundled `.bzl`,
and that the `.bzl` field names are now *data-driven contract*, not incidental.

### 3. C2 is described as "done / converged" but `razel_build.info` does not exist, and the algebra is narrower than the plan claims. [accuracy — affects the C3 premise]

**Issue.** The doc's opening asserts C0–C2 are done and the provider model "converged on
the `razel-dds` value algebra (one fold, two instances)". Two mismatches with the code:
- Plan §10 C2 (`RazelStarlarkBoundaryPlan.md:328`) specifies
  `razel_build.info(schema, direct, deps)` as the generic constructor and "*Green:* both
  providers via the one constructor." There is **no `info` builtin** —
  `razel_build_members` exposes only `command_line` and `action` (`engine.rs:101`–`151`;
  grep for `fn info` / `"info"` returns nothing). Provider capture is still done by the
  hardcoded `CcInfo`/`JavaInfo` builtins (`dialect.rs:376`, `:399`). So C2's *info* move
  was not built; what landed is the storage convergence (the `FieldValue` map +
  `DdsRead` fold), which is real and good, but it is not the §10 C2 surface.
- Plan §10 C2 lists "nested struct" in the algebra; `razel-dds` has only
  `Scalar`/`Set`/`OrderedDepset` (`razel-dds/src/lib.rs:81`–`92`); `Map` and nested are
  reserved, not present.

**Why it matters.** C3's whole premise is "formalize the hooks now that the engine is
generic." If the generic *provider constructor* (`info`) was never built, then C3a is not
"move names from literals into data" on top of a generic constructor — it is *also*
building the generic capture path that C2 was supposed to deliver (replacing the two
hardcoded builtins). That is more than "mechanical." The doc should either reconcile
("C2 delivered storage convergence; the `info` constructor folds into C3a") or carve the
constructor out as an explicit C3a sub-step. As written it over-states the starting
point.

**Recommendation.** Add a one-line "what C2 actually shipped vs §10 C2" note, and fold
the missing generic `info`-style capture into C3a's scope explicitly (it is the natural
home — registry-driven capture and registry-driven fold are the same registry).

### 4. The B5 ledger that §0/§5 lean on is not in the repo. [accuracy / traceability]

**Issue.** The doc repeatedly grounds the extension points in "the B5 ledger"
(`RazelHookSeam.md:5`, `:16`, §5) and the plan's exit (`RazelStarlarkBoundaryPlan.md:318`
B5 "the ledger"). No such file exists: `RazelJavaSpikeLedger.md` is absent; a repo-wide
grep for `JavaSpikeLedger`/`SpikeLedger`/`B5 ledger` matches **only `RazelHookSeam.md`
itself**. The closest artifact is `dev-docs/history/D5-spike-gate.md`.

**Why it matters.** The "extension points the ledger surfaced" (toolchain resolver +
action-transform for ijar/include-scan) are the doc's authority for *which* hooks exist.
If the ledger was never written (or lives only in history), the action-transform hook in
particular (§5) rests on an unverifiable source. The ijar/include-scan transforms are
not present anywhere in the code today (java does ijar inside the `.bzl` template as a
plain `Turbine` action, `java_defs.bzl:40`–`46`; there is no post-processor seam).

**Recommendation.** Either point §5 at the actual ledger (fix the path / link
`history/`), or restate the action-transform hook's justification from first principles.
And see concern 5 — the action-transform "hook" may be premature regardless.

### 5. The action-transform hook (§5) is a seam with zero current users and no clear shape — risk of speculative scaffolding. [nice-to-have / consider deferring]

**Issue.** §5 proposes a *registration point* for include-scan / ijar transforms, impls
stubbed. But: cc has no include-scan in razel (it relies on Bazel-style pre-declared
hdrs — `deps.rs` header comment, `native_cc.rs:60`), and java's "ijar" is just the
Turbine action emitted inline by `java_defs.bzl`, not a post-processor over another
action's I/O. So the hook would be a registration API with **no caller and no concrete
transform to shape it** — exactly the "premature abstraction" the doc rightly warns
against for the `Toolchain` type in §4.

**Why it matters.** Defining a registration shape with no instance to pressure-test it
risks baking in the wrong signature (the same one-member-abstraction trap §4 avoids).
This is inconsistent with the doc's own discipline.

**Recommendation.** Defer the action-transform *seam* to Phase D alongside the real
transforms (the doc already defers the *impls* to D, §8). For C3, document the extension
point in prose as a known Phase-D seam; do not land a registration API yet. This also
shrinks C3c.

### 6. C3c gate: `"cc"`/`"java"` are common-substring false-positive magnets; the existing scanner's comment-skip is too weak. [must-fix the token list before C3c]

**Issue.** The model gate (`gate_violations`, `xtask/src/main.rs:165`–`181`) is a raw
`line.contains(pattern)` with a single mitigation: skip lines whose *trimmed start* is
`//` (`:171`). That works for distinctive tokens (`thread_local!`, `OnceLock`). The C3c
banned set includes `"cc"` and `"java"` (`RazelHookSeam.md:106`–`108`), which appear as
substrings in `success`, `accumulate`, `JavaScript`, identifiers, doc prose mid-line,
etc. The comment-skip only catches full-line `//` comments, not trailing `// …` or
in-code identifiers. False positives will be rampant; to suppress them you either gut the
token (only match `"cc"` with quotes — but then `CcToolchainMode` / `cc_provider_map`
identifiers slip the net = false negatives) or accrete an allowlist that erodes the gate.

**Why it matters.** A gate that is noisy gets `#[allow]`-ed into uselessness, and at
**N=2** languages a string scan is arguably premature anyway (concern: the leak set is
small enough to eyeball; the gate's value is *forward* enforcement). The doc should be
honest that the gate is a tripwire for the next language, not a today-necessity.

**Recommendation.** (a) Ban the *distinctive* tokens only — `CcInfo`, `JavaInfo`,
`macos_core_config`, `cc_provider_map`, and the provider field-name literals — not the
bare `"cc"`/`"java"` substrings (the `"cc"` engine match is removed by C3b anyway, and
the rust/py/sh modules already use `rust_rules`/`py_rules`/`sh_rules` identifiers, not
`"java"`). (b) Scope the scan to the *core* file list explicitly (the inverse of the
existing all-`crates/` scan) with the per-language modules + `ruleset_modules` as the
allowlist, matching §6. (c) Match on `ProviderTypeId::new("…")` / `FieldId::new("…")`
*call* patterns rather than bare substrings to kill the false-positive class.

### 7. Provider MERGE / DefaultInfo / determinism gaps the doc doesn't address. [nice-to-have, but name them]

- **Cross-language merge is undefined.** `Dds::assert` merges field-by-field and
  `OrderedDepset` merge is **non-commutative** (`razel-dds/src/lib.rs:128`–`162`,
  explicit `[F4]` warning). The registry (§3) makes "any provider on any target" generic,
  which makes it *easier* for two producers to assert the same `(target, ordered-field)`
  — the exact case the algebra warns is order-dependent. The doc should state the
  single-producer-per-`(target,field)` discipline as a registry invariant (the algebra
  doc punts it to "Phase C decides" — C3 *is* that decision point).
- **`DefaultInfo`'s place is ambiguous.** §2 lists `DefaultInfo` schema registration as a
  `dds.rs` leak to evict, but `DefaultInfo` is the one truly language-agnostic provider —
  every ruleset emits it (`dialect.rs:360`, all native rules set `default_info`). It
  should be a *core* schema, not a per-language registration. The doc treats it like
  `CcInfo`; it isn't. Call it out as the engine's built-in provider (registered by the
  core, in the allowlist) — otherwise C3a has no module to "own" it.
- **`neverlink` is always written** (`dialect.rs:418` writes it even when `false`), so
  every java target carries the scalar. Harmless today, but a registry-driven generic
  projection will surface `neverlink=false` into the dep struct for every java dep unless
  the projection policy distinguishes "fold field" from "control field." Worth a line.

## Accuracy check — did §2 + §4 hold against the code?

**§2 leak inventory — partially accurate, one row mislabeled, one module missing.**

| §2 claim | verdict | evidence |
|---|---|---|
| `state.rs`: `cc_provider_map` + `CcInfo`/`JavaInfo` accessors | **accurate** | `cc_provider_map` `state.rs:76`; `hdrs`/`cflags`/`compile_jars`/`runtime_jars`/`neverlink` accessors hardcode the literals `state.rs:51`–`68` |
| `engine.rs`: `command_line` matches `"cc"` → `macos_core_config` | **accurate** | `engine.rs:113`–`114` (note: the surrounding comment `engine.rs:105` already claims "no cc-hardcoding," which is false — the match arm *is* the hardcoding; the doc is right, the source comment is aspirational) |
| `dds.rs (to_dds)`: registers `DefaultInfo`/`CcInfo`/`JavaInfo` schemas + field names | **accurate** | `dds.rs:36`–`52` |
| `dialect.rs`: capture builtins + dep folds + dep-struct names + `cc_provider_map` (filegroup) | **accurate** | builtins `dialect.rs:376`,`:399`; folds `:152`–`161`; dep-struct `:163`–`168`; `filegroup` → `cc_provider_map` `:352` |
| `rules.rs`: `"cc"` ×5 is registration, NOT a core leak | **accurate split** | `ruleset_modules` `rules.rs:130`–`167`; `CcToolchainMode` wiring `rules.rs:174`; the §6 "allowed registration" classification is correct per the code |
| **MISSING: `deps.rs`** | **gap** | `deps.rs:63` hardcodes `CcInfo`; `:70` folds `hdrs`/`cflags`; this is generic machinery (used by cc/rust/py), not a registration — belongs in the table (see concern 1) |

The doc's "legitimate provider definitions vs generic machinery enumerating the language
set" split (§2 closing, §6) is the **right test**, and it applies it correctly to
`rules.rs`. It just failed to *run* the test on `deps.rs`, which fails the test (generic
machinery, hardcoded language).

**§4 toolchain-gap — accurate and well-argued.**
- "cc's toolchain is a `FeatureConfig`" — confirmed, `razel-cc-toolchain/src/lib.rs:30`,
  `:50`–`57`, `:226`.
- "java is template-shaped, no toolchain object, rides `razel_build.action`" — confirmed,
  `java_defs.bzl:40`–`69` build argv literally and never call `command_line`; cc uses
  `command_line` (`cc_defs.bzl:30`, `:47`). The asymmetry is exactly as described.
- "a generic `Toolchain` returning a command line is cc-shaped today / one-member
  abstraction" — confirmed; java would have nothing to put behind it.
- The interim resolver "still returns `FeatureConfig` (cc-coupled type)" — confirmed; the
  resolver fn signature would expose `razel_cc_toolchain::FeatureConfig` to `Session`,
  the same coupling `engine.rs:114` has, just relocated. The doc names this residual
  honestly. **Agree with deferring (i) to Phase D.**

One nuance the doc could add: the interim resolver coupling means `state.rs`/`Session`
would gain a `razel-cc-toolchain` dependency (today the cc-config crate is only reached
from `engine.rs`). That is a *new* edge in the crate graph for a known-temporary type.
Acceptable, but note it (and ensure the C3c gate's allowlist or the dds-boundary-style
check doesn't later flag it).

## Open questions / Phase-D boundary

1. **Is the registry's unit of registration the provider, or the (provider, dep-struct
   projection)?** Concern 2 argues it must be the latter. Resolve before C3a coding.
2. **Where does the generic capture path (`razel_build.info`, §10 C2) actually land?** If
   C2 didn't ship it (concern 3), C3a should, or C3 inherits two hardcoded builtins it
   claims to have removed.
3. **Does `deps.rs`'s native-rule dep path get the same registry treatment as the dialect
   path, or do they unify?** Two folds, two call sites (`deps.rs:63`, `dialect.rs:135`).
   The cleanest end state is one registry-driven projection used by both; the doc should
   say whether C3a unifies them or leaves the native path as-is (in which case the gate
   would still trip on `deps.rs`).
4. **Action-transform seam: Phase D, not C3** (concern 5) — confirm the C3 deliverable is
   prose documentation of the seam, not a registration API.
5. **Cross-language `OrderedDepset` merge discipline** — C3 is the decision point the
   algebra deferred (`razel-dds/src/lib.rs:131`). State the single-producer invariant.

### Alignment with the north star

The seam-as-registration goal is squarely declarative-first: a language becomes a row in
`ruleset_modules` + a registry population, not edits to shared control flow. That is the
right shape and the `rust`/`py`/`sh` modules prove the registration path already mostly
works for *action* emission. The gap is that **provider flow** is not yet registration —
it is two hardcoded folds (`deps.rs`, `dialect.rs`) plus a py hack on the cc channel. C3a
done *thoroughly* (both paths, the projection mapping, DefaultInfo as a core provider)
delivers the north star; C3a done as the doc's "mechanical name move" leaves the seam
half-open and the gate (C3c) would expose it. Simpler design available? Not materially —
the registry is the minimal mechanism. The simplification is in *scope honesty*: drop the
action-transform API (concern 5) and the bare-substring gate tokens (concern 6), and add
the two missing leak sites. No grand redesign needed.
