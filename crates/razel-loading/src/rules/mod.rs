//! Starlark-defined rules + analysis (Phase 3): `rule(implementation, attrs)` returns a
//! callable custom value; instantiating it runs the rule **implementation** with a `ctx`
//! (Bazel dialect: `ctx.attr.*`, `ctx.label`, `ctx.actions.declare_file/run/write`) and
//! captures the registered actions (inputs/outputs) and `DefaultInfo` — the target
//! **analyzes**. Plus `select()` (host-config-lite) and `DefaultInfo`.
//!
//! Analysis runs in the **same eval scope** as instantiation (the impl `Value` never
//! escapes the heap) — sidestepping module freezing. Tier-2.5 simplification; a two-phase
//! freeze model comes when caching / cross-target dep-providers demand it.

mod globals;
mod load;
mod eval;
mod pkg;
mod tree;
#[cfg(test)]
mod tests;

// `pub use` (not pub(crate)) so the glob caps each item at its OWN visibility — the items
// `lib.rs` re-exports publicly (analyze_*, load_tree_report*, …) stay `pub`; the rest stay
// `pub(crate)`. A glob re-export caps per-item, so this never over-exposes. `load` exports
// only pub(crate) items, so it's re-exported pub(crate) (a `pub use load::*` would warn that
// nothing is public enough).
pub use {globals::*, eval::*, pkg::*, tree::*};
pub(crate) use load::*;
