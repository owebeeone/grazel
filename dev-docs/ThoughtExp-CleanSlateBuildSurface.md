# Thought Experiment — a clean-slate build surface (PROMPT, not for now)

*Recorded as a prompt for a future session/agent. Do not explore inline now.*

## The prompt

The Fundamentals (`ArchFundamentals.md`, F1–F23) are **surface-agnostic** — they say
what any build engine must do. The Bazel input surface (`ArchBazelConstraints.md`,
C1–C9) is just **one encoding** of the input data, kept only because we chose
compatibility.

> **If you did *not* have to be Bazel-compatible — if you were designing the "makefile"
> from scratch over the same engine — what would the input surface look like?**

Strip away the compatibility constraint and ask: what is the *minimal, best* way to
encode "these are my targets, their typed dependencies, their inputs, and how to build
them," such that the engine still gets F1–F23?

## What to separate first (essential vs accidental in BUILD/.bzl)

- **Essential (the data):** targets; typed dependency edges; declared inputs/outputs;
  the build steps (actions) and their toolchains; configuration variation; the
  producer→consumer contract (providers).
- **Plausibly accidental (the encoding):** that the surface is a *Turing-ish interpreted
  language* (Starlark) at all; the `rule()`/`ctx`/`provider()` authoring ceremony; macros;
  untyped configs and `select` strings; the BUILD-per-directory packaging; the
  two-tier "BUILD instantiates, `.bzl` defines" split.

## Directions to weigh (questions, not answers)

1. **Do you need an interpreter, or is most of it data?** How much real *computation* do
   BUILD files do vs. just *declare*? If 95% is declaration, a **typed declarative data
   model** (schema-validated) replaces the interpreter for that 95% — and you get F4/F11
   (typed identity, decoupling) *for free, by construction*. This is exactly the
   project's north star: **declarative discipline; config-as-data; `.glade` as a
   de-noising lens** — the build surface as a pure declarative artifact with the noise
   removed.
2. **Where computation is genuinely needed** (computed deps, conditionals, fan-out): what
   is the *smallest* computation that suffices — a constrained, total expression language
   over the data, rather than a general interpreter? (Calibrated indirection: the
   interpreter is the ultimate relief valve; do you actually need its full power, or a
   slice?)
3. **Typed from the start.** Attributes/providers as a declared schema, not stringly-typed
   `select` keys and untyped structs — sidestepping Bazel's most expensive scar.
4. **Is the surface even primary, or a projection?** If the engine consumes a clean IR,
   the IR *is* the real language; the human surface could be one **view/projection** of
   it (and Bazel's BUILD/.bzl another). What's the canonical IR, and what surfaces project
   to/from it?
5. **Packaging & naming** without the directory-`BUILD` coupling — what's the minimal
   identity/visibility model?

## Invariants the experiment may NOT relax

F1–F23 still hold — only the *surface* changes. The exercise is "re-encode the same input
data for the same engine," not "build a different engine." A good outcome is a surface
that satisfies the Fundamentals more *directly* (fewer relief valves spent, more
properties true by construction) than Bazel's surface does.

## Why it matters even though we're not doing it

It pressure-tests the front-end/engine seam (`ArchBazelConstraints.md`): if the engine is
truly surface-agnostic, this experiment is *possible* — the Bazel surface becomes a
swappable front-end. If the experiment feels impossible, the seam is leaking and the
architecture has absorbed Bazel-isms it shouldn't have. So this prompt doubles as a
**litmus for the seam's cleanliness**, regardless of whether the alternate surface is
ever built.

*Compare any future answer against the `glade`/`.glade` declarative model
(`dev-docs/glade/*`, `StackMap.md`).*
