//! `cargo xtask` — project automation.
//!
//! `codegen`         regenerate `crates/razel-wire/src/generated.rs` from the taut IR
//! `codegen --check` fail (exit 2) if the committed output is stale vs. the IR
//!
//! Generation shells out to `tautc` (the Python taut codegen CLI) — deliberately
//! kept OUT of `cargo build` so the crate graph stays pure-Rust and offline. The
//! command is `tautc` by default; override with `$TAUTC` (e.g. for an uninstalled
//! checkout: `TAUTC="python3 -m taut.cli" PYTHONPATH=../taut/src cargo xtask codegen`).

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
    match args.next().as_deref() {
        Some("codegen") => {
            let check = args.next().as_deref() == Some("--check");
            codegen(check)
        }
        other => {
            eprintln!("unknown xtask {other:?}\nusage: cargo xtask codegen [--check]");
            ExitCode::from(64)
        }
    }
}

fn codegen(check: bool) -> ExitCode {
    let root = workspace_root();
    let schema = root.join("crates/razel-wire/wire/razel.taut.py");
    let target = root.join("crates/razel-wire/src/generated.rs");
    let out_dir = std::env::temp_dir().join("razel-wire-codegen");
    let _ = std::fs::remove_dir_all(&out_dir);

    // `tautc gen <schema> -o <tmp> -l rust --api-only`  (override binary via $TAUTC)
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
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("tautc failed: {s}");
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!(
                "could not run tautc ({tautc:?}): {e}\nis the taut tool installed? (pip install -e ../taut)"
            );
            return ExitCode::FAILURE;
        }
    }

    // Format the raw tautc output with rustfmt so the committed file is both
    // repo-consistent and drift-stable: `cargo fmt` can't change it (already
    // formatted) and `--check` compares formatted-to-formatted. rustfmt is
    // deterministic, so regeneration reproduces byte-identical output.
    let raw = out_dir.join("rust/api.rs");
    let fmt = Command::new("rustfmt")
        .args(["--edition", "2024"])
        .arg(&raw)
        .status();
    match fmt {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("rustfmt failed: {s}");
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("could not run rustfmt: {e}");
            return ExitCode::FAILURE;
        }
    }
    let fresh = std::fs::read_to_string(&raw).expect("read generated api.rs");

    if check {
        let committed = std::fs::read_to_string(&target).unwrap_or_default();
        if committed == fresh {
            println!("codegen: generated.rs is up to date with the IR");
            ExitCode::SUCCESS
        } else {
            eprintln!(
                "codegen DRIFT: {} is stale vs {}.\n  run `cargo xtask codegen` and commit.",
                target.display(),
                schema.display()
            );
            ExitCode::from(2)
        }
    } else {
        std::fs::write(&target, fresh).expect("write generated.rs");
        println!("codegen: wrote {}", target.display());
        ExitCode::SUCCESS
    }
}
