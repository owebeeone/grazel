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

mod eval; // P1.3: evaluate an Expr over the query graph → a label set
mod graph; // P1.3: the query graph (own adjacency over §11 edges) + deps/rdeps/patterns
mod output; // P1.4: --output=label / label_kind formatting
mod parse; // P1.1: the §12 expression parser → AST

pub use eval::eval;
pub use graph::{LabelSet, QueryGraph};
pub use output::{Output, format};
pub use parse::{Expr, parse};

// P1.5+ land path operators (somepath/allpaths) and the cmd_query entry.
