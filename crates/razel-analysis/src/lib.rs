//! Analysis (Phase 3): wire Starlark-rule analysis results into the IR.
//!
//! Live surface: [`analysis::wire_to_ir`] (`razel_loading::analyze_starlark` →
//! `razel_ir::Graph`), consumed by `razel-build`. The earlier built-in-rule
//! `analyze`/`TargetDecl` pipeline and the standalone `Depset<T>` nested-set (ported Bazel
//! `NestedSet` order semantics, never wired to the live path) were removed in Phase 1 — V2
//! rebuilds ordered-set semantics as the DDS `OrderedDepset` monoid (RazelV2Contracts §3).

pub mod analysis;
pub mod producer;
pub use analysis::{wire_to_dds, wire_to_ir};
pub use producer::{CcLibrary, Producer, ProducerCtx, TargetAttrs, assemble};
