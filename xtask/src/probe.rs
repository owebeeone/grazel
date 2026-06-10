//! `cargo xtask probe` — the deterministic ladder runner (RazelV3Plan §5). Each probe is one
//! corpus-shaped BUILD evaluated against the engine (vendored externals from `../third-party`);
//! the output is the first failure per probe, CLASSIFIED — the supervisor's ticket generator.
//! Probes that have gone green are marked `must_pass` and become regression sentinels (the probe
//! exits non-zero only if one of those fails; ladder frontiers are EXPECTED to fail).

use razel_loading::{GlobalFlags, analyze_workspace_with};
use std::path::PathBuf;
use std::process::ExitCode;

struct Probe {
    name: &'static str,
    rung: &'static str,
    build: &'static str,
    /// Green rungs guard against regression; frontier rungs report their first failure.
    must_pass: bool,
}

const PROBES: &[Probe] = &[
    Probe {
        name: "skylib-paths",
        rung: "L2",
        build: "load(\"@bazel_skylib//lib:paths.bzl\", \"paths\")\n\
                filegroup(name = paths.basename(\"x/leaf.o\"), srcs = [])\n",
        must_pass: true,
    },
    Probe {
        name: "skylib-common-settings",
        rung: "L2",
        build: "load(\"@bazel_skylib//rules:common_settings.bzl\", \"BuildSettingInfo\")\n\
                filegroup(name = \"x\", srcs = [])\n",
        must_pass: true,
    },
    Probe {
        name: "rules-rust-load",
        rung: "L2",
        build: "load(\"@rules_rust//rust/private:rust.bzl\", \"rust_library\")\n\
                filegroup(name = \"x\", srcs = [])\n",
        must_pass: false,
    },
    Probe {
        name: "rules-rust-library",
        rung: "L2",
        build: "load(\"@rules_rust//rust/private:rust.bzl\", \"rust_library\")\n\
                rust_library(name = \"hello\", srcs = [\"lib.rs\"])\n",
        must_pass: false,
    },
];

/// Classify a first-failure message into the ticket taxonomy (RazelV3Plan §5).
fn classify(err: &str) -> &'static str {
    if err.contains("external repo file not found")
        || err.contains("cannot read")
        || err.contains("needs an external base")
    {
        "resource"
    } else if err.contains("not found") && err.contains("Variable") {
        "missing-global"
    } else if err.contains("no symbol") {
        "missing-export"
    } else if err.contains("no attribute")
        || err.contains("not supported on type")
        || err.contains("Object of type")
    {
        "missing-member"
    } else {
        "semantic"
    }
}

/// First line of an error, compacted for the one-line report.
fn first_line(err: &str) -> String {
    err.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string()
}

pub(crate) fn probe(workspace_root: PathBuf) -> ExitCode {
    let third_party = workspace_root.parent().map(|p| p.join("third-party"));
    let mut sentinel_failures = 0;
    for p in PROBES {
        let ws = std::env::temp_dir().join(format!("razel-probe-{}-{}", p.name, std::process::id()));
        let pkg = ws.join("app");
        if std::fs::create_dir_all(&pkg).is_err() {
            eprintln!("PROBE {} [{}]: FAIL(harness) — cannot create temp workspace", p.name, p.rung);
            sentinel_failures += 1;
            continue;
        }
        let _ = std::fs::write(pkg.join("BUILD"), p.build);
        let flags = GlobalFlags { external_base: third_party.clone(), ..Default::default() };
        // Probe targets are named for what the BUILD declares; analyze the package's first target.
        let result = analyze_workspace_with(&ws, "//app:x", flags);
        let _ = std::fs::remove_dir_all(&ws);
        match result {
            Ok(_) => eprintln!("PROBE {} [{}]: OK", p.name, p.rung),
            Err(e) => {
                let class = classify(&e);
                eprintln!("PROBE {} [{}]: FAIL({class}) — {}", p.name, p.rung, first_line(&e));
                if p.must_pass {
                    sentinel_failures += 1;
                }
            }
        }
    }
    if sentinel_failures > 0 {
        eprintln!("\nxtask probe: FAIL — {sentinel_failures} green-rung sentinel(s) regressed.");
        return ExitCode::from(1);
    }
    eprintln!("\nxtask probe: OK — sentinels green; frontier failures above are the ticket feed.");
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_the_ticket_taxonomy() {
        assert_eq!(classify("external repo file not found for `@rules_cc//...`"), "resource");
        assert_eq!(classify("error: Variable `provider` not found"), "missing-global");
        assert_eq!(classify("Module has no symbol `CcInfo`"), "missing-export");
        assert_eq!(classify("error: Object of type `struct` has no attribute `foo`"), "missing-member");
        assert_eq!(classify("dependency cycle detected at `a`"), "semantic");
    }
}
