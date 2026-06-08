# RazelCcRules — folded into RazelStarlarkBoundaryPlan

This spec is now **§5–§6 of [`RazelStarlarkBoundaryPlan.md`](./RazelStarlarkBoundaryPlan.md)**,
where cc is the *first instance* of the generic `razel_build` engine (rather than a one-off):

- `razel_cc` = `razel_build` specialized to cc's (richest) feature config — §5/§6.
- razel ships its **own** `cc:defs.bzl` over it, **bundled in the binary** (§7), exposing the
  public rule API + provider contract (`CcInfo`/`DefaultInfo`/`CcToolchainInfo`) — §4.
- Backed by the parity-proven primitives (`Constrain` · derive · producer/`fold_set` · path model),
  with the config *evaluated* from the real `cc_toolchain_config_lib.bzl` (retiring the hand-port).

See the boundary plan for the unified model, the verified per-language bucketing, the bundling
decision, and the build sequence.
