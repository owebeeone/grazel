//! `cargo xtask perfgate` — the **PL gate** (RazelCrateUniversePlan P0.0). A bounded, hermetic,
//! **< 20 s** workspace-load benchmark that guards against O(n²) load regressions as the
//! loading-phase graph capture (P0.4/P0.5) and query adjacency (P1.3) land.
//!
//! Why not `xtask tfload`: that loads the whole TensorFlow corpus — too slow to run on every
//! step, and it depends on that corpus being vendored. perfgate instead generates a SYNTHETIC
//! corpus of pure NATIVE builtins (`filegroup` deps chains + `alias` + an unresolved `select()`
//! — no external `@rules_rust` load), so it is hermetic, deterministic, and fast, and it
//! exercises exactly the at-risk loading-phase paths.
//!
//! Two assertions (RazelCrateUniversePlan §Performance discipline):
//!   - **scaling** — `t(2N)/t(N) ≤ MAX_RATIO`: the machine-INDEPENDENT no-O(n²) guard (linear
//!     load ≈ 2.0; quadratic ≈ 4.0). This is the primary signal.
//!   - **budget** — `t(N) ≤ baseline + 10%`: a machine-relative constant-factor watch. The
//!     baseline lives in `xtask/perf-baseline.json`; the first run CAPTURES it (no budget check).
//! The whole run must finish under HARD_CAP, else the gate itself is too slow → FAIL (tune
//! `RAZEL_PERFGATE_N` down).

use razel_loading::{GlobalFlags, load_tree_report_with_threads};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// `t(2N)/t(N)`: linear ≈ 2.0, O(n²) ≈ 4.0. 2.3 = linear + headroom for fixed-cost amortization.
const MAX_RATIO: f64 = 2.3;
/// `t(N)` may exceed the recorded baseline by at most this percent.
const BUDGET_PCT: u64 = 10;
/// The gate must run in under this; exceeding it means the benchmark itself got too slow.
const HARD_CAP: Duration = Duration::from_secs(20);

fn env_num(key: &str, default: usize) -> usize {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// One synthetic package's `BUILD`, all NATIVE builtins (no `load()`): a `deps`-style label
/// chain (`filegroup` `srcs`), an `alias`, and an UNRESOLVED `select()`. Back-edges point only
/// to LOWER indices, so any prefix `[0..k]` is a closed, loadable set. Deterministic in `idx`.
fn package_build(idx: usize) -> String {
    let mut deps = String::new();
    for back in [1usize, 2, 5] {
        if idx >= back {
            deps.push_str(&format!("        \"//pkg{}:lib\",\n", idx - back));
        }
    }
    format!(
        "filegroup(name = \"leaf\", srcs = [])\n\
         filegroup(\n    name = \"lib\",\n    srcs = [\n        \":leaf\",\n{deps}    ],\n)\n\
         alias(name = \"lib_alias\", actual = \":lib\")\n\
         filegroup(name = \"sel\", srcs = select({{\"//conditions:default\": [\":lib\"]}}))\n"
    )
}

/// Generate `count` packages `pkg0..pkg{count-1}` under `root` + an empty `MODULE.bazel` marker.
/// Returns the package-path list (the loader input).
fn generate_corpus(root: &Path, count: usize) -> std::io::Result<Vec<String>> {
    std::fs::create_dir_all(root)?;
    std::fs::write(root.join("MODULE.bazel"), "")?; // workspace-root marker
    let mut packages = Vec::with_capacity(count);
    for i in 0..count {
        let pkg = format!("pkg{i}");
        let dir = root.join(&pkg);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("BUILD"), package_build(i))?;
        packages.push(pkg);
    }
    Ok(packages)
}

/// Load the first `n` packages (sequential, for clean timing). Returns the wall time and the
/// first load error if any (a synthetic corpus must load cleanly, else the timing is meaningless).
fn load_slice(root: &Path, packages: &[String], n: usize) -> (Duration, Option<String>) {
    let t0 = Instant::now();
    let (report, _) =
        load_tree_report_with_threads(root, GlobalFlags::default(), &packages[..n], Vec::new(), 1);
    let dt = t0.elapsed();
    let err = report.iter().find_map(|(p, r)| r.as_ref().err().map(|e| format!("{p}: {e}")));
    (dt, err)
}

/// The scaling check — PURE, so it is unit-testable. Linear load ⇒ ~2.0; O(n²) ⇒ ~4.0.
fn check_scaling(t_n: Duration, t_2n: Duration) -> Result<f64, String> {
    let ratio = t_2n.as_secs_f64() / t_n.as_secs_f64().max(1e-9);
    if ratio > MAX_RATIO {
        return Err(format!(
            "load scales super-linearly: t(2N)/t(N) = {ratio:.2} > {MAX_RATIO} (O(n²) signature)"
        ));
    }
    Ok(ratio)
}

