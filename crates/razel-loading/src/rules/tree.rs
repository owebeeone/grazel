//! `rules::tree` — split from `rules.rs` (facade in `mod.rs`).

use crate::state::{AnalyzedTarget, GlobalFlags, Session};
use starlark::syntax::{AstModule, Dialect};
use std::path::Path;
use super::*;

/// The TREE-LOAD driver (L6 coverage metric): load every given package in ONE session
/// (shared .bzl cache / config space — the realistic shape), returning per-package results.
/// A package failure doesn't stop the sweep; the report is the point.
pub fn load_tree_report(
    root: &Path,
    flags: GlobalFlags,
    packages: &[String],
) -> Vec<(String, Result<(), String>)> {
    load_tree_report_prepared(root, flags, packages, Vec::new())
}


/// `load_tree_report` with PRE-PARSED BUILD ASTs (from [`prepare_build_asts`]): the load+parse
/// half runs in parallel; the eval half stays sequential and consumes the cache.
/// `load_tree_report_prepared` + the session's FULL loaded-package list (deps included) —
/// the next run seeds its queue with it so workers fan across the shared dep spine instead
/// of queueing behind whoever demands it first.
pub fn load_tree_report_prepared(
    root: &Path,
    flags: GlobalFlags,
    packages: &[String],
    asts: Vec<(String, starlark::syntax::AstModule)>,
) -> Vec<(String, Result<(), String>)> {
    load_tree_report_seeded(root, flags, packages, asts).0
}


pub fn load_tree_report_seeded(
    root: &Path,
    flags: GlobalFlags,
    packages: &[String],
    asts: Vec<(String, starlark::syntax::AstModule)>,
) -> (Vec<(String, Result<(), String>)>, Vec<String>) {
    // P4: N workers over a shared queue against ONE Session (Send+Sync, P1–P3).
    // DEFAULT-ON since round 46 (decision: Gianni — the parity bar held since round 34:
    // driven-work coverage equality, deterministic run-to-run, 0 livelock signatures).
    // RAZEL_LOAD_THREADS=1 reproduces sequential behavior exactly (the escape hatch).
    // Default 6 (Gianni, round 46): the measured knee — 12 workers average ~5 busy cores
    // at current corpus depth (the spine serializes the rest); 6 buys the same wall with
    // half the contention. Bazel-flag mapping of record: this is `--loading_phase_threads`
    // (loading/analysis), NOT `--jobs` (execution-phase actions) — wire when razel-cli
    // grows a tree command.
    let threads = std::env::var("RAZEL_LOAD_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
                .min(6)
        });
    load_tree_report_with_threads(root, flags, packages, asts, threads)
}


/// [`load_tree_report_seeded`] with an EXPLICIT worker count (the parity tests' entry —
/// no env mutation).
pub fn load_tree_report_with_threads(
    root: &Path,
    flags: GlobalFlags,
    packages: &[String],
    asts: Vec<(String, starlark::syntax::AstModule)>,
    threads: usize,
) -> (Vec<(String, Result<(), String>)>, Vec<String>) {
    let (_session, report, loaded) = drive_tree(root, flags, packages, asts, threads);
    (report, loaded)
}


/// Like [`load_tree_report_with_threads`], plus every analyzed target (the Session `results`
/// values) — the input to taut fact serialization and the content-addressed cache.
pub fn load_tree_report_with_targets(
    root: &Path,
    flags: GlobalFlags,
    packages: &[String],
    asts: Vec<(String, starlark::syntax::AstModule)>,
    threads: usize,
) -> (Vec<(String, Result<(), String>)>, Vec<String>, Vec<AnalyzedTarget>) {
    let (session, report, loaded) = drive_tree(root, flags, packages, asts, threads);
    let targets = session.results.borrow().values().cloned().collect();
    (report, loaded, targets)
}


