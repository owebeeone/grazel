//! The TF TREE-LOAD driver (L6 coverage): sweep every package under tensorflow/, load each in
//! one shared session, and report the coverage curve + the top failure classes — the
//! checkpoint-3 yardstick. A package = a directory with a BUILD file.

use razel_loading::{
    GlobalFlags, SchedHook, load_tree_report, load_tree_report_seeded, prepare_build_asts,
};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

type ProviderReanalyzeDiag = Arc<Mutex<BTreeMap<String, usize>>>;
type DepsetDiagHandle = Arc<Mutex<DepsetDiag>>;

// Depsets are now a DAG (NestedSet): construction stores `direct` members + `transitive` child
// refs WITHOUT flattening, so the diagnostic reports DAG shape (width), not a flattened element
// census. Flatten cost moved to consumption; the headline metric is the eval-phase wall-clock.
#[derive(Debug, Default)]
struct DepsetDiag {
    calls: usize,
    direct: usize,
    transitive: usize,
    max_direct: usize,
    max_transitive: usize,
}

impl DepsetDiag {
    fn record(&mut self, key: &str) {
        let direct = depset_stat(key, "direct").unwrap_or(0);
        let transitive = depset_stat(key, "transitive").unwrap_or(0);

        self.calls += 1;
        self.direct += direct;
        self.transitive += transitive;
        self.max_direct = self.max_direct.max(direct);
        self.max_transitive = self.max_transitive.max(transitive);
    }
}

fn depset_stat(key: &str, name: &str) -> Option<usize> {
    key.split_whitespace()
        .find_map(|part| part.strip_prefix(name)?.strip_prefix('=')?.parse().ok())
}

/// Every package (dir with a BUILD file) under `<ws>/tensorflow`, sorted; `sample` keeps
/// every Nth (the fast inner loop). Shared by `tfload` and `stress`.
pub(crate) fn discover_packages(ws: &Path, sample: usize) -> Vec<String> {
    let mut packages = Vec::new();
    let mut stack = vec![ws.join("tensorflow")];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p
                .file_name()
                .is_some_and(|f| f == "BUILD" || f == "BUILD.bazel")
            {
                if let Ok(rel) = dir.strip_prefix(ws) {
                    packages.push(rel.to_string_lossy().to_string());
                }
            }
        }
    }
    packages.sort();
    packages.dedup();
    if sample > 1 {
        packages = packages.into_iter().step_by(sample).collect();
    }
    packages
}

pub(crate) fn tfload(root: &Path) -> Result<(), String> {
    let ws = root.join("../third-party/tensorflow");
    // RAZEL_TFLOAD_SAMPLE=N: sweep every Nth package — the fast inner loop (seconds, not
    // minutes); the full sweep is for banking numbers.
    let sample = std::env::var("RAZEL_TFLOAD_SAMPLE")
        .ok()
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(1);
    let packages = discover_packages(&ws, sample);
    let mut flags = GlobalFlags::default();
    flags.external_base = Some(root.join("../third-party"));
    flags.fetched_external_base = crate::fetchcmd::fetched_external_dir(&ws);
    let provider_reanalyze_diag = install_provider_reanalyze_diag(&mut flags);
    let depset_diag = install_depset_diag(&mut flags);
    // RAZEL_TFLOAD_ONE=<pkg>[,<pkg>…]: print FULL errors (debugging a failure class). A comma
    // list loads in order in ONE session — replicates sweep context (earlier packages paving
    // aliases/config_settings) for order-dependent classes.
    if let Ok(one) = std::env::var("RAZEL_TFLOAD_ONE") {
        let pkgs: Vec<String> = one.split(',').map(String::from).collect();
        let report = load_tree_report(&ws, flags, &pkgs);
        for (pkg, r) in &report {
            match r {
                Ok(()) => println!("{pkg}: OK"),
                Err(e) => println!(
                    "{pkg}: FAIL
{e}"
                ),
            }
        }
        print_provider_reanalyze_diag(&provider_reanalyze_diag);
        print_depset_diag(&depset_diag);
        return Ok(());
    }
    let total = packages.len();
    // Load+parse / execute split: the pure half parallelizes; eval consumes the AST cache.
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8);
    let t0 = std::time::Instant::now();
    let asts = prepare_build_asts(&ws, &packages, threads, false);
    let parse_ms = t0.elapsed().as_millis();
    // Spine seeding: prepend the previous run's loaded-set (deps incl. — the llvm/mlir spine
    // is wide and mutually independent) so workers fan across it instead of queueing behind
    // one demand chain.
    let spine_path = std::env::temp_dir().join("razel-tfload-spine.txt");
    // DIAGNOSED (round 23; was "208ms anomaly"): seeding was never the bug — the POOL is
    // unsound at threads>1. Per-eval-stack Session state (current_pkg / current_bzl_repo) is
    // shared across workers, so any concurrent eval misresolves labels and the sweep collapses
    // into a fast-fail cascade (sample-16: 6-7/53 in <1s at threads≥2 vs 10/53 in 54s
    // sequential; the "20s" walls were begin_pkg_load's condvar timeout, not eval). Seeding
    // just fans workers out, exposing it sooner. Parked until P4a (per-worker eval context —
    // RazelGaps.md); coverage printed at threads>1 is not a coverage number.
    let seed_enabled = std::env::var("RAZEL_TFLOAD_SEED").is_ok();
    if let Ok(spine) = std::fs::read_to_string(&spine_path).and_then(|s| {
        if seed_enabled {
            Ok(s)
        } else {
            Err(std::io::Error::other("seeding disabled"))
        }
    }) {
        let mut seeded: Vec<String> = spine.lines().map(String::from).collect();
        let known: std::collections::BTreeSet<&str> = seeded.iter().map(|s| s.as_str()).collect();
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
                (
                    p.clone(),
                    by_pkg
                        .get(p.as_str())
                        .map(|r| (*r).clone())
                        .unwrap_or(Ok(())),
                )
            })
            .collect();
        println!(
            "phases: parallel read+parse {parse_ms}ms ({threads} threads), eval {}ms (seeded)",
            t1.elapsed().as_millis()
        );
        print_provider_reanalyze_diag(&provider_reanalyze_diag);
        print_depset_diag(&depset_diag);
        return summarize(report, packages.len());
    }
    let t1 = std::time::Instant::now();
    let (report, loaded) = load_tree_report_seeded(&ws, flags, &packages, asts);
    let _ = std::fs::write(&spine_path, loaded.join("\n"));
    println!(
        "phases: parallel read+parse {parse_ms}ms ({threads} threads), eval {}ms",
        t1.elapsed().as_millis()
    );
    print_provider_reanalyze_diag(&provider_reanalyze_diag);
    print_depset_diag(&depset_diag);
    summarize(report, total)
}