/// The budget check — PURE. Both in milliseconds.
fn check_budget(current_ms: u64, baseline_ms: u64) -> Result<(), String> {
    let ceil = baseline_ms + baseline_ms * BUDGET_PCT / 100;
    if current_ms > ceil {
        return Err(format!(
            "load {current_ms}ms exceeds budget {ceil}ms (baseline {baseline_ms}ms + {BUDGET_PCT}%)"
        ));
    }
    Ok(())
}

fn baseline_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("xtask/perf-baseline.json")
}

pub(crate) fn perfgate(workspace_root: &Path) -> Result<(), String> {
    let started = Instant::now();
    let n = env_num("RAZEL_PERFGATE_N", 300);
    let tmp = std::env::temp_dir().join(format!("razel-perfgate-{}", std::process::id()));

    // Generate 2N once; load the [0..N] and [0..2N] prefixes (each a closed set).
    let packages = generate_corpus(&tmp, 2 * n).map_err(|e| {
        let _ = std::fs::remove_dir_all(&tmp);
        format!("generate corpus: {e}")
    })?;
    let (t_n, err_n) = load_slice(&tmp, &packages, n);
    let (t_2n, err_2n) = load_slice(&tmp, &packages, 2 * n);
    let _ = std::fs::remove_dir_all(&tmp);
    if let Some(e) = err_n.or(err_2n) {
        return Err(format!("synthetic corpus failed to load (harness or loader bug): {e}"));
    }

    let n_ms = t_n.as_millis() as u64;
    let ratio = check_scaling(t_n, t_2n)?;
    println!(
        "perfgate: N={n} {:.0}ms, 2N={} {:.0}ms; scaling t(2N)/t(N) = {ratio:.2} (cap {MAX_RATIO})",
        t_n.as_secs_f64() * 1e3,
        2 * n,
        t_2n.as_secs_f64() * 1e3,
    );

    // Budget vs the recorded baseline. The baseline is only comparable at the SAME `n`, so a
    // missing file OR an `n` mismatch RE-CAPTURES rather than comparing apples to oranges.
    let path = baseline_path(workspace_root);
    let prior = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let baseline_ms = prior
        .as_ref()
        .filter(|v| v.get("n").and_then(serde_json::Value::as_u64) == Some(n as u64))
        .and_then(|v| v.get("load_n_ms").and_then(serde_json::Value::as_u64));
    match baseline_ms {
        Some(baseline_ms) => {
            check_budget(n_ms, baseline_ms)?;
            println!("perfgate: budget OK — {n_ms}ms ≤ baseline {baseline_ms}ms + {BUDGET_PCT}%");
        }
        None => {
            let why = if prior.is_some() { "recaptured (N changed)" } else { "captured" };
            let json = serde_json::json!({ "n": n, "load_n_ms": n_ms, "ratio_x100": (ratio * 100.0) as u64 });
            std::fs::write(&path, serde_json::to_string_pretty(&json).unwrap() + "\n")
                .map_err(|e| format!("write {}: {e}", path.display()))?;
            println!("perfgate: baseline {why} → {} (load_n_ms={n_ms})", path.display());
        }
    }

    let total = started.elapsed();
    if total > HARD_CAP {
        return Err(format!(
            "perfgate ran {:.1}s > {}s cap — lower RAZEL_PERFGATE_N",
            total.as_secs_f64(),
            HARD_CAP.as_secs()
        ));
    }
    println!("xtask perfgate: OK ({:.1}s total)", total.as_secs_f64());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_generation_is_deterministic_and_exercises_the_paths() {
        assert_eq!(package_build(7), package_build(7), "must be deterministic in idx");
        assert_ne!(package_build(0), package_build(7), "idx 0 has no back-edges");
        let p = package_build(10);
        assert!(p.contains("//pkg9:lib"), "back-edge (deps chain) present: {p}");
        assert!(p.contains("alias("), "exercises alias");
        assert!(p.contains("select("), "exercises an unresolved select");
        assert!(!package_build(0).contains("//pkg"), "idx 0 has no cross-package edge");
    }

    #[test]
    fn scaling_check_flags_quadratic() {
        // linear: 2N ≈ 2× → OK (with headroom).
        assert!(check_scaling(Duration::from_millis(100), Duration::from_millis(205)).is_ok());
        // quadratic: 2N ≈ 4× → FAIL.
        assert!(check_scaling(Duration::from_millis(100), Duration::from_millis(400)).is_err());
    }

    #[test]
    fn budget_check_flags_regression() {
        assert!(check_budget(110, 100).is_ok(), "+10% exactly is within budget");
        assert!(check_budget(111, 100).is_err(), "+11% trips the budget");
    }
}
