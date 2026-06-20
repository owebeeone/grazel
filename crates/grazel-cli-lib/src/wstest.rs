//! `grazel ws test` — the workstream's acceptance instrument (GrazelWorkstream §1).
//!
//! Stages exercise the REAL binary end-to-end (CLI → scope resolution → socket →
//! daemon → wire → back); a stage that could pass against a mock is wrongly
//! written. The same ladder runs as the shipped verb and in-process under
//! `cargo test` (grazel-cli's integration test) — only `StageCtx.grazel_bin`
//! differs (`current_exe` vs `CARGO_BIN_EXE_grazel`).



mod run_stages;
mod build_parity;
mod pid_alive;
mod daemon_status;
mod members_of;
mod test_verb_protocol;
mod gryth_examples_corpus;
mod http_localhost_only;
mod impl_drop_for_reap;

pub use run_stages::*;
pub use build_parity::*;
// These hold only crate-internal helpers (the stage ladder reaches them via `use super::*`);
// re-export at pub(crate) — nothing here is part of the public `wstest` surface.
pub(crate) use pid_alive::*;
pub(crate) use daemon_status::*;
pub(crate) use members_of::*;
pub(crate) use test_verb_protocol::*;
pub(crate) use gryth_examples_corpus::*;
pub(crate) use http_localhost_only::*;