pub(crate) fn drive_tree(
    root: &Path,
    flags: GlobalFlags,
    packages: &[String],
    asts: Vec<(String, starlark::syntax::AstModule)>,
    threads: usize,
) -> (Session, Vec<(String, Result<(), String>)>, Vec<String>) {
    let session = Session::new(Some(root.to_path_buf()), flags);
    session.ast_cache.borrow_mut().extend(asts);
    if threads <= 1 {
        let report: Vec<(String, Result<(), String>)> = packages
            .iter()
            .map(|pkg| (pkg.clone(), load_package_entry(&session, pkg)))
            .collect();
        crate::loaded::finalize_edges(&session, root); // P0.5: resolve loading-phase edges
        let loaded = loaded_done(&session);
        return (session, report, loaded);
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(vec![None; packages.len()]);
    let retry = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(pkg) = packages.get(i) else { break };
                    let before = session.partial_reads();
                    let r = load_package_entry(&session, pkg);
                    // F4 (restart): an entry that FAILED after consuming cross-thread
                    // partial state (CycleProceed grants, dead declaration waits) is not a
                    // sequential verdict — queue it for the post-drain restart rounds.
                    if r.is_err() && session.partial_reads() > before {
                        retry.lock().expect("retry").push(i);
                    }
                    results
                        .lock()
                        .expect("results")
                        .get_mut(i)
                        .map(|slot| *slot = Some(r));
                }
            });
        }
    });
    let mut results = results.into_inner().expect("results");
    // Restart rounds, SINGLE-threaded (Skyframe's answer, RazelDemandFutures.md §5): by
    // now the cycle partners are terminal, so each retry sees what a sequential entry
    // would have. Rounds until no progress — termination is structural, no cap to tune.
    let mut retry = retry.into_inner().expect("retry");
    retry.sort_unstable();
    while !retry.is_empty() {
        eprintln!(
            "razel: restarting {} entry load(s) after cross-thread partial reads",
            retry.len()
        );
        let mut progressed = false;
        let mut still_failing = Vec::new();
        for &i in &retry {
            let r = load_package_entry(&session, &packages[i]);
            if r.is_ok() {
                progressed = true;
            } else {
                still_failing.push(i);
            }
            results[i] = Some(r);
        }
        if !progressed {
            break;
        }
        retry = still_failing;
    }
    let report: Vec<(String, Result<(), String>)> = packages
        .iter()
        .cloned()
        .zip(results.into_iter().map(|r| r.unwrap_or(Ok(()))))
        .collect();
    crate::loaded::finalize_edges(&session, root); // P0.5: resolve loading-phase edges
    let loaded = loaded_done(&session);
    (session, report, loaded)
}


/// All packages the session finished loading (deps included) — the spine list. The wait
/// graph also tracks `.bzl` modules and declarations — packages only here.
pub(crate) fn loaded_done(session: &Session) -> Vec<String> {
    session
        .loaded
        .lock()
        .expect("loaded")
        .res
        .iter()
        .filter_map(|(k, st)| match (k, st) {
            (crate::state::ResKey::Pkg(p), crate::state::PkgState::Done) => Some(p.clone()),
            _ => None,
        })
        .collect()
}


/// PARALLEL read+parse of the packages' BUILD files (pure — no Session state): the
/// load+parse / execute split. Returns `({pkg}/BUILD, ast)` pairs; unparseable files are
/// skipped (the sequential path re-reads and surfaces the error properly).
pub fn prepare_build_asts(
    root: &Path,
    packages: &[String],
    threads: usize,
    strict_bazel: bool,
) -> Vec<(String, starlark::syntax::AstModule)> {
    let n = threads.max(1);
    let chunks: Vec<&[String]> = packages.chunks(packages.len().div_ceil(n)).collect();
    std::thread::scope(|scope| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                scope.spawn(move || {
                    let mut out = Vec::new();
                    for pkg in chunk {
                        // Same resolution as `load_package` (E-mode XOR, bazel precedence);
                        // XOR errors are SKIPPED here so the sequential path surfaces them.
                        let Ok(Some(path)) =
                            crate::workspace::resolve_build_file(&root.join(pkg), strict_bazel)
                        else {
                            continue;
                        };
                        let Ok(src) = std::fs::read_to_string(&path) else {
                            continue;
                        };
                        let name = format!("{pkg}/BUILD");
                        if let Ok(ast) = AstModule::parse(
                            &name,
                            detab_leading(&src).into_owned(),
                            &Dialect::Extended,
                        ) {
                            out.push((name, ast));
                        }
                    }
                    out
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    })
}


