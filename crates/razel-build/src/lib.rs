//! The build driver — bridges analysis → action → execution (Phase 3 ↔ 5 integration).
//!
//! Takes a Starlark-defined target through the whole stack: `analyze_starlark` runs the
//! rule impl and captures its actions, each action is converted to a content-addressed
//! `razel_actions::Action`, and `razel_exec` runs it in the exec root (cache hit → 0 exec).
//! This is the first point where razel **actually builds** from a rule.
//!
//! SCOPE: single package, inputs assumed materialized in the exec root; no toolchain
//! selection / `define_config` sugar yet (the rule emits argv directly via `ctx.actions.run`).
//! That + the link-with-deps cross-target flow are the next increments (D7).

pub mod incremental;
pub use incremental::IncrementalBuilder;

use razel_actions::Action;
use razel_analysis::wire_to_ir;
use razel_core::{Digest, FileId, TargetId};
use razel_exec::{Cache, build_action};
use razel_ir::TargetKind;
use razel_loading::{
    analyze_bazel_with, analyze_starlark, analyze_workspace_resolved, load_tree_report_with_targets,
};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::{Condvar, Mutex};
// Re-exported so the daemon/clients can hold warm analysis (the analyze/execute split).
pub use razel_loading::{
    AnalyzedTarget, GlobalFlags, args, bazel_flags, config_segment, convenience_symlinks,
    resolve_build_file,
};


mod drive;
mod exec_root;
mod affected;
#[cfg(test)]
mod tests1;
#[cfg(test)]
mod tests2;

pub use {drive::*, exec_root::*, affected::*};
