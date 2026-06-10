//! The TF TREE-LOAD driver (L6 coverage): sweep every package under tensorflow/, load each in
//! one shared session, and report the coverage curve + the top failure classes — the
//! checkpoint-3 yardstick. A package = a directory with a BUILD file.

use razel_loading::{GlobalFlags, load_tree_report, load_tree_report_seeded, prepare_build_asts};
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
    // RAZEL_TFLOAD_SAMPLE=N: sweep every Nth package — the fast inner loop (seconds, not
    // minutes); the full sweep is for banking numbers.
    if let Ok(n) = std::env::var("RAZEL_TFLOAD_SAMPLE") {
        if let Ok(n) = n.parse::<usize>() {
            if n > 1 {
                packages = packages.into_iter().step_by(n).collect();
            }
        }
    }
    let mut flags = GlobalFlags::default();
    flags.external_base = Some(root.join("../third-party"));
    // RAZEL_TFLOAD_ONE=<pkg>: print one package's FULL error (debugging a failure class).
    if let Ok(one) = std::env::var("RAZEL_TFLOAD_ONE") {
        let report = load_tree_report(&ws, flags, &[one.clone()]);
        match &report[0].1 {
            Ok(()) => println!("{one}: OK"),
            Err(e) => println!("{one}: FAIL
{e}"),
        }
        return Ok(());
    }
    let total = packages.len();
    // Load+parse / execute split: the pure half parallelizes; eval consumes the AST cache.
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8);
    let t0 = std::time::Instant::now();
    let asts = prepare_build_asts(&ws, &packages, threads);
    let parse_ms = t0.elapsed().as_millis();
    // Spine seeding: prepend the previous run's loaded-set (deps incl. — the llvm/mlir spine
    // is wide and mutually independent) so workers fan across it instead of queueing behind
    // one demand chain.
    let spine_path = std::env::temp_dir().join("razel-tfload-spine.txt");
    // EXPERIMENTAL (off by default): run-2 timing came back anomalously fast (208ms for a
    // spine that takes 20s) with matching coverage — too good to trust without diagnosis.
    // Enable with RAZEL_TFLOAD_SEED=1 to investigate.
    let seed_enabled = std::env::var("RAZEL_TFLOAD_SEED").is_ok();
    if let Ok(spine) = std::fs::read_to_string(&spine_path).and_then(|s| {
        if seed_enabled { Ok(s) } else { Err(std::io::Error::other("seeding disabled")) }
    }) {
        let mut seeded: Vec<String> = spine.lines().map(String::from).collect();
        let known: std::collections::BTreeSet<&str> =
            seeded.iter().map(|s| s.as_str()).collect();
        let _ = known; // seed list first, sweep list after (dedup below)
        seeded.extend(packages.iter().cloned());
        seeded.dedup();
        let mut seen = std::collections::BTreeSet::new();
        seeded.retain(|p| seen.insert(p.clone()));
        // Only the seed ORDER changes; the REPORT below still scores the sweep list.
        let t1 = std::time::Instant::now();
        let (full, loaded) = load_tree_report_seeded(&ws, flags, &seeded, asts);
        let _ = std::fs::write(&spine_path, loaded.join("\n"));
        let by_pkg: std::collections::BTreeMap<&str, &Result<(), String>> =
            full.iter().map(|(p, r)| (p.as_str(), r)).collect();
        let report: Vec<(String, Result<(), String>)> = packages
            .iter()
            .map(|p| {
                (p.clone(), by_pkg.get(p.as_str()).map(|r| (*r).clone()).unwrap_or(Ok(())))
            })
            .collect();
        println!(
            "phases: parallel read+parse {parse_ms}ms ({threads} threads), eval {}ms (seeded)",
            t1.elapsed().as_millis()
        );
        return summarize(report, packages.len());
    }
    let t1 = std::time::Instant::now();
    let (report, loaded) = load_tree_report_seeded(&ws, flags, &packages, asts);
    let _ = std::fs::write(&spine_path, loaded.join("\n"));
    println!(
        "phases: parallel read+parse {parse_ms}ms ({threads} threads), eval {}ms",
        t1.elapsed().as_millis()
    );
    summarize(report, total)
}

fn summarize(report: Vec<(String, Result<(), String>)>, total: usize) -> Result<(), String> {
    let ok = report.iter().filter(|(_, r)| r.is_ok()).count();
    // Failure classes: signature = the LAST line carrying an error message.
    let mut classes: BTreeMap<String, (usize, String)> = BTreeMap::new();
    for (pkg, r) in &report {
        if let Err(e) = r {
            let line = e
                .lines()
                .rev()
                .find(|l| l.contains("error") || l.contains("failed") || l.contains("not "))
                .or_else(|| e.lines().next())
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
