//! S2 (round 28): DETERMINISTIC interleaving tests over the wait-graph seam
//! (`GlobalFlags::sched_hook`). The P4a shakeout's races were caught by re-running a
//! probabilistic fixture and capturing traces; these script the schedules instead, so the
//! regressions are exact. Bugs covered: #2 (.bzl double-eval), #5 (cycle livelock),
//! #6 (InFlight leak). Bugs #3/#4 (harvest visibility/indexing) stay guarded by
//! parallel_parity.rs — they live below this seam.

use razel_loading::{GlobalFlags, SchedHook, load_tree_report_with_threads};
use std::sync::{Arc, Barrier, Mutex};

/// Collects every (point, key) event; releases a 2-party barrier when both workers ENTER
/// an acquire whose key passes `gate` — the schedule-forcing trick: both workers are held
/// at the contention point, then released together.
fn hook_with_gate(
    events: Arc<Mutex<Vec<(String, String)>>>,
    gate: impl Fn(&str) -> bool + Send + Sync + 'static,
) -> SchedHook {
    let barrier = Arc::new(Barrier::new(2));
    SchedHook(Arc::new(move |point: &str, key: &str| {
        events.lock().unwrap().push((point.to_string(), key.to_string()));
        if point == "enter" && gate(key) {
            barrier.wait();
        }
    }))
}

fn count(events: &Mutex<Vec<(String, String)>>, point: &str, key_part: &str) -> usize {
    events
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, k)| p == point && k.contains(key_part))
        .count()
}

