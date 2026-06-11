# RazelFetchPlan — external fetch + cache (the resource-wall killer)

*2026-06-11, round 35 design (stabilization lane). Decision (Gianni, this date): razel cannot
be Bazel-compatible without fetch — implement fetch + cache now; razel's cache goes NEXT TO
Bazel's, never into it; a central shared cache (iroh-shaped) is a later mitigation, not a
blocker. Supersedes the per-repo vendor-grant loop for everything tf_http_archive-shaped;
the L6a hand-vendored posture remains for config-GENERATED repos and as an override layer.*

## §1 Why now (the census)

Round 33's full-error census: of 514 failing packages, ~60% are unvendored-repo walls and
another 6% are *unfaithful* vendors (flatbuffers: we vendored raw upstream; TF's repo rule
overlays its own `build_defs.bzl` via `link_files` — a bug class that exists ONLY because
hand-vendoring reproduces repo rules by hand). Fetch converts the failure table into pure
engine truth and ends the grant-per-repo loop.

## §2 What Bazel actually does here (the compatibility target)

For this TF snapshot the WORKSPACE chain is authoritative (MODULE.bazel exists but the repo
set is declared via `workspace0–3.bzl`; bzlmod is a later arc). The unit is
`tf_http_archive` (`third_party/repo.bzl`): **pure data** — `urls` (mirror-first), `sha256`,
`strip_prefix`, `patch_file[]`, `build_file`, `link_files{}`, `system_*` variants. Bazel
executes it as a repository_rule against a content-addressed download cache
(`<output_user_root>/cache/repos/v1/content_addressable/sha256/<hash>/file`).

## §3 Architecture: extractor → fetcher → materializer (NOT the L7 subsystem)

**R1 — spec extraction (no network).** razel already evaluates Starlark: eval TF's
`WORKSPACE` (it IS Starlark) with `repository_rule` bound to a RECORDER — evaluating the
REAL `repo.bzl`, so `tf_http_archive`'s own mirror/patch post-processing runs and every
instantiation records `(rule kind, name, kwargs)`. Output: `razel-repos.lock.json` (the
lockfile) + a dry-run report (repo count, patch inventory, unfetchable/unknown-shape repos
— each a NAMED hole, which is the point). Engine-side: a pub extraction entry in
razel-loading; absorbing bindings for workspace-only surface (`register_toolchains` etc.)
surfaced by the probe loop, loudly.

**R2 — fetcher + download cache.** Sha-pinned download (curl shell-out MVP; mirror-first,
fail → next URL), verify sha256, store content-addressed. **Only the top-level root is
razel's own — everything BELOW it mirrors Bazel's layout byte-for-byte** (decision, Gianni
2026-06-11: so a razel fetch and a Bazel fetch of the same workspace are directly
`diff -r`-comparable; also makes a future iroh/central layer or a read-Bazel's-cache
fallback a path change, not a redesign):
- root: `/private/var/tmp/_razel_<user>/` (macOS) / `~/.cache/razel/_razel_<user>/` (Linux)
  — SIBLING to `_bazel_<user>`, NEVER writing under it.
- download cache: `<root>/cache/repos/v1/content_addressable/sha256/<hash>/file`
  (Bazel's exact schema).
- output base: `<root>/<md5(workspace-path)>/` (Bazel's workspace-hash scheme).
AD2: roots threaded via flags/config, no ambient globals.

**R3 — materializer.** Extract (tar.gz/zip), `strip_prefix`, apply `patch_file[]`
(`patch -p1` MVP), write `build_file` as BUILD, apply `link_files` copies → materialized
repo at `<output base>/external/<name>/` — Bazel's exact shape, so
`diff -r _bazel_<u>/<h>/external/curl _razel_<u>/<h>/external/curl` answers "did razel
materialize what Bazel would" with no tooling. The Session's external resolution gains a
second base: **hand-vendored `third-party/` wins over fetched** (curated overlays keep
working; retire them deliberately, not by collision). The flatbuffers class dies by
construction here — link_files is mechanical.

**R4 — the re-based census.** Full sweep against the materialized closure; the failure
table becomes engine-capability truth. Re-size eager-select / py chains / native-global
classes on the bigger corpus; re-run the seeding experiment behind deferred-select.

## §4 Explicitly out (posture unchanged)

- `repository_ctx.execute` / autoconf repos (`@local_config_*`, python configure): host-
  materialized stubs remain (round-24 posture). L7.
- bzlmod/MODULE resolution: later arc (the snapshot's WORKSPACE chain is authoritative).
- No Bazel-cache writes; no cache sharing until the central-cache (iroh) design exists.

## §5 Risks, ranked

1. **Workspace-eval surface** — WORKSPACE-only natives/loads razel hasn't met; mitigated by
   the recorder running the real repo.bzl + loud absorbs + the dry-run report listing every
   unrecorded repo shape.
2. **Patch/archive tail** — odd formats, .zip vs .tar.*, patch fuzz; per-repo failures stay
   named holes, never silent.
3. **Disk** — TF's closure is GBs; content-addressed cache is one-time, materialized repos
   are per-spec-hash (GC later).
4. **Supply chain** — downloads are sha256-pinned from TF's own lockfile-equivalent specs;
   a mismatch is a hard fail.

## §6 Acceptance

R1: lockfile covers ≥ the census's named repos (@curl/@pypi/@ruy/@stablehlo/@FP16/@riegeli/
@gemmlowp/…) + dry-run report banked. R2: cache hit/miss + sha-verify round-trips, offline
re-run = full cache hits. R3: a previously-unvendored repo (e.g. @gemmlowp) materializes and
its 6-package class dies with zero hand-vendoring. R4: sweep + census re-base; suite + gates
+ sentinels + rungolds green throughout; sweeps backgrounded.
