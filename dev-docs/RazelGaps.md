# RazelGaps — unplanned work (backlog)

A running collection of things razel will need that are **not on a phase plan** (no roll-build slot
yet), surfaced during development. Promote an item into a plan (e.g. `RazelStarlarkBoundaryPlan.md`
§10) when it's scheduled. Keep entries actionable: what, why, and the known specifics.

## bazelrc / razelrc processing

razel must eat both `.bazelrc` (Bazel-compatible) and `.razelrc` (razel extensions). Today only the
`--bazelrc` *flag name* is recognized (`razel-cli/src/bazel_flags.rs`); there is **no rc-file parsing**.

- **Format:** `<command> <args>` lines (`build`, `test`, `common`, `always`); `import` /
  `try-import <file>`; `#` comments; line continuations; `--config=X` → expand the `<command>:X` lines.
- **Locations / precedence:** system → workspace `.bazelrc` → home `~/.bazelrc` → `--bazelrc=`
  overrides, lines accumulating in order.
- **`.razelrc` layering:** read `.bazelrc` first (compat), then `.razelrc` last (overrides + carries
  razel-only flags Bazel would reject) — so a project keeps its working `.bazelrc` and adds razel-isms
  separately.
- **Unknown-flag tolerance:** the `bazel_flags` table already exists so razel *recognizes* Bazel flags
  it doesn't act on (no hard error) — that's what makes eating a real `.bazelrc` safe.
- **Toolchain hook:** rc settings select the toolchain (native vs adopt-Bazel) **per language**
  (cc, py, rust) — see the toolchain item below + `RazelStarlarkBoundaryPlan.md` §7.

## Toolchain-change cache invalidation

Build-correctness requirement: when the build tool changes (e.g. `rustc`/`clang` is updated), the
actions that used it MUST re-run. The tool is an **input**; its **content digest** belongs in the
action's cache key.

- **Key on content, not timestamp.** `(size, mtime)` is a fast "did it change?" stat-proxy (to skip
  re-hashing a 300 MB `rustc` every build); the *key* is the content digest. Timestamp-as-key is
  unsafe both ways — misses mtime-preserving updates (→ stale/wrong output) and fires on `touch`.
- **Per-action, precise:** a new tool digest → cache miss only for actions that had that tool as an
  input — not a global flush.
- **Native toolchains especially:** they're *non-hermetic* — the host `rustc`/`clang` can change
  underneath you with no signal, so capturing the resolved tool's digest per build is what keeps
  native builds correct. Hermetic (downloaded) toolchains are pinned → safe by construction (an update
  is a new pin = a new key anyway).
- **razel gap:** the digest infra exists (`razel_ir::FileNode.digest`, `content_key`), but the action
  key today is description/path-based (`mnemonic | input-paths | output-paths`) — it does **not** fold
  in input *contents*, incl. the tool. Needs: (1) the resolved toolchain tool tracked as an input,
  (2) its digest folded into the action key (mtime/size as the stat fast-path). Belongs with toolchain
  resolution.
- This is *why* Bazel lists toolchain files (`cc_wrapper.sh`, the `rustc` binaries) as action inputs —
  the cache-invalidation mechanism, not just sandbox detail (cf. `RazelStarlarkBoundaryPlan.md` §8).
