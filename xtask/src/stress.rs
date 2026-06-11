//! S1 (round 28): the pool STRESS harness — the P4a session's ad-hoc shell loops as a
//! repeatable tool. One sequential baseline sweep, then N parallel sweeps; LOUD failure on
//! any of: a takeover-timeout event (the livelock signature — counted via the S2 sched
//! hook), parallel coverage below the band (default ≥90% of sequential — the known
//! CycleProceed gap; tighten to 100% when demand futures land), or a parallel run slower
//! than sequential (the stall signature).
//!
//! Knobs: RAZEL_STRESS_SAMPLE (every-Nth package, default 8 — the inner loop; 1 = full
//! tree), RAZEL_STRESS_RUNS (parallel runs, default 3), RAZEL_STRESS_BAND_PCT (default 90).

use crate::tfload::discover_packages;
use razel_loading::{GlobalFlags, SchedHook, load_tree_report_with_threads};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct SweepResult {
    ok: usize,
    total: usize,
    timeouts: usize,
    wall: std::time::Duration,
}

fn run_one(ws: &Path, root: &Path, packages: &[String], threads: usize) -> SweepResult {
    let timeouts = Arc::new(AtomicUsize::new(0));
    let counter = timeouts.clone();
    let mut flags = GlobalFlags::default();
    flags.external_base = Some(root.join("../third-party"));
    flags.sched_hook = Some(SchedHook(Arc::new(move |point: &str, _: &str| {
        if point == "takeover-timeout" {
            counter.fetch_add(1, Ordering::Relaxed);
        }
    })));
    let t0 = std::time::Instant::now();
    let (report, _) = load_tree_report_with_threads(ws, flags, packages, Vec::new(), threads);
    SweepResult {
        ok: report.iter().filter(|(_, r)| r.is_ok()).count(),
        total: packages.len(),
        timeouts: timeouts.load(Ordering::Relaxed),
        wall: t0.elapsed(),
    }
}

fn env_num(key: &str, default: usize) -> usize {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

pub(crate) fn stress(root: &Path) -> Result<(), String> {
    let ws = root.join("../third-party/tensorflow");
    let sample = env_num("RAZEL_STRESS_SAMPLE", 8);
    let runs = env_num("RAZEL_STRESS_RUNS", 3);
    let band_pct = env_num("RAZEL_STRESS_BAND_PCT", 90);
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8);
    let packages = discover_packages(&ws, sample);

    let seq = run_one(&ws, root, &packages, 1);
    println!(
        "stress: sequential baseline {}/{} in {:.1}s (sample={sample})",
        seq.ok,
        seq.total,
        seq.wall.as_secs_f32()
    );
    if seq.timeouts != 0 {
        return Err(format!("sequential run hit {} takeover-timeouts (impossible — 1 thread)", seq.timeouts));
    }

    let floor = seq.ok * band_pct / 100;
    for i in 1..=runs {
        let par = run_one(&ws, root, &packages, threads);
        println!(
            "stress: parallel run {i}/{runs} (threads={threads}): {}/{} in {:.1}s, timeouts={}",
            par.ok,
            par.total,
            par.wall.as_secs_f32(),
            par.timeouts
        );
        if par.timeouts != 0 {
            return Err(format!("run {i}: {} takeover-timeout(s) — the livelock signature", par.timeouts));
        }
        if par.ok < floor {
            return Err(format!(
                "run {i}: coverage {}/{} below the band (floor {floor} = {band_pct}% of sequential {})",
                par.ok, par.total, seq.ok
            ));
        }
        if par.wall > seq.wall {
            return Err(format!(
                "run {i}: parallel ({:.1}s) slower than sequential ({:.1}s) — stall signature",
                par.wall.as_secs_f32(),
                seq.wall.as_secs_f32()
            ));
        }
    }
    println!(
        "xtask stress: OK — {runs} parallel runs in band (≥{band_pct}% of {} sequential, 0 timeouts)",
        seq.ok
    );
    Ok(())
}
