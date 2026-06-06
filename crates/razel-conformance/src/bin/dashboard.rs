//! Class-A conformance dashboard: run every `.star` under a directory through the
//! golden runner and print a pass-rate report.
//!
//! Usage: `dashboard <dir>` (e.g. Bazel's `.../net/starlark/java/eval/testdata`).
//! Classes B/C (the `Razel` e2e driver) are not wired yet — reported as such, honestly.

use razel_conformance::run_star_source;
use std::path::{Path, PathBuf};

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

fn main() {
    let dir = std::env::args()
        .nth(1)
        .expect("usage: dashboard <dir-with-.star-files>");
    let mut files = Vec::new();
    collect_star(Path::new(&dir), &mut files);
    files.sort();

    let (mut total_pass, mut total_cases, mut fully_green) = (0usize, 0usize, 0usize);
    println!("# Class A — Starlark golden conformance\n");
    println!("| file | pass/total |");
    println!("|---|---|");
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap_or_default();
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let rep = run_star_source(&name, &src);
        let (p, t) = (rep.passed(), rep.total());
        total_pass += p;
        total_cases += t;
        if p == t && t > 0 {
            fully_green += 1;
        }
        println!("| {name} | {p}/{t} |");
    }
    let pct = if total_cases == 0 {
        0.0
    } else {
        100.0 * total_pass as f64 / total_cases as f64
    };
    println!(
        "\n**Class A: {total_pass}/{total_cases} cases ({pct:.1}%) across {} files; {fully_green} fully green.**",
        files.len()
    );
    println!("**Class B (Buck2 e2e): not wired (Razel driver — later phase).**");
    println!("**Class C (example projects): not wired (later phase).**");
}
