//! Per-analysis state + core types + the host-cc tool layer (C0 decomposition of rules.rs).
//! The foundation every other loader module imports. AD2: state is a fresh `Session` per
//! analyze_*, threaded explicitly — no ambient globals.

mod types;
mod session;
mod sched;
mod wait;
mod flags;
mod paths;
mod tools;
#[cfg(test)]
mod tests;

pub use {types::*, wait::*, flags::*, paths::*};
pub(crate) use {session::*, sched::*, tools::*};
