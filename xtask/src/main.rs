//! `cargo xtask` — project automation.
//!
//! `codegen [--check]`  regenerate `crates/razel-wire/src/generated.rs` from the
//!                      taut IR (types + CBOR codec)
//! `corpus  [--check]`  regenerate `crates/razel-wire/src/vectors.rs` — the golden
//!                      cross-language corpus, encoded by taut's Python codec
//!
//! `--check` fails (exit 2) if the committed output is stale. Both shell out to
//! Python (`tautc` / `corpus.py`) — deliberately kept OUT of `cargo build` so the
//! crate graph stays pure-Rust and offline — then run the output through rustfmt
//! so the committed file is repo-consistent AND drift-stable (regeneration is
//! deterministic, so committed == freshly-formatted).
//!
//! Tool resolution is via env, with the caller supplying `taut` on PYTHONPATH:
//!   TAUTC   codegen CLI       (default `tautc`)
//!   PYTHON  interpreter       (default `python3`, used for corpus.py)
//! e.g. `TAUTC="python3 -m taut.cli" PYTHONPATH=../taut/src cargo xtask codegen`
//!      `PYTHONPATH=../taut/src cargo xtask corpus --check`

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <root>/xtask
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let cmd = args.next();
    let check = args.next().as_deref() == Some("--check");
    match cmd.as_deref() {
        Some("codegen") => codegen(check),
        Some("corpus") => corpus(check),
        Some("flags") => flags(check),
        other => {
            eprintln!(
                "unknown xtask {other:?}\nusage: cargo xtask <codegen|corpus|flags> [--check]"
            );
            ExitCode::from(64)
        }
    }
}

/// Regenerate the Bazel-flag recognition table (`razel-cli/src/bazel_flags.rs`) from
/// the committed JSON inventory via `gen_flags_table.py --stdout`. To refresh the
/// inventory itself against a newer Bazel, run `extract_bazel_flags.py <bazel-src>`
/// and diff with `--diff` (the version-comparison oracle).
fn flags(check: bool) -> ExitCode {
    let root = workspace_root();
    let script = root.join("crates/razel-cli/flags/gen_flags_table.py");
    let target = root.join("crates/razel-cli/src/bazel_flags.rs");
    let raw = std::env::temp_dir().join("razel-bazel-flags.rs");

    let python = std::env::var("PYTHON").unwrap_or_else(|_| "python3".into());
    let out = Command::new(&python).arg(&script).arg("--stdout").output();
    let out = match out {
        Ok(o) if o.status.success() => o.stdout,
        Ok(o) => {
            eprintln!(
                "gen_flags_table.py failed: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("could not run {python:?} on gen_flags_table.py: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = std::fs::write(&raw, out) {
        eprintln!("could not write temp flags table: {e}");
        return ExitCode::FAILURE;
    }
    finalize(&raw, &target, "flags", check)
}

/// Regenerate the wire types + codec from the taut IR via `tautc`.
fn codegen(check: bool) -> ExitCode {
    let root = workspace_root();
    let schema = root.join("crates/razel-wire/wire/razel.taut.py");
    let target = root.join("crates/razel-wire/src/generated.rs");
    let out_dir = std::env::temp_dir().join("razel-wire-codegen");
    let _ = std::fs::remove_dir_all(&out_dir);

    let tautc = std::env::var("TAUTC").unwrap_or_else(|_| "tautc".into());
    let mut parts = tautc.split_whitespace();
    let prog = parts.next().expect("empty $TAUTC");
    let status = Command::new(prog)
        .args(parts)
        .arg("gen")
        .arg(&schema)
        .arg("-o")
        .arg(&out_dir)
        .args(["-l", "rust", "--api-only"])
        .status();
    if let Err(code) = ok_status(
        status,
        &tautc,
        "is taut installed? (pip install -e ../taut)",
    ) {
        return code;
    }
    finalize(&out_dir.join("rust/api.rs"), &target, "codegen", check)
}

/// Regenerate the golden corpus via `corpus.py --stdout`.
fn corpus(check: bool) -> ExitCode {
    let root = workspace_root();
    let script = root.join("crates/razel-wire/wire/corpus.py");
    let target = root.join("crates/razel-wire/src/vectors.rs");
    let raw = std::env::temp_dir().join("razel-wire-vectors.rs");

    let python = std::env::var("PYTHON").unwrap_or_else(|_| "python3".into());
    let out = Command::new(&python).arg(&script).arg("--stdout").output();
    let out = match out {
        Ok(o) if o.status.success() => o.stdout,
        Ok(o) => {
            eprintln!("corpus.py failed: {}", String::from_utf8_lossy(&o.stderr));
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!(
                "could not run {python:?} on corpus.py: {e}\n  PYTHONPATH must include taut (PYTHONPATH=../taut/src)"
            );
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = std::fs::write(&raw, out) {
        eprintln!("could not write temp corpus: {e}");
        return ExitCode::FAILURE;
    }
    finalize(&raw, &target, "corpus", check)
}

/// rustfmt `raw` in place, then compare to / overwrite `target`.
fn finalize(raw: &Path, target: &Path, label: &str, check: bool) -> ExitCode {
    let fmt = Command::new("rustfmt")
        .args(["--edition", "2024"])
        .arg(raw)
        .status();
    if let Err(code) = ok_status(fmt, &"rustfmt".into(), "is rustfmt installed?") {
        return code;
    }
    let fresh = std::fs::read_to_string(raw).expect("read generated output");

    if check {
        let committed = std::fs::read_to_string(target).unwrap_or_default();
        if committed == fresh {
            println!("{label}: {} is up to date", target.display());
            ExitCode::SUCCESS
        } else {
            eprintln!(
                "{label} DRIFT: {} is stale.\n  run `cargo xtask {label}` and commit.",
                target.display()
            );
            ExitCode::from(2)
        }
    } else {
        std::fs::write(target, fresh).expect("write generated output");
        println!("{label}: wrote {}", target.display());
        ExitCode::SUCCESS
    }
}

/// Map a spawned command's status to Ok / an ExitCode to bubble up.
fn ok_status(
    status: std::io::Result<std::process::ExitStatus>,
    prog: &String,
    hint: &str,
) -> Result<(), ExitCode> {
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => {
            eprintln!("{prog} failed: {s}");
            Err(ExitCode::FAILURE)
        }
        Err(e) => {
            eprintln!("could not run {prog:?}: {e}\n  {hint}");
            Err(ExitCode::FAILURE)
        }
    }
}
