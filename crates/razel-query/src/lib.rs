//! `razel-query` — the `razel query` engine (RazelCrateUniverseDesign Part B; plan Phase 1).
//!
//! A subset of `bazel query`'s expression language evaluated over the **§11 loading-phase graph**
//! (`razel_loading`'s `LoadedTarget`/`QueryNode`/`Edge`). It reads that graph and **never triggers
//! analysis** — the boundary that keeps query fast and `bazel query`-faithful. That boundary is
//! mechanical: this crate depends on `razel-loading` + leaf utilities only, NEVER on
//! `razel-build`/`razel-analysis`/`razel-exec`/`razel-engine` (enforced by `cargo xtask gates`).
//!
//! Built across the plan steps: P1.1 parser → P1.2 load-only resolver → P1.3 adjacency + evaluator
//! → P1.4 predicates/output → P1.5 somepath/allpaths → P1.6 `cmd_query` → P1.7 `qg` goldens.

// P1.1+ land the modules here (parse, graph, eval, output).
