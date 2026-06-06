//! Class-A conformance dashboard: run every `.star` under a directory through the
//! golden runner and report against the divergence manifest (the project's status doc).
//!
//! Usage: `dashboard <dir>` (e.g. Bazel's `.../net/starlark/java/eval/testdata`).
//! Classes B/C (the `Razel` e2e driver) are not wired yet — reported as such, honestly.

use razel_conformance::{evaluate_gate, expected_divergences, run_star_source};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn collect_star(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_star(&p, out);
            } else if p.extension().is_some_and(|x| x == "star") {
                out.push(p);
            }
        }
    }
}

fn main() -> ExitCode {
    let dir = std::env::args()
        .nth(1)
        .expect("usage: dashboard <dir-with-.star-files>");
    let mut files = Vec::new();
    collect_star(Path::new(&dir), &mut files);
    files.sort();

    let mut per_file = Vec::new();
    println!("# Class A — Starlark golden conformance\n");
    println!("| file | pass/total | quarantined | status |");
    println!("|---|---|---|---|");
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap_or_default();
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let rep = run_star_source(&name, &src);
        let (p, t) = (rep.passed(), rep.total());
        let fail = t - p;
        let exp = expected_divergences(&name);
        let status = if fail > exp {
            "❌ REGRESSION"
        } else if fail < exp {
            "⚠ stale"
        } else if exp > 0 {
            "quarantined"
        } else {
            "✅"
        };
        println!("| {name} | {p}/{t} | {} | {status} |", exp.min(fail));
        per_file.push((name, p, t));
    }

    let g = evaluate_gate(&per_file);
    println!(
        "\n**Class A: raw {}/{} ({:.1}%); {} quarantined (documented divergences); \
         non-quarantined {}/{} ({:.1}%).**",
        g.raw_pass,
        g.total,
        g.raw_pct(),
        g.quarantined,
        g.raw_pass,
        g.nonquarantined_total(),
        g.nonquarantined_pct(),
    );
    for r in &g.regressions {
        println!("- ❌ {r}");
    }
    for s in &g.stale {
        println!("- ⚠ {s}");
    }
    println!("**Class B (Buck2 e2e): not wired (Razel driver — later phase).**");
    println!("**Class C (example projects): not wired (later phase).**");

    let gate = g.passed();
    println!(
        "\n**Phase 1 gate (≥95% non-quarantined, no regressions): {}**",
        if gate { "PASS ✅" } else { "FAIL ❌" }
    );
    if gate {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
