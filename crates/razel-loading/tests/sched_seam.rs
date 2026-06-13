//! S2 (round 28): DETERMINISTIC interleaving tests over the wait-graph seam
//! (`GlobalFlags::sched_hook`). The P4a shakeout's races were caught by re-running a
//! probabilistic fixture and capturing traces; these script the schedules instead, so the
//! regressions are exact. Bugs covered: #2 (.bzl double-eval), #5 (cycle livelock),
//! #6 (InFlight leak). Bugs #3/#4 (harvest visibility/indexing) stay guarded by
//! parallel_parity.rs — they live below this seam.

use razel_loading::{GlobalFlags, SchedHook, load_tree_report_with_threads};
use std::sync::{Arc, Mutex};

/// Collects every (point, key) event; holds workers that ENTER an acquire whose key passes
/// `gate` until TWO DISTINCT THREADS have arrived — the schedule-forcing trick. A 5s
/// TIMEOUT rendezvous, not a hard barrier: under load the work queue can hand BOTH entries
/// to ONE worker (no second party ever arrives), and a hard 2-party Barrier then hangs the
/// whole bin — the collision simply didn't happen that run, and the tests' event assertions
/// still hold. Once released (by rendezvous or timeout), later enters pass through.
fn hook_with_gate(
    events: Arc<Mutex<Vec<(String, String)>>>,
    gate: impl Fn(&str) -> bool + Send + Sync + 'static,
) -> SchedHook {
    let state = Arc::new((
        Mutex::new((
            std::collections::HashSet::<std::thread::ThreadId>::new(),
            false,
        )),
        std::sync::Condvar::new(),
    ));
    SchedHook(Arc::new(move |point: &str, key: &str| {
        events
            .lock()
            .unwrap()
            .push((point.to_string(), key.to_string()));
        if point == "enter" && gate(key) {
            let (lock, cv) = &*state;
            let mut g = lock.lock().unwrap();
            if g.1 {
                return; // already released — pass through
            }
            g.0.insert(std::thread::current().id());
            if g.0.len() >= 2 {
                g.1 = true;
                cv.notify_all();
                return;
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !g.1 {
                let now = std::time::Instant::now();
                if now >= deadline {
                    g.1 = true; // solo run: release everyone, never hang
                    cv.notify_all();
                    break;
                }
                let (g2, _) = cv.wait_timeout(g, deadline - now).unwrap();
                g = g2;
            }
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
    flags.sched_hook = Some(hook_with_gate(events.clone(), |k| {
        k.starts_with("@unvendored_zzz")
    }));
    let (report, _) = load_tree_report_with_threads(
        &root,
        flags,
        &["a".to_string(), "b".to_string()],
        Vec::new(),
        2,
    );
    let _ = std::fs::remove_dir_all(&root);
    for (pkg, r) in &report {
        assert!(
            r.is_err(),
            "{pkg} must fail LOUDLY on the unvendored dep: {r:?}"
        );
    }
    assert_eq!(
        count(&events, "takeover-timeout", ""),
        0,
        "no waiter may hit the 20s backstop"
    );
    assert!(
        count(&events, "finish-err", "@unvendored_zzz") >= 1,
        "the failed load must finish its claim (the leak regression): {:?}",
        events.lock().unwrap()
    );
}

/// Bug #5 regression: a cross-thread PACKAGE cycle (a's drive demands b, b's drive demands a;
/// no target cycle) must resolve without the 20s timeout. Depending on exact scheduling, the
/// second demander may see `cycle-proceed` or the owner may finish first and produce `ready`.
/// Consumers read dep FILES only: provider flow through a cycle's partial state is the known
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
    assert_eq!(
        count(&events, "takeover-timeout", ""),
        0,
        "cycles must not reach the backstop"
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
        assert!(
            r.is_err(),
            "{pkg} must surface c's package-in-error loudly: {r:?}"
        );
        let msg = r.as_ref().unwrap_err();
        assert!(
            msg.contains("not vendored"),
            "{pkg} must carry the REAL error: {msg}"
        );
    }
    assert_eq!(
        count(&events, "own", "@unvendored_zzz"),
        1,
        "the failing dep load must run ONCE — later consumers read the cached error: {:?}",
        events.lock().unwrap()
    );
}

/// F3 (demand futures): a CycleProceed reader demanding a declaration its owner has NOT
/// driven yet must WAIT for the owner's `record_target` (the per-declaration future), not
/// error on the partial state. The forced order: p1 declares the demander BEFORE the
/// demanded target (u1 first, t4 second); B (driving p2) is hook-held at its `//slow`
/// demand — a key only B reaches, AFTER recording t2 and BEFORE demanding t4 — so the sole
/// gated arrival rides the 5s release, inside which A deterministically graph-parks on p2
/// (its u1 → t2 demand). B's t4 walk then always sees the cycle. Pre-fix: B cycle-proceeds
/// into p1's earliest state and errors "`//p1:t4` is neither a declared target nor a source
/// file". Post-fix: B parks on the declaration future; the insert wakes A (the package
/// waiter), A cycle-proceeds (t2 IS recorded), drives t4, and the record publishes B
/// onward. Consumers read dep FILES only (provider instances through a cycle remain the
/// restart pass's contract — F4).
#[test]
fn cycle_reader_waits_for_undriven_declaration() {
    let root = std::env::temp_dir().join(format!("razel-seam-declwait-{}", std::process::id()));
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
    std::fs::create_dir_all(root.join("slow")).unwrap();
    std::fs::write(root.join("slow/x.txt"), "x").unwrap();
    std::fs::write(
        root.join("slow/BUILD"),
        "filegroup(name = \"x\", srcs = [\"x.txt\"])\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("p1")).unwrap();
    std::fs::write(
        root.join("p1/BUILD"),
        "load(\"//defs:info.bzl\", \"lib\", \"use\")\n\
         use(name = \"u1\", deps = [\"//p2:t2\"])\n\
         lib(name = \"t4\")\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("p2")).unwrap();
    std::fs::write(
        root.join("p2/BUILD"),
        "load(\"//defs:info.bzl\", \"lib\", \"use\")\n\
         lib(name = \"t2\")\n\
         use(name = \"mid\", deps = [\"//slow:x\"])\n\
         use(name = \"u2\", deps = [\"//p1:t4\"])\n",
    )
    .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut flags = GlobalFlags::default();
    flags.sched_hook = Some(hook_with_gate(events.clone(), |k| k == "slow"));
    let (report, _) = load_tree_report_with_threads(
        &root,
        flags,
        &["p1".to_string(), "p2".to_string()],
        Vec::new(),
        2,
    );
    let _ = std::fs::remove_dir_all(&root);
    for (pkg, r) in &report {
        assert!(
            r.is_ok(),
            "the cycle reader must wait for the declaration: {pkg}: {r:?}"
        );
    }
    assert_eq!(
        count(&events, "takeover-timeout", ""),
        0,
        "no waiter may hit the 20s backstop"
    );
    assert_eq!(
        count(&events, "decl-done", "//p1:t4"),
        1,
        "the owner's record must publish the awaited declaration: {:?}",
        events.lock().unwrap()
    );
}

/// F4 (restart): the MUTUAL-UNDRIVEN-DECLARATION shape — each entry's drive demands a
/// declaration the OTHER entry has declared but not yet driven (each is parked before its
/// own producer target). The decl-future waits form an all-Decl cycle: the workers' drive
/// loops ARE the publishers, and both are parked — a true cross-thread deadlock that the
/// cycle rule resolves by failing the later walker (proceed-partial), sweeping the other
/// onto the same fallthrough. Sequentially BOTH packages load (the nested dep-load defers
/// and harvests). The tree driver must RESTART failed entries that consumed cross-thread
/// partial reads once the pool drains — both green, matching the sequential sweep.
#[test]
fn mutual_undriven_decl_demands_restart_to_parity() {
    let root = std::env::temp_dir().join(format!("razel-seam-restart-{}", std::process::id()));
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
    for s in ["slowa", "slowb"] {
        std::fs::create_dir_all(root.join(s)).unwrap();
        std::fs::write(root.join(s).join("x.txt"), "x").unwrap();
        std::fs::write(
            root.join(s).join("BUILD"),
            "filegroup(name = \"x\", srcs = [\"x.txt\"])\n",
        )
        .unwrap();
    }
    // Drive order is the trap: each entry demands the OTHER's last-declared target while
    // its own producer is still undriven. The slowa/slowb demands are the rendezvous —
    // keys only one worker each reaches, AFTER declaring, BEFORE the cross-demand.
    std::fs::create_dir_all(root.join("p1")).unwrap();
    std::fs::write(
        root.join("p1/BUILD"),
        "load(\"//defs:info.bzl\", \"lib\", \"use\")\n\
         use(name = \"g1\", deps = [\"//slowa:x\"])\n\
         use(name = \"u1\", deps = [\"//p2:t2\"])\n\
         lib(name = \"t4\")\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("p2")).unwrap();
    std::fs::write(
        root.join("p2/BUILD"),
        "load(\"//defs:info.bzl\", \"lib\", \"use\")\n\
         use(name = \"g2\", deps = [\"//slowb:x\"])\n\
         use(name = \"u2\", deps = [\"//p1:t4\"])\n\
         lib(name = \"t2\")\n",
    )
    .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut flags = GlobalFlags::default();
    flags.sched_hook = Some(hook_with_gate(events.clone(), |k| {
        k == "slowa" || k == "slowb"
    }));
    let (report, _) = load_tree_report_with_threads(
        &root,
        flags,
        &["p1".to_string(), "p2".to_string()],
        Vec::new(),
        2,
    );
    let _ = std::fs::remove_dir_all(&root);
    for (pkg, r) in &report {
        assert!(
            r.is_ok(),
            "restart must recover the partial-read failure: {pkg}: {r:?}"
        );
    }
    assert_eq!(
        count(&events, "takeover-timeout", ""),
        0,
        "no waiter may hit the 20s backstop"
    );
}

/// F2 (demand futures): two workers demanding the same DEFERRED NATIVE body (FnOnce —
/// `Session.native_decls[i].take()`) must single-flight: the loser WAITS for the runner's
/// record instead of seeing an empty slot and erroring "neither a declared target nor a
/// source file" (the wrong reason). The runner is held mid-body (inside its `//slow` srcs
/// demand) so the loser's window is deterministic: post-fix the loser's claim is the gate's
/// second arrival; pre-fix the loser never gates and fails inside the 5s window.
#[test]
fn deferred_native_demand_single_flights() {
    let root = std::env::temp_dir().join(format!("razel-seam-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("n")).unwrap();
    std::fs::create_dir_all(root.join("slow")).unwrap();
    std::fs::write(root.join("slow/x.txt"), "x").unwrap();
    std::fs::write(
        root.join("slow/BUILD"),
        "filegroup(name = \"x\", srcs = [\"x.txt\"])\n",
    )
    .unwrap();
    // n is only ever DEP-loaded (not a sweep entry), so its native decl defers to
    // `deferred_natives` and the demand-run races.
    std::fs::write(
        root.join("n/BUILD"),
        "filegroup(name = \"gen\", srcs = [\"//slow:x\"])\n",
    )
    .unwrap();
    for pkg in ["a", "b"] {
        std::fs::create_dir_all(root.join(pkg)).unwrap();
        std::fs::write(
            root.join(pkg).join("BUILD"),
            format!("filegroup(name = \"{pkg}\", srcs = [\"//n:gen\"])\n"),
        )
        .unwrap();
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut flags = GlobalFlags::default();
    flags.sched_hook = Some(hook_with_gate(events.clone(), |k| {
        k == "slow" || k == "//n:gen"
    }));
    let (report, _) = load_tree_report_with_threads(
        &root,
        flags,
        &["a".to_string(), "b".to_string()],
        Vec::new(),
        2,
    );
    let _ = std::fs::remove_dir_all(&root);
    for (pkg, r) in &report {
        assert!(
            r.is_ok(),
            "the FnOnce loser must wait, not error: {pkg}: {r:?}"
        );
    }
    assert_eq!(
        count(&events, "takeover-timeout", ""),
        0,
        "no waiter may hit the 20s backstop"
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
    assert_eq!(
        count(&events, "own", "info.bzl"),
        1,
        "exactly ONE eval of the module"
    );
    assert_eq!(
        count(&events, "ready", "info.bzl"),
        1,
        "the loser must wait and read the cache"
    );
    assert_eq!(count(&events, "takeover-timeout", ""), 0);
}
