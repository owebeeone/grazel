//! The TF TREE-LOAD driver (L6 coverage): sweep every package under tensorflow/, load each in
//! one shared session, and report the coverage curve + the top failure classes — the
//! checkpoint-3 yardstick. A package = a directory with a BUILD file.

use razel_loading::{GlobalFlags, load_tree_report};
use std::collections::BTreeMap;
use std::path::Path;

pub(crate) fn tfload(root: &Path) -> Result<(), String> {
    let ws = root.join("../third-party/tensorflow");
    let mut packages = Vec::new();
    let mut stack = vec![ws.join("tensorflow")];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|f| f == "BUILD" || f == "BUILD.bazel") {
                if let Ok(rel) = dir.strip_prefix(&ws) {
                    packages.push(rel.to_string_lossy().to_string());
                }
            }
        }
    }
    packages.sort();
    packages.dedup();
    let mut flags = GlobalFlags::default();
    flags.external_base = Some(root.join("../third-party"));
    let total = packages.len();
    let report = load_tree_report(&ws, flags, &packages);
    let ok = report.iter().filter(|(_, r)| r.is_ok()).count();
    // Failure classes: first line, trimmed to a coarse signature.
    let mut classes: BTreeMap<String, (usize, String)> = BTreeMap::new();
    for (pkg, r) in &report {
        if let Err(e) = r {
            // Signature = the LAST `error:`-ish line (the deepest cause), not caret art.
            let line = e
                .lines()
                .rev()
                .find(|l| {
                    let t = l.trim();
                    !t.is_empty()
                        && !t.starts_with('|')
                        && !t.starts_with('^')
                        && !t.starts_with("-->")
                        && !t.chars().all(|c| c.is_ascii_digit() || c == ' ' || c == '|')
                })
                .unwrap_or(e)
                .trim();
            let sig: String = line.chars().take(90).collect();
            let entry = classes.entry(sig).or_insert((0, pkg.clone()));
            entry.0 += 1;
        }
    }
    let mut sorted: Vec<_> = classes.into_iter().collect();
    sorted.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
    println!("tfload: {ok}/{total} packages load ({:.1}%)", 100.0 * ok as f64 / total as f64);
    println!("top failure classes:");
    for (sig, (n, example)) in sorted.iter().take(15) {
        println!("  {n:4}  {sig}  (e.g. {example})");
    }
    Ok(())
}
