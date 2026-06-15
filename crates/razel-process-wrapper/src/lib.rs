//! `razel-process-wrapper` — the exec-time process wrapper (rules_rust `process_wrapper`
//! analogue). The executor invokes it (by path, like `rustc`) to wrap subprocess actions whose
//! contract the bare command can't express.
//!
//! Subcommands:
//! - `build-script` (P3.8 §5.2): run a cargo build script with a default-deny env, capture its
//!   stdout, parse the §6.1 directives, write the JSONL flags file + OUT_DIR. The WRITER.
//! - `rustc` (P3.9, §6.1): apply a flags file to a rustc invocation (`--cfg`/`-l`/`-L`/`-C
//!   link-arg`/env). The READER. — not yet implemented.
//!
//! The shared [`flags`] module is the single definition of the flags-file schema both ends use.

pub mod bs_runner;
pub mod flags;
