//! The TF TREE-LOAD driver (L6 coverage): sweep every package under tensorflow/, load each in
//! one shared session, and report the coverage curve + the top failure classes — the
//! checkpoint-3 yardstick. A package = a directory with a BUILD file.

use razel_core::Digest;
use razel_loading::{
    AnalyzedTarget, GlobalFlags, SchedHook, load_tree_report, load_tree_report_seeded,
    load_tree_report_with_targets, prepare_build_asts,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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
    let provider_field_diag = install_provider_field_diag(&mut flags);
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
    // RAZEL_TFLOAD_CACHE=<dir>: the whole-corpus content-addressed taut cache. A second run over an
    // unchanged corpus DECODES the serialized facts instead of re-analyzing — the incremental win.
    // Opt-in; the default sweep below is unchanged.
    if let Some(cache_dir) = std::env::var_os("RAZEL_TFLOAD_CACHE") {
        let cache_dir = PathBuf::from(cache_dir);
        let _ = std::fs::create_dir_all(&cache_dir);
        let cache_file = cache_dir.join(format!("{}.taut", tfcache_fingerprint(&ws, &packages).to_hex()));
        // HIT: decode the serialized facts — no analysis, no Starlark.
        if let Ok(bytes) = std::fs::read(&cache_file) {
            let t = std::time::Instant::now();
            if let Some((ok_count, targets)) = decode_tfcache(&bytes) {
                println!(
                    "phases: parallel read+parse {parse_ms}ms, eval {}ms (FROM CACHE: {} facts decoded)",
                    t.elapsed().as_millis(),
                    targets.len()
                );
                println!(
                    "tfload: {ok_count}/{total} packages load ({:.1}%) [cached]",
                    100.0 * ok_count as f64 / total.max(1) as f64
                );
                return Ok(());
            }
            // corrupt entry → fall through and re-analyze
        }
        // MISS: analyze, then serialize the facts to the cache.
        let load_threads = std::env::var("RAZEL_LOAD_THREADS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or_else(|| {
                std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(6)
            });
        let t1 = std::time::Instant::now();
        let (report, _loaded, targets) =
            load_tree_report_with_targets(&ws, flags, &packages, asts, load_threads);
        let eval_ms = t1.elapsed().as_millis();
        let ok_count = report.iter().filter(|(_, r)| r.is_ok()).count();
        let bytes = encode_tfcache(ok_count, &targets);
        let wrote = bytes.len();
        let _ = std::fs::write(&cache_file, bytes);
        println!(
            "phases: parallel read+parse {parse_ms}ms, eval {eval_ms}ms (cache MISS → wrote {} facts, {wrote} bytes)",
            targets.len()
        );
        print_provider_reanalyze_diag(&provider_reanalyze_diag);
        print_depset_diag(&depset_diag);
        return summarize(report, total);
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
    print_provider_field_diag(&provider_field_diag);
    summarize(report, total)
}

/// Cache key = the package selection + options + a SOURCE fingerprint over every BUILD/`.bzl`
/// under the corpus. Any source edit (incl. a loaded `.bzl`) changes the key → a miss → re-analyze
/// — so the cache is SOUND for a read-only corpus. Two honest limits, both fine for tfload and
/// noted for the general build-path cache: it does not catch a glob-affecting change to a
/// NON-BUILD/`.bzl` file (e.g. a new `.cc`), and it uses size+mtime (a content edit that preserves
/// both — rare — would be missed); a precise read-set + content hash is the follow-up.
fn tfcache_fingerprint(ws: &Path, packages: &[String]) -> Digest {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"tfload-cache-v2\0");
    for pkg in packages {
        buf.extend_from_slice(pkg.as_bytes());
        buf.push(0);
    }
    buf.push(0xff);
    buf.extend_from_slice(source_fingerprint(&ws.join("tensorflow")).as_bytes());
    Digest::of(&buf)
}

/// Fingerprint every `BUILD`/`BUILD.bazel`/`*.bzl` under `root` by (relpath, size, mtime), sorted.
/// Non-source files are ignored. Iterative walk (no recursion).
fn source_fingerprint(root: &Path) -> Digest {
    let mut entries: Vec<(String, u64, u128)> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(e.path());
                continue;
            }
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name == "BUILD" || name == "BUILD.bazel" || name.ends_with(".bzl") {
                if let Ok(md) = e.metadata() {
                    let mtime = md
                        .modified()
                        .ok()
                        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_nanos())
                        .unwrap_or(0);
                    let path = e.path();
                    let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned();
                    entries.push((rel, md.len(), mtime));
                }
            }
        }
    }
    entries.sort();
    let mut buf = Vec::new();
    for (rel, size, mtime) in &entries {
        buf.extend_from_slice(rel.as_bytes());
        buf.push(0);
        buf.extend_from_slice(&size.to_le_bytes());
        buf.extend_from_slice(&mtime.to_le_bytes());
    }
    Digest::of(&buf)
}