fn install_provider_reanalyze_diag(flags: &mut GlobalFlags) -> Option<ProviderReanalyzeDiag> {
    if std::env::var_os("RAZEL_TFLOAD_DIAG_PROVIDER_REANALYZE").is_none() {
        return None;
    }
    let counts = Arc::new(Mutex::new(BTreeMap::<String, usize>::new()));
    let hook_counts = Arc::clone(&counts);
    let previous = flags.sched_hook.clone();
    flags.sched_hook = Some(SchedHook(Arc::new(move |point, key| {
        if let Some(hook) = &previous {
            (hook.0)(point, key);
        }
        if point == "provider-reanalyze" {
            *hook_counts
                .lock()
                .expect("provider reanalyze counts")
                .entry(key.to_string())
                .or_default() += 1;
        }
    })));
    Some(counts)
}

fn print_provider_reanalyze_diag(diag: &Option<ProviderReanalyzeDiag>) {
    let Some(counts) = diag else { return };
    let counts = counts.lock().expect("provider reanalyze counts");
    let total: usize = counts.values().sum();
    println!(
        "provider-reanalyze: {total} fallback(s) across {} label(s)",
        counts.len()
    );
    let mut sorted: Vec<_> = counts.iter().collect();
    sorted.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (label, n) in sorted.into_iter().take(15) {
        println!("  {n:4}  {label}");
    }
}

fn install_depset_diag(flags: &mut GlobalFlags) -> Option<DepsetDiagHandle> {
    if std::env::var_os("RAZEL_TFLOAD_DIAG_DEPSET").is_none() {
        return None;
    }
    let counts = Arc::new(Mutex::new(DepsetDiag::default()));
    let hook_counts = Arc::clone(&counts);
    let previous = flags.sched_hook.clone();
    flags.sched_hook = Some(SchedHook(Arc::new(move |point, key| {
        if let Some(hook) = &previous {
            (hook.0)(point, key);
        }
        if point == "depset" {
            hook_counts.lock().expect("depset diag").record(key);
        }
    })));
    Some(counts)
}

fn print_depset_diag(diag: &Option<DepsetDiagHandle>) {
    let Some(counts) = diag else { return };
    let counts = counts.lock().expect("depset diag");
    println!(
        "depset: {} construction(s), direct {} member(s), transitive {} child-depset(s), \
         max direct {}, max transitive {}",
        counts.calls, counts.direct, counts.transitive, counts.max_direct, counts.max_transitive
    );
}

fn summarize(report: Vec<(String, Result<(), String>)>, total: usize) -> Result<(), String> {
    let ok = report.iter().filter(|(_, r)| r.is_ok()).count();
    // RAZEL_TFLOAD_CLASS=<substr>: print the FIRST full error matching — the class-member
    // debugger for order-dependent classes the ONE probe can't reach standalone.
    if let Ok(pat) = std::env::var("RAZEL_TFLOAD_CLASS") {
        if let Some((pkg, Err(e))) = report
            .iter()
            .find(|(_, r)| r.as_ref().is_err_and(|e| e.contains(&pat)))
        {
            println!("=== {pkg}: first `{pat}` member, full error ===\n{e}");
        }
    }
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
    println!(
        "tfload: {ok}/{total} packages load ({:.1}%)",
        100.0 * ok as f64 / total as f64
    );
    println!("top failure classes:");
    for (sig, (n, example)) in sorted.iter().take(15) {
        println!("  {n:4}  {sig}  (e.g. {example})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depset_diag_aggregates_shape_events() {
        let mut diag = DepsetDiag::default();
        diag.record("direct=2 transitive=0");
        diag.record("direct=5 transitive=1");

        assert_eq!(diag.calls, 2);
        assert_eq!(diag.direct, 7);
        assert_eq!(diag.transitive, 1);
        assert_eq!(diag.max_direct, 5);
        assert_eq!(diag.max_transitive, 1);
    }
}