/// Bug #6 regression (the "deadlock again" livelock): a load that errors BEFORE eval (here:
/// unvendored external repo) must still finish its InFlight claim. With the leak, the second
/// worker parked on a dead entry until the 20s takeover — observable as a takeover-timeout
/// event. Two workers, both demanding the same unvendored repo, gated to collide.
#[test]
fn failed_external_load_finishes_instead_of_leaking() {
    let root = std::env::temp_dir().join(format!("razel-seam-leak-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for pkg in ["a", "b"] {
        std::fs::create_dir_all(root.join(pkg)).unwrap();
        std::fs::write(
            root.join(pkg).join("BUILD"),
            format!("filegroup(name = \"{pkg}\", srcs = [\"@unvendored_zzz//:x\"])\n"),
        )
        .unwrap();
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut flags = GlobalFlags::default();
    flags.external_base = Some(root.clone()); // exists, but @unvendored_zzz does not
    flags.sched_hook = Some(hook_with_gate(events.clone(), |k| k.starts_with("@unvendored_zzz")));
    let (report, _) = load_tree_report_with_threads(
        &root,
        flags,
        &["a".to_string(), "b".to_string()],
        Vec::new(),
        2,
    );
    let _ = std::fs::remove_dir_all(&root);
    for (pkg, r) in &report {
        assert!(r.is_err(), "{pkg} must fail LOUDLY on the unvendored dep: {r:?}");
    }
    assert_eq!(count(&events, "takeover-timeout", ""), 0, "no waiter may hit the 20s backstop");
    assert!(
        count(&events, "finish-err", "@unvendored_zzz") >= 1,
        "the failed load must finish its claim (the leak regression): {:?}",
        events.lock().unwrap()
    );
}

/// Bug #5 regression: a cross-thread PACKAGE cycle (a's drive demands b, b's drive demands a;
/// no target cycle) must resolve via cycle-proceed — never via the 20s timeout. Consumers
/// read dep FILES only: provider flow through a cycle's partial state is the known
/// demand-futures gap, not this test's contract.
#[test]
fn package_cycle_resolves_without_timeout() {
    let root = std::env::temp_dir().join(format!("razel-seam-cycle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("defs")).unwrap();
    std::fs::write(
        root.join("defs/info.bzl"),
        r#"def _lib_impl(ctx):
    return [DefaultInfo(files = [])]

lib = rule(implementation = _lib_impl, attrs = {})

def _use_impl(ctx):
    paths = []
    for d in ctx.attr.deps:
        for f in d.files:
            paths.append(f.path)
    ctx.actions.run(executable = "tool", outputs = [], inputs = [], arguments = paths)

use = rule(implementation = _use_impl, attrs = {"deps": attr.label_list()})
"#,
    )
    .unwrap();
    std::fs::write(root.join("defs/BUILD"), "").unwrap();
    for (pkg, other) in [("a", "b"), ("b", "a")] {
        std::fs::create_dir_all(root.join(pkg)).unwrap();
        std::fs::write(
            root.join(pkg).join("BUILD"),
            format!(
                "load(\"//defs:info.bzl\", \"lib\", \"use\")\nlib(name = \"{pkg}1\")\nuse(name = \"{pkg}2\", deps = [\"//{other}:{other}1\"])\n"
            ),
        )
        .unwrap();
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut flags = GlobalFlags::default();
    // Gate: each worker entering the OTHER's package acquire — held until both collide.
    flags.sched_hook = Some(hook_with_gate(events.clone(), |k| k == "a" || k == "b"));
    let (report, _) = load_tree_report_with_threads(
        &root,
        flags,
        &["a".to_string(), "b".to_string()],
        Vec::new(),
        2,
    );
    let _ = std::fs::remove_dir_all(&root);
    for (pkg, r) in &report {
        assert!(r.is_ok(), "cycle members must load: {pkg}: {r:?}");
    }
    assert_eq!(count(&events, "takeover-timeout", ""), 0, "cycles must not reach the backstop");
    assert!(
        count(&events, "cycle-proceed", "") >= 1,
        "the forced collision must resolve via cycle-proceed: {:?}",
        events.lock().unwrap()
    );
}

/// The PkgState::Failed memo (round 29): a DECLARE-phase package failure (here: pre-eval —
/// the dep repo is not vendored) is Bazel's "package in error" — it must evaluate ONCE and
/// serve every later consumer the CACHED error. Before the memo, the purge-retry semantics
/// re-evaluated the failing package per consumer (the 1:00 → 1:17 sweep regression).
/// Analysis-phase failures stay retryable (guarded by cross_package_providers).
#[test]
fn declare_phase_failure_is_cached_package_in_error() {
    let root = std::env::temp_dir().join(format!("razel-seam-memo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    // Two consumers of the same failing package: `c` fails pre-eval (unvendored dep repo
    // inside its BUILD via a load — simplest: c's BUILD itself is fine but deps the repo).
    std::fs::create_dir_all(root.join("c")).unwrap();
    std::fs::write(
        root.join("c/BUILD"),
        "filegroup(name = \"c\", srcs = [\"@unvendored_zzz//:x\"])\n",
    )
    .unwrap();
    for pkg in ["a", "b"] {
        std::fs::create_dir_all(root.join(pkg)).unwrap();
        std::fs::write(
            root.join(pkg).join("BUILD"),
            format!("filegroup(name = \"{pkg}\", srcs = [\"//c:c\"])\n"),
        )
        .unwrap();
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let ev = events.clone();
    let mut flags = GlobalFlags::default();
    flags.external_base = Some(root.clone());
    flags.sched_hook = Some(SchedHook(Arc::new(move |p: &str, k: &str| {
        ev.lock().unwrap().push((p.to_string(), k.to_string()));
    })));
    // SEQUENTIAL: the memo is about retry semantics, not races.
    let (report, _) = load_tree_report_with_threads(
        &root,
        flags,
        &["a".to_string(), "b".to_string()],
        Vec::new(),
        1,
    );
    let _ = std::fs::remove_dir_all(&root);
    for (pkg, r) in &report {
        assert!(r.is_err(), "{pkg} must surface c's package-in-error loudly: {r:?}");
        let msg = r.as_ref().unwrap_err();
        assert!(msg.contains("not vendored"), "{pkg} must carry the REAL error: {msg}");
    }
    assert_eq!(
        count(&events, "own", "@unvendored_zzz"),
        1,
        "the failing dep load must run ONCE — later consumers read the cached error: {:?}",
        events.lock().unwrap()
    );
}

/// Bug #2 regression: two workers racing the same uncached `.bzl` must produce ONE eval
/// (one `own`) and one waiter (`ready`) — a double-eval would mint two provider identities.
#[test]
fn bzl_single_flight_under_forced_collision() {
    let root = std::env::temp_dir().join(format!("razel-seam-bzl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("defs")).unwrap();
    std::fs::write(
        root.join("defs/info.bzl"),
        r#"MyInfo = provider(fields = ["msg"])

def _lib_impl(ctx):
    return [MyInfo(msg = "x")]

lib = rule(implementation = _lib_impl, attrs = {})
"#,
    )
    .unwrap();
    std::fs::write(root.join("defs/BUILD"), "").unwrap();
    for pkg in ["a", "b"] {
        std::fs::create_dir_all(root.join(pkg)).unwrap();
        std::fs::write(
            root.join(pkg).join("BUILD"),
            format!("load(\"//defs:info.bzl\", \"lib\")\nlib(name = \"{pkg}1\")\n"),
        )
        .unwrap();
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut flags = GlobalFlags::default();
    flags.sched_hook = Some(hook_with_gate(events.clone(), |k| k.ends_with("info.bzl")));
    let (report, _) = load_tree_report_with_threads(
        &root,
        flags,
        &["a".to_string(), "b".to_string()],
        Vec::new(),
        2,
    );
    let _ = std::fs::remove_dir_all(&root);
    for (pkg, r) in &report {
        assert!(r.is_ok(), "{pkg}: {r:?}");
    }
    assert_eq!(count(&events, "own", "info.bzl"), 1, "exactly ONE eval of the module");
    assert_eq!(count(&events, "ready", "info.bzl"), 1, "the loser must wait and read the cache");
    assert_eq!(count(&events, "takeover-timeout", ""), 0);
}