/// Cache file = `[ok_count: u64-le][gzip(taut snapshot bytes)]`. The taut facts repeat paths and
/// labels heavily, so deflate shrinks them a lot; the cost is paid on the slow miss, and gzip
/// inflate on the hit is fast.
fn encode_tfcache(ok_count: usize, targets: &[AnalyzedTarget]) -> Vec<u8> {
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write;
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    let _ = enc.write_all(&razel_deps_engine::encode_snapshot(targets));
    let mut buf = (ok_count as u64).to_le_bytes().to_vec();
    buf.extend_from_slice(&enc.finish().unwrap_or_default());
    buf
}

fn decode_tfcache(bytes: &[u8]) -> Option<(usize, Vec<AnalyzedTarget>)> {
    use std::io::Read;
    if bytes.len() < 8 {
        return None;
    }
    let ok = u64::from_le_bytes(bytes[..8].try_into().ok()?) as usize;
    let mut snapshot = Vec::new();
    flate2::read::GzDecoder::new(&bytes[8..]).read_to_end(&mut snapshot).ok()?;
    let targets = razel_deps_engine::decode_snapshot(&snapshot).ok()?;
    Some((ok, targets))
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

#[derive(Default)]
struct ProviderFields {
    projectable: usize,
    nonproj: usize,
    nonproj_types: BTreeMap<String, usize>,
}
type ProviderFieldDiag = Arc<Mutex<ProviderFields>>;

/// RAZEL_TFLOAD_DIAG_PROVIDER_FIELDS: count captured provider FIELDS that are DDS-projectable vs
/// not — the signal for whether cross-thread consumers can be served from DDS facts (option 1) or
/// need the faithful fix (option 2/3). NOTE: counts ALL captured fields (an upper bound on the
/// loss — a non-projectable field that no consumer reads is still counted).
fn install_provider_field_diag(flags: &mut GlobalFlags) -> Option<ProviderFieldDiag> {
    if std::env::var_os("RAZEL_TFLOAD_DIAG_PROVIDER_FIELDS").is_none() {
        return None;
    }
    let counts: ProviderFieldDiag = Arc::new(Mutex::new(ProviderFields::default()));
    let hook = Arc::clone(&counts);
    let previous = flags.sched_hook.clone();
    flags.sched_hook = Some(SchedHook(Arc::new(move |point, key| {
        if let Some(h) = &previous {
            (h.0)(point, key);
        }
        if point == "provider-field" {
            if let Some((proj, ty)) = key.split_once(':') {
                let mut c = hook.lock().expect("provider field diag");
                if proj == "1" {
                    c.projectable += 1;
                } else {
                    c.nonproj += 1;
                    *c.nonproj_types.entry(ty.to_string()).or_default() += 1;
                }
            }
        }
    })));
    Some(counts)
}

fn print_provider_field_diag(diag: &Option<ProviderFieldDiag>) {
    let Some(counts) = diag else { return };
    let c = counts.lock().expect("provider field diag");
    let total = c.projectable + c.nonproj;
    if total == 0 {
        println!("provider-fields: (none captured)");
        return;
    }
    println!(
        "provider-fields: {}/{} DDS-projectable ({:.1}%); {} non-projectable",
        c.projectable,
        total,
        100.0 * c.projectable as f64 / total as f64,
        c.nonproj
    );
    let mut sorted: Vec<_> = c.nonproj_types.iter().collect();
    sorted.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (ty, n) in sorted.into_iter().take(10) {
        println!("  {n:8}  non-projectable field type `{ty}`");
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

    #[test]
    fn source_fingerprint_changes_on_bzl_edit_not_on_unrelated_files() {
        let dir = std::env::temp_dir().join(format!("razel-srcfp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("pkg")).unwrap();
        std::fs::write(dir.join("pkg/BUILD"), "filegroup(name='g')").unwrap();
        std::fs::write(dir.join("pkg/defs.bzl"), "A = 1").unwrap();
        let fp0 = source_fingerprint(&dir);

        // Unrelated (non-BUILD/.bzl) files do NOT change the key.
        std::fs::write(dir.join("pkg/notes.txt"), "hello").unwrap();
        std::fs::write(dir.join("pkg/main.cc"), "int main(){}").unwrap();
        assert_eq!(fp0, source_fingerprint(&dir), "non-source files are ignored");

        // A loaded `.bzl` edit DOES (size + mtime move) — the caveat-1 fix.
        std::fs::write(dir.join("pkg/defs.bzl"), "A = 2  # edited").unwrap();
        let fp1 = source_fingerprint(&dir);
        assert_ne!(fp0, fp1, "a .bzl edit must invalidate the cache key");

        // …and a BUILD edit.
        std::fs::write(dir.join("pkg/BUILD"), "filegroup(name='g2')").unwrap();
        assert_ne!(fp1, source_fingerprint(&dir), "a BUILD edit must invalidate");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
