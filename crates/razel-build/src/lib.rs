//! The build driver — bridges analysis → action → execution (Phase 3 ↔ 5 integration).
//!
//! Takes a Starlark-defined target through the whole stack: `analyze_starlark` runs the
//! rule impl and captures its actions, each action is converted to a content-addressed
//! `razel_actions::Action`, and `razel_exec` runs it in the exec root (cache hit → 0 exec).
//! This is the first point where razel **actually builds** from a rule.
//!
//! SCOPE: single package, inputs assumed materialized in the exec root; no toolchain
//! selection / `define_config` sugar yet (the rule emits argv directly via `ctx.actions.run`).
//! That + the link-with-deps cross-target flow are the next increments (D7).

pub mod incremental;
pub use incremental::IncrementalBuilder;

use razel_actions::Action;
use razel_analysis::wire_to_ir;
use razel_core::{Digest, FileId, TargetId};
use razel_exec::{Cache, build_action};
use razel_ir::TargetKind;
use razel_loading::{
    analyze_bazel_with, analyze_starlark, analyze_workspace_resolved, load_tree_report_with_targets,
};
// Re-exported so the daemon/clients can hold warm analysis (the analyze/execute split).
pub use razel_loading::{
    AnalyzedTarget, GlobalFlags, config_segment, convenience_symlinks, resolve_build_file,
};

/// Build `target` from a **real Bazel `BUILD`** (loads cc rules from `@rules_cc`,
/// resolved to razel's native rules). Analysis + execution; single-package.
pub fn build_bazel(
    build_src: &str,
    target: &str,
    exec_root: &Path,
    cache: &Cache,
) -> Result<BuildReport, String> {
    build_bazel_with(build_src, target, exec_root, cache, GlobalFlags::default())
}

/// [`build_bazel`] with build-wide [`GlobalFlags`] (the CLI's `-c`/`--copt`/`--linkopt`/…)
/// applied to every cc action.
pub fn build_bazel_with(
    build_src: &str,
    target: &str,
    exec_root: &Path,
    cache: &Cache,
    flags: GlobalFlags,
) -> Result<BuildReport, String> {
    let jobs = flags.jobs;
    execute_jobs(
        &analyze_bazel_with(build_src, flags)?,
        target,
        exec_root,
        cache,
        jobs,
    )
}

/// Build `top_label` (`//pkg:name`) from a **multi-package Bazel workspace** rooted
/// at `root`, loading dependency packages on demand. exec_root = the workspace root
/// (paths are package-qualified, matching Bazel's workspace-relative includes).
pub fn build_workspace(root: &Path, top_label: &str, cache: &Cache) -> Result<BuildReport, String> {
    build_workspace_with(root, top_label, cache, GlobalFlags::default())
}

/// [`build_workspace`] with build-wide [`GlobalFlags`] applied to every cc action.
pub fn build_workspace_with(
    root: &Path,
    top_label: &str,
    cache: &Cache,
    flags: GlobalFlags,
) -> Result<BuildReport, String> {
    let jobs = flags.jobs;
    // B4: an external-repo top label is an ALIAS (`@crates//:blake3`) whose analysis keys the target
    // under its RESOLVED canonical name (`@@rules_rust++crate+crates__blake3-1.8.2//:blake3`). Start
    // execution from that resolved name — `collect_order` would otherwise not find the apparent alias.
    let (targets, resolved) = analyze_workspace_resolved(root, top_label, flags)?;
    execute_jobs(&targets, &resolved, root, cache, jobs)
}
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::{Condvar, Mutex};

/// Expand a Bazel target PATTERN to concrete target labels under `root`. Supports `//...`,
/// `//...:all`, `//pkg/...`, `//pkg/...:all`, and `//pkg:all`; a concrete label (`//pkg:name`,
/// bare name — no `...`/`:all`) is returned as-is (no discovery). Discovers packages
/// (BUILD/BUILD.bazel/BUILD.razel), keeps the ones the pattern's package part selects,
/// analyzes them once, and returns the matching target labels (sorted, deduped).
pub fn expand_pattern(root: &Path, pattern: &str, flags: GlobalFlags) -> Result<Vec<String>, String> {
    if !pattern.contains("...") && !pattern.ends_with(":all") {
        return Ok(vec![pattern.to_string()]); // concrete — no discovery
    }
    let body = pattern.strip_prefix("//").unwrap_or(pattern);
    let pkgs = razel_loading::discover_packages(root, flags.strict_bazel);
    let matched: Vec<String> = if body == "..." || body == "...:all" {
        pkgs
    } else if let Some(pfx) = body
        .strip_suffix("/...:all")
        .or_else(|| body.strip_suffix("/..."))
    {
        pkgs.into_iter().filter(|p| p == pfx || p.starts_with(&format!("{pfx}/"))).collect()
    } else {
        // `//pkg:all` → that one package.
        let pkg = body.split_once(':').map(|(p, _)| p).unwrap_or(body);
        pkgs.into_iter().filter(|p| p == pkg).collect()
    };
    if matched.is_empty() {
        return Err(format!("no packages match `{pattern}` under {}", root.display()));
    }
    let (_report, _loaded, targets) =
        load_tree_report_with_targets(root, flags, &matched, Vec::new(), 1);
    let mut labels: Vec<String> = targets.into_iter().map(|t| t.name).collect();
    labels.sort();
    labels.dedup();
    if labels.is_empty() {
        return Err(format!("`{pattern}` matched {} package(s) but no targets", matched.len()));
    }
    Ok(labels)
}

// `discover_packages` moved to `razel_loading::patterns` (P1.2 / R1#1) so query enumerates without
// the build driver; `expand_pattern` above calls it via `razel_loading::discover_packages`.

/// A target surfaced by the impact query: its canonical label + coarse kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AffectedTarget {
    pub label: String,
    pub kind: TargetKind,
}

/// The reverse (rdep) impact of changing `sources`: the affected deliverables and
/// tests. This is the AI-agent / test-selection query — "edit these files → rebuild
/// these, re-run those" — answered by the IR's stored reverse edges (O(affected)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Affected {
    pub sources: Vec<String>,
    pub targets: Vec<AffectedTarget>,
    pub tests: Vec<AffectedTarget>,
}

/// Compute the impact of editing `files` (paths relative to `package`): analyze the
/// BUILD, wire it into the IR, and walk reverse edges from each file to its dependent
/// targets. No execution — a pure graph query.
pub fn affected(build_src: &str, package: &str, files: &[String]) -> Result<Affected, String> {
    let analyzed = analyze_starlark("BUILD", build_src)?;
    let g = wire_to_ir(package, &analyzed);

    let mut tests = BTreeSet::new();
    let mut deliverables = BTreeSet::new();
    for f in files {
        let fid = FileId::new(format!("{package}/{f}"));
        let (t, d) = g.impacted_targets(&fid);
        tests.extend(t);
        deliverables.extend(d);
    }

    let to_ref = |tid: &TargetId| AffectedTarget {
        label: tid.0.clone(),
        kind: g.target(tid).map(|n| n.kind).unwrap_or(TargetKind::Library),
    };
    Ok(Affected {
        sources: files.to_vec(),
        targets: deliverables.iter().map(to_ref).collect(),
        tests: tests.iter().map(to_ref).collect(),
    })
}

/// Post-order DFS over `deps` → targets ordered deps-first (a target's deps execute before it).
fn collect_order(
    name: &str,
    by_name: &HashMap<String, AnalyzedTarget>,
    order: &mut Vec<String>,
    seen: &mut HashSet<String>,
) -> Result<(), String> {
    if !seen.insert(name.to_string()) {
        return Ok(());
    }
    let t = by_name
        .get(name)
        .ok_or_else(|| format!("unknown target: {name}"))?;
    for d in &t.deps {
        collect_order(d, by_name, order, seen)?;
    }
    order.push(name.to_string());
    Ok(())
}

/// The outcome of a build: the produced output paths (in order) and how many
/// actions actually **executed** (cache misses). `executed == 0` means the whole
/// target was served from cache — the incremental "nothing to do" signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildReport {
    pub produced: Vec<String>,
    pub executed: usize,
    /// The REQUESTED target's DefaultInfo (bazel semantics: the build's outputs —
    /// what `run` execs, what clients see; `produced` keeps every intermediate).
    pub default_outputs: Vec<String>,
}

/// Build a target and its transitive deps: analyze, order deps-first, and execute every
/// action in `exec_root` (cache hit → 0 exec). Returns the produced output paths in order.
pub fn build_target(
    build_src: &str,
    target: &str,
    exec_root: &Path,
    cache: &Cache,
) -> Result<Vec<String>, String> {
    Ok(build_target_report(build_src, target, exec_root, cache)?.produced)
}

/// Analysis only: run the BUILD's rules and capture each target's actions. Split out
/// from execution so a warm daemon can cache it and skip re-parsing an unchanged BUILD.
pub fn analyze_build(build_src: &str) -> Result<Vec<AnalyzedTarget>, String> {
    analyze_starlark("BUILD", build_src)
}

/// Like [`build_target`] but also reports how many actions executed (cache misses) —
/// the basis for `Cached` vs `Built` status and the `recomputes` metric.
pub fn build_target_report(
    build_src: &str,
    target: &str,
    exec_root: &Path,
    cache: &Cache,
) -> Result<BuildReport, String> {
    execute(&analyze_build(build_src)?, target, exec_root, cache)
}

/// Run ONE target's actions in `exec_root` against `cache` (cache hit → 0 exec). The unit
/// shared by the serial and parallel drivers, so `-j1` and `-jN` run identical action/cache
/// semantics — only the *order across independent targets* differs. Returns
/// `(executed_count, produced_paths)`.
fn run_one_target(
    t: &AnalyzedTarget,
    exec_root: &Path,
    cache: &Cache,
) -> Result<(usize, Vec<String>), String> {
    let mut executed = 0;
    let mut produced = Vec::new();
    for act in &t.actions {
        // Digest the declared inputs that exist on disk → the action's content key.
        let mut inputs = BTreeMap::new();
        for inp in &act.inputs {
            if let Ok(bytes) = std::fs::read(exec_root.join(inp)) {
                inputs.insert(inp.clone(), Digest::of(&bytes));
            }
        }
        let action = Action {
            argv: act.argv.clone(),
            inputs,
            env: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
            tools: BTreeMap::new(),
            platform: "host".into(),
            outputs: act.outputs.clone(),
        };
        let r = build_action(&action, cache, exec_root).map_err(|e| e.to_string())?;
        if r.exit_code != 0 {
            return Err(format!("action failed ({}): {:?}", r.exit_code, act.argv));
        }
        if !r.cached {
            executed += 1;
        }
        produced.extend(act.outputs.clone());
    }
    Ok((executed, produced))
}

/// Execute a pre-analyzed target graph: order deps-first and run every action in
/// `exec_root` (cache hit → 0 exec). Serial — equivalent to [`execute_jobs`] with `jobs=1`.
/// Separated from [`analyze_build`] so callers (the daemon) can reuse warm analysis.
pub fn execute(
    targets: &[AnalyzedTarget],
    target: &str,
    exec_root: &Path,
    cache: &Cache,
) -> Result<BuildReport, String> {
    execute_jobs(targets, target, exec_root, cache, 1)
}

/// [`execute`] with up to `jobs` targets running CONCURRENTLY (S5x). A Kahn ready-queue over
/// the dep DAG: a target runs only once all its deps complete, so the shared `exec_root` sees
/// the same writes-before-reads a serial build would; independent targets (same topo layer)
/// run in parallel WITHOUT a barrier (a finished target unblocks its dependents immediately —
/// no waiting on a slow sibling). `jobs<=1` is the plain serial walk. The report is
/// canonicalised to the topo order, so `-j1` and `-jN` are byte-identical (the determinism
/// bar); the per-action outputs are content-addressed, hence identical regardless of order.
pub fn execute_jobs(
    targets: &[AnalyzedTarget],
    target: &str,
    exec_root: &Path,
    cache: &Cache,
    jobs: usize,
) -> Result<BuildReport, String> {
    let by_name: HashMap<String, AnalyzedTarget> = targets
        .iter()
        .map(|t| (t.name.clone(), t.clone()))
        .collect();

    let mut order = Vec::new();
    collect_order(target, &by_name, &mut order, &mut HashSet::new())?;
    let n = order.len();
    // Bazel semantics: a build's OUTPUTS are the requested target's DefaultInfo, not every
    // intermediate (post-order ⇒ the requested target is last in `order`).
    let default_outputs = order
        .last()
        .and_then(|t| by_name.get(t))
        .map(|t| t.default_info.clone())
        .unwrap_or_default();

    if jobs <= 1 {
        let mut produced = Vec::new();
        let mut executed = 0;
        for tname in &order {
            let (e, p) = run_one_target(&by_name[tname], exec_root, cache)?;
            executed += e;
            produced.extend(p);
        }
        return Ok(BuildReport { produced, executed, default_outputs });
    }

    // Parallel. indegree[i] = unfinished in-graph deps of `order[i]`; rdeps[j] = the targets
    // that depend on j (the edges we relax when j completes).
    let pos: HashMap<&str, usize> =
        order.iter().enumerate().map(|(i, t)| (t.as_str(), i)).collect();
    let mut indeg = vec![0usize; n];
    let mut rdeps: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, tname) in order.iter().enumerate() {
        for d in &by_name[tname].deps {
            if let Some(&j) = pos.get(d.as_str()) {
                indeg[i] += 1;
                rdeps[j].push(i);
            }
        }
    }
    // Per-target result slots (each written by exactly one worker → no contention).
    let slots: Vec<Mutex<Option<(usize, Vec<String>)>>> =
        (0..n).map(|_| Mutex::new(None)).collect();

    // All scheduling state under ONE lock so readiness/active/completed stay consistent
    // (no cross-atomic races); `active` = targets currently running, used to detect a stall
    // (ready-empty + active==0 + work-remaining ⇒ a dependency cycle).
    struct Sched {
        ready: VecDeque<usize>,
        indeg: Vec<usize>,
        active: usize,
        completed: usize,
        err: Option<String>,
    }
    let sched = Mutex::new(Sched {
        ready: (0..n).filter(|&i| indeg[i] == 0).collect(),
        indeg,
        active: 0,
        completed: 0,
        err: None,
    });
    let cv = Condvar::new();

    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| {
                loop {
                    let i = {
                        let mut s = sched.lock().expect("sched");
                        loop {
                            if s.err.is_some() || s.completed == n {
                                return;
                            }
                            if let Some(i) = s.ready.pop_front() {
                                s.active += 1;
                                break i;
                            }
                            if s.active == 0 {
                                // Nothing ready, nothing running, work remains → cycle.
                                s.err = Some("dependency cycle in parallel execute".into());
                                cv.notify_all();
                                return;
                            }
                            s = cv.wait(s).expect("sched wait");
                        }
                    };
                    let res = run_one_target(&by_name[&order[i]], exec_root, cache);
                    let mut s = sched.lock().expect("sched");
                    match res {
                        Ok(r) => *slots[i].lock().expect("slot") = Some(r),
                        Err(e) => {
                            s.err = Some(e);
                            cv.notify_all();
                            return;
                        }
                    }
                    for &j in &rdeps[i] {
                        s.indeg[j] -= 1;
                        if s.indeg[j] == 0 {
                            s.ready.push_back(j);
                        }
                    }
                    s.active -= 1;
                    s.completed += 1;
                    cv.notify_all();
                }
            });
        }
    });

    let sched = sched.into_inner().expect("sched");
    if let Some(e) = sched.err {
        return Err(e);
    }
    // Flatten results in topo order → produced/executed independent of completion order.
    let mut produced = Vec::new();
    let mut executed = 0;
    for slot in &slots {
        let (e, p) = slot.lock().expect("slot").take().expect("every target ran");
        executed += e;
        produced.extend(p);
    }
    Ok(BuildReport { produced, executed, default_outputs })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILD: &str = r#"
def _impl(ctx):
    out = ctx.attr.name + ".o"
    ctx.actions.run(
        executable = "/usr/bin/cc",
        outputs = [out],
        inputs = [ctx.attr.src],
        arguments = ["-c", ctx.attr.src, "-o", out],
    )
    return [DefaultInfo(files = [out])]

cc_obj = rule(implementation = _impl, attrs = {"src": 1})
cc_obj(name = "widget", src = "widget.c")
"#;

    /// S4: a build's OUTPUTS are the requested target's DefaultInfo — never the
    /// intermediates (`run` execs outputs[0]; a cc_binary's first PRODUCED file is
    /// a .o, its DefaultInfo is the linked binary).
    #[test]
    fn report_default_outputs_are_the_targets_default_info() {
        use razel_loading::{AnalyzedAction, AnalyzedTarget};
        let exec = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(cache_dir.path()).unwrap();
        let touch = |out: &str| AnalyzedAction {
            mnemonic: "Touch".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), format!("touch {out}")],
            inputs: vec![],
            outputs: vec![out.into()],
        };
        let targets = vec![
            AnalyzedTarget {
                name: "//p:dep".into(),
                deps: vec![],
                actions: vec![touch("dep.txt")],
                default_info: vec!["dep.txt".into()],
                providers: Default::default(),
            },
            AnalyzedTarget {
                name: "//p:bin".into(),
                deps: vec!["//p:dep".into()],
                actions: vec![touch("a.o"), touch("bin")],
                default_info: vec!["bin".into()],
                providers: Default::default(),
            },
        ];
        let report = execute(&targets, "//p:bin", exec.path(), &cache).unwrap();
        assert_eq!(report.default_outputs, vec!["bin".to_string()]);
        assert_eq!(report.produced, vec!["dep.txt", "a.o", "bin"], "intermediates stay in produced");
    }

    #[test]
    fn compiles_a_real_object_through_the_rule_engine() {
        if !Path::new("/usr/bin/cc").exists() {
            return; // skip where no cc
        }
        let exec = tempfile::tempdir().unwrap();
        std::fs::write(exec.path().join("widget.c"), "int answer(void){return 42;}").unwrap();
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // First build: rule impl runs → emits a cc compile action → razel executes it.
        let produced = build_target(BUILD, "widget", exec.path(), &cache).unwrap();
        assert_eq!(produced, vec!["widget.o"]);
        let obj = exec.path().join("widget.o");
        assert!(obj.exists(), "razel did not produce widget.o");
        assert!(std::fs::metadata(&obj).unwrap().len() > 0);

        // Second build in a fresh exec root: cache hit restores the object (0 exec).
        let exec2 = tempfile::tempdir().unwrap();
        std::fs::write(
            exec2.path().join("widget.c"),
            "int answer(void){return 42;}",
        )
        .unwrap();
        let produced2 = build_target(BUILD, "widget", exec2.path(), &cache).unwrap();
        assert_eq!(produced2, vec!["widget.o"]);
        assert!(exec2.path().join("widget.o").exists());
    }

    #[test]
    fn report_counts_executed_then_zero_on_cache_hit() {
        if !Path::new("/usr/bin/cc").exists() {
            return;
        }
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // Cold build: the one compile action executes (a cache miss).
        let exec = tempfile::tempdir().unwrap();
        std::fs::write(exec.path().join("widget.c"), "int answer(void){return 42;}").unwrap();
        let r1 = build_target_report(BUILD, "widget", exec.path(), &cache).unwrap();
        assert_eq!(r1.produced, vec!["widget.o"]);
        assert_eq!(r1.executed, 1, "cold build executes the action");

        // Warm rebuild in a fresh exec root: same content key → cache hit → 0 executed.
        let exec2 = tempfile::tempdir().unwrap();
        std::fs::write(
            exec2.path().join("widget.c"),
            "int answer(void){return 42;}",
        )
        .unwrap();
        let r2 = build_target_report(BUILD, "widget", exec2.path(), &cache).unwrap();
        assert_eq!(r2.produced, vec!["widget.o"]);
        assert_eq!(
            r2.executed, 0,
            "warm rebuild is fully cached — nothing recomputed"
        );
    }

    // The cpp-tutorial stage1 BUILD, verbatim — a real Bazel BUILD with a load().
    const CPP_TUTORIAL_STAGE1: &str = r#"
load("@rules_cc//cc:cc_binary.bzl", "cc_binary")

cc_binary(
    name = "hello-world",
    srcs = ["hello-world.cc"],
)
"#;

    #[test]
    fn builds_and_runs_real_bazel_cpp_tutorial_stage1() {
        if !Path::new("/usr/bin/c++").exists() {
            return; // no C++ driver
        }
        let exec = tempfile::tempdir().unwrap();
        // The actual stage1 source (std::iostream/string/ctime; no deps/headers).
        std::fs::write(
            exec.path().join("hello-world.cc"),
            r#"#include <iostream>
#include <string>
int main(int argc, char** argv) {
    std::string who = argc > 1 ? argv[1] : "world";
    std::cout << "Hello " << who << std::endl;
    return 0;
}
"#,
        )
        .unwrap();
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // razel loads the real BUILD (resolving @rules_cc → native cc_binary) and builds.
        let report = build_bazel(CPP_TUTORIAL_STAGE1, "hello-world", exec.path(), &cache).unwrap();
        assert_eq!(report.produced, vec!["hello-world.cc.o", "hello-world"]); // compile + link outputs
        assert_eq!(report.executed, 2, "compile + link");

        // The produced binary runs and prints the greeting.
        let bin = exec.path().join("hello-world");
        assert!(bin.exists(), "razel did not produce the binary");
        let out = std::process::Command::new(&bin).output().unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Hello world");
    }

    // cpp-tutorial stage2, verbatim: a cc_library + a cc_binary depending on it.
    const CPP_TUTORIAL_STAGE2: &str = r#"
load("@rules_cc//cc:cc_binary.bzl", "cc_binary")
load("@rules_cc//cc:cc_library.bzl", "cc_library")

cc_library(
    name = "hello-greet",
    srcs = ["hello-greet.cc"],
    hdrs = ["hello-greet.h"],
)

cc_binary(
    name = "hello-world",
    srcs = ["hello-world.cc"],
    deps = [
        ":hello-greet",
    ],
)
"#;

    #[test]
    fn builds_and_runs_real_bazel_cpp_tutorial_stage2() {
        if !Path::new("/usr/bin/c++").exists() || !Path::new("/usr/bin/ar").exists() {
            return;
        }
        let exec = tempfile::tempdir().unwrap();
        let w = |name: &str, body: &str| std::fs::write(exec.path().join(name), body).unwrap();
        w(
            "hello-greet.h",
            "#ifndef HELLO_GREET_H_\n#define HELLO_GREET_H_\n#include <string>\nstd::string get_greet(const std::string& who);\n#endif\n",
        );
        w(
            "hello-greet.cc",
            "#include \"hello-greet.h\"\nstd::string get_greet(const std::string& who) { return \"Hello \" + who; }\n",
        );
        w(
            "hello-world.cc",
            "#include \"hello-greet.h\"\n#include <iostream>\nint main(int argc, char** argv) {\n  std::string who = argc > 1 ? argv[1] : \"world\";\n  std::cout << get_greet(who) << std::endl;\n  return 0;\n}\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        let report = build_bazel(CPP_TUTORIAL_STAGE2, "hello-world", exec.path(), &cache).unwrap();
        // deps-first: compile+archive the lib, then compile+link the binary.
        assert_eq!(report.executed, 4, "2 compiles + archive + link");
        assert!(report.produced.contains(&"libhello-greet.a".to_string()));
        assert!(report.produced.contains(&"hello-world".to_string()));

        let out = std::process::Command::new(exec.path().join("hello-world"))
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Hello world");
    }

    #[test]
    fn builds_and_runs_real_bazel_cpp_tutorial_stage3() {
        if !Path::new("/usr/bin/c++").exists() || !Path::new("/usr/bin/ar").exists() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let w = |rel: &str, body: &str| {
            let p = root.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        // lib package
        w(
            "lib/BUILD",
            r#"load("@rules_cc//cc:cc_library.bzl", "cc_library")
cc_library(name = "hello-time", srcs = ["hello-time.cc"], hdrs = ["hello-time.h"], visibility = ["//main:__pkg__"])
"#,
        );
        w(
            "lib/hello-time.h",
            "#ifndef LIB_HELLO_TIME_H_\n#define LIB_HELLO_TIME_H_\nvoid print_localtime();\n#endif\n",
        );
        w(
            "lib/hello-time.cc",
            "#include \"lib/hello-time.h\"\n#include <ctime>\n#include <iostream>\nvoid print_localtime() { std::time_t t = std::time(nullptr); std::cout << std::asctime(std::localtime(&t)); }\n",
        );
        // main package — depends on //lib:hello-time AND :hello-greet
        w(
            "main/BUILD",
            r#"load("@rules_cc//cc:cc_binary.bzl", "cc_binary")
load("@rules_cc//cc:cc_library.bzl", "cc_library")
cc_library(name = "hello-greet", srcs = ["hello-greet.cc"], hdrs = ["hello-greet.h"])
cc_binary(name = "hello-world", srcs = ["hello-world.cc"], deps = [":hello-greet", "//lib:hello-time"])
"#,
        );
        w(
            "main/hello-greet.h",
            "#ifndef MAIN_HELLO_GREET_H_\n#define MAIN_HELLO_GREET_H_\n#include <string>\nstd::string get_greet(const std::string& who);\n#endif\n",
        );
        w(
            "main/hello-greet.cc",
            "#include \"main/hello-greet.h\"\nstd::string get_greet(const std::string& who) { return \"Hello \" + who; }\n",
        );
        w(
            "main/hello-world.cc",
            "#include \"lib/hello-time.h\"\n#include \"main/hello-greet.h\"\n#include <iostream>\nint main(int argc, char** argv) {\n  std::string who = argc > 1 ? argv[1] : \"world\";\n  std::cout << get_greet(who) << std::endl;\n  print_localtime();\n  return 0;\n}\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // Cross-package: //lib:hello-time is loaded on demand while analyzing main.
        let report = build_workspace(root.path(), "//main:hello-world", &cache).unwrap();
        assert_eq!(report.executed, 6, "3 compiles + 2 archives + 1 link");
        assert!(report.produced.contains(&"main/hello-world".to_string()));
        assert!(report.produced.contains(&"lib/libhello-time.a".to_string()));

        let out = std::process::Command::new(root.path().join("main/hello-world"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).starts_with("Hello world"),
            "stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    #[test]
    fn builds_a_bazel_target_with_glob_srcs() {
        if !Path::new("/usr/bin/c++").exists() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let w = |rel: &str, body: &str| {
            let p = root.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        // srcs via glob — the rule never names the files; razel scans the package dir.
        w(
            "app/BUILD",
            "load(\"@rules_cc//cc:cc_binary.bzl\", \"cc_binary\")\ncc_binary(name = \"prog\", srcs = glob([\"*.cc\"]))\n",
        );
        w(
            "app/main.cc",
            "#include <iostream>\nconst char* who();\nint main() { std::cout << \"Hello \" << who() << std::endl; return 0; }\n",
        );
        w("app/who.cc", "const char* who() { return \"world\"; }\n");
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        let report = build_workspace(root.path(), "//app:prog", &cache).unwrap();
        assert_eq!(
            report.executed, 3,
            "glob found 2 srcs → 2 compiles + 1 link"
        );
        assert!(report.produced.contains(&"app/prog".to_string()));

        let out = std::process::Command::new(root.path().join("app/prog"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Hello world");
    }

    #[test]
    fn copts_and_propagated_defines_reach_the_compiler() {
        if !Path::new("/usr/bin/c++").exists() || !Path::new("/usr/bin/ar").exists() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let w = |rel: &str, body: &str| {
            let p = root.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        // lib exports a define; the binary inherits it (propagation) and adds its own
        // via copts (local). main computes LIBVAL + BINVAL.
        w(
            "lib/BUILD",
            "load(\"@rules_cc//cc:cc_library.bzl\", \"cc_library\")\ncc_library(name = \"v\", srcs = [\"v.cc\"], defines = [\"LIBVAL=10\"])\n",
        );
        w("lib/v.cc", "int v_unused() { return 0; }\n");
        w(
            "app/BUILD",
            "load(\"@rules_cc//cc:cc_binary.bzl\", \"cc_binary\")\ncc_binary(name = \"prog\", srcs = [\"main.cc\"], deps = [\"//lib:v\"], copts = [\"-DBINVAL=5\"])\n",
        );
        w(
            "app/main.cc",
            "#include <iostream>\nint main() { std::cout << (LIBVAL + BINVAL) << std::endl; return 0; }\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // Without copts/defines wired, main.cc wouldn't compile (LIBVAL/BINVAL undefined).
        build_workspace(root.path(), "//app:prog", &cache).unwrap();
        let out = std::process::Command::new(root.path().join("app/prog"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "15");
    }

    #[test]
    fn global_copts_reach_every_compile() {
        if !Path::new("/usr/bin/c++").exists() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("BUILD"),
            "load(\"@rules_cc//cc:cc_binary.bzl\", \"cc_binary\")\ncc_binary(name = \"prog\", srcs = [\"main.cc\"])\n",
        )
        .unwrap();
        // No -DANSWER in the BUILD — it must arrive via the global flag (CLI -c/--copt).
        std::fs::write(
            root.path().join("main.cc"),
            "#include <iostream>\nint main() { std::cout << ANSWER << std::endl; return 0; }\n",
        )
        .unwrap();
        let build_src = std::fs::read_to_string(root.path().join("BUILD")).unwrap();
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        let flags = GlobalFlags {
            copts: vec!["-DANSWER=42".into()],
            linkopts: vec![],
            ..Default::default()
        };
        build_bazel_with(&build_src, "prog", root.path(), &cache, flags).unwrap();

        let out = std::process::Command::new(root.path().join("prog"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "42");
    }

    #[test]
    fn builds_via_a_custom_bzl_macro() {
        if !Path::new("/usr/bin/c++").exists() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let w = |rel: &str, body: &str| {
            let p = root.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        // A repo-defined macro in its own .bzl — razel evaluates it (not just resolves
        // @rules_cc), so the BUILD can call cc_app() which wraps cc_binary.
        w(
            "tools/defs.bzl",
            "load(\"@rules_cc//cc:cc_binary.bzl\", \"cc_binary\")\ndef cc_app(name, srcs):\n    cc_binary(name = name, srcs = srcs)\n",
        );
        w(
            "app/BUILD",
            "load(\"//tools:defs.bzl\", \"cc_app\")\ncc_app(name = \"prog\", srcs = [\"main.cc\"])\n",
        );
        w(
            "app/main.cc",
            "#include <iostream>\nint main() { std::cout << \"Hello world\" << std::endl; return 0; }\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        let report = build_workspace(root.path(), "//app:prog", &cache).unwrap();
        assert_eq!(
            report.executed, 2,
            "macro expanded to a cc_binary: compile + link"
        );
        assert!(report.produced.contains(&"app/prog".to_string()));

        let out = std::process::Command::new(root.path().join("app/prog"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Hello world");
    }

    #[test]
    fn build_package_declaration_builtins_are_no_ops() {
        // package()/package_group()/licenses()/exports_files() are recognized so real
        // BUILD files evaluate; razel treats them as no-op declarations.
        if !Path::new("/usr/bin/c++").exists() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let w = |rel: &str, body: &str| {
            let p = root.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        w(
            "pkg/BUILD",
            "load(\"@rules_cc//cc:cc_binary.bzl\", \"cc_binary\")\n\
package(default_visibility = [\"//visibility:public\"], licenses = [\"notice\"])\n\
licenses([\"notice\"])\n\
exports_files([\"data.txt\"], visibility = [\"//visibility:public\"])\n\
package_group(name = \"friends\", packages = [\"//...\"])\n\
cc_binary(name = \"prog\", srcs = [\"main.cc\"])\n",
        );
        w(
            "pkg/main.cc",
            "#include <iostream>\nint main() { std::cout << \"ok\" << std::endl; return 0; }\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // The no-op builtins must not derail evaluation; the cc_binary still builds.
        let report = build_workspace(root.path(), "//pkg:prog", &cache).unwrap();
        assert!(report.produced.contains(&"pkg/prog".to_string()));
        let out = std::process::Command::new(root.path().join("pkg/prog"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");
    }

    #[test]
    fn recognizes_build_graph_and_skylib_builtins() {
        // config_setting/filegroup/alias/test_suite + skylib (bzl_library/string_flag)
        // are recognized so a real BUILD evaluates; the cc_binary alongside still builds.
        if !Path::new("/usr/bin/c++").exists() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let w = |rel: &str, body: &str| {
            let p = root.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        w(
            "pkg/BUILD",
            "load(\"@rules_cc//cc:cc_binary.bzl\", \"cc_binary\")\n\
load(\"@bazel_skylib//:bzl_library.bzl\", \"bzl_library\")\n\
load(\"@bazel_skylib//rules:common_settings.bzl\", \"string_flag\")\n\
config_setting(name = \"dbg\", values = {\"compilation_mode\": \"dbg\"})\n\
filegroup(name = \"data\", srcs = [\"a.txt\"])\n\
alias(name = \"prog_alias\", actual = \":prog\")\n\
test_suite(name = \"all_tests\", tests = [])\n\
string_flag(name = \"mode\", build_setting_default = \"x\")\n\
bzl_library(name = \"defs_lib\", srcs = [\"defs.bzl\"])\n\
cc_binary(name = \"prog\", srcs = [\"main.cc\"])\n",
        );
        w(
            "pkg/main.cc",
            "#include <iostream>\nint main() { std::cout << \"ok\" << std::endl; return 0; }\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        let report = build_workspace(root.path(), "//pkg:prog", &cache).unwrap();
        assert!(report.produced.contains(&"pkg/prog".to_string()));
        let out = std::process::Command::new(root.path().join("pkg/prog"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");
    }

    #[test]
    fn label_native_and_autoconfig_helpers_evaluate() {
        // Label(), native.package_name(), and an auto-config helper (if_rocm_is_configured
        // → not configured) are exercised during BUILD eval; the cc_binary still builds
        // (gpu.cc is dropped because ROCm is "not configured").
        if !Path::new("/usr/bin/c++").exists() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let w = |rel: &str, body: &str| {
            let p = root.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        w(
            "pkg/BUILD",
            "load(\"@rules_cc//cc:cc_binary.bzl\", \"cc_binary\")\n\
load(\"@local_config_rocm//rocm:build_defs.bzl\", \"if_rocm_is_configured\")\n\
_lbl = Label(\"//pkg:prog\")\n\
_here = native.package_name()\n\
cc_binary(name = \"prog\", srcs = [\"main.cc\"] + if_rocm_is_configured([\"gpu.cc\"]))\n",
        );
        w(
            "pkg/main.cc",
            "#include <iostream>\nint main() { std::cout << \"ok\" << std::endl; return 0; }\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // gpu.cc doesn't exist; if the not-configured helper didn't drop it, this fails.
        let report = build_workspace(root.path(), "//pkg:prog", &cache).unwrap();
        assert!(report.produced.contains(&"pkg/prog".to_string()));
        let out = std::process::Command::new(root.path().join("pkg/prog"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");
    }

    #[test]
    fn stdlib_print_works_in_a_loaded_rule() {
        // Mirrors bazelbuild-examples rules/empty: a loaded .bzl rule whose impl
        // calls print() — exercises the standard-library globals (Print extension).
        let root = tempfile::tempdir().unwrap();
        let w = |rel: &str, body: &str| {
            let p = root.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        w(
            "pkg/empty.bzl",
            "def _impl(_):\n    print(\"this rule does nothing\")\nempty = rule(implementation = _impl)\n",
        );
        w(
            "pkg/BUILD",
            "load(\"//pkg:empty.bzl\", \"empty\")\nempty(name = \"nothing\")\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // No actions, no outputs — but it must analyze + "build" (0 executed) cleanly.
        let report = build_workspace(root.path(), "//pkg:nothing", &cache).unwrap();
        assert_eq!(report.executed, 0);
    }

    #[test]
    fn rule_writes_a_file_via_ctx_outputs_and_actions_write() {
        // Mirrors bazelbuild-examples rules/actions_write: attr.string/output schema,
        // ctx.outputs.<name>, and ctx.actions.write(content=) producing a real file.
        if !Path::new("/bin/sh").exists() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let w = |rel: &str, body: &str| {
            let p = root.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        w(
            "pkg/defs.bzl",
            "def _impl(ctx):\n    ctx.actions.write(output = ctx.outputs.out, content = ctx.attr.content)\n\
gen = rule(implementation = _impl, attrs = {\"content\": attr.string(), \"out\": attr.output()})\n",
        );
        w(
            "pkg/BUILD",
            "load(\"//pkg:defs.bzl\", \"gen\")\ngen(name = \"hi\", content = \"hello there\", out = \"hi.txt\")\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        let report = build_workspace(root.path(), "//pkg:hi", &cache).unwrap();
        assert!(report.produced.contains(&"pkg/hi.txt".to_string()));
        assert_eq!(
            std::fs::read_to_string(root.path().join("pkg/hi.txt")).unwrap(),
            "hello there"
        );
    }

    #[test]
    fn depset_flattens_direct_and_transitive_deduped() {
        // AE primitive: depset(direct, transitive=[depset]).to_list() — folds direct +
        // transitive members, de-duplicated, order-preserving.
        if !Path::new("/bin/sh").exists() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let w = |rel: &str, body: &str| {
            let p = root.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        w(
            "pkg/defs.bzl",
            "def _impl(ctx):\n    inner = depset([\"b\", \"c\"])\n    d = depset([\"a\", \"b\"], transitive = [inner])\n    ctx.actions.write(output = ctx.outputs.out, content = \",\".join(d.to_list()))\n\
gen = rule(implementation = _impl, attrs = {\"out\": attr.output()})\n",
        );
        w(
            "pkg/BUILD",
            "load(\"//pkg:defs.bzl\", \"gen\")\ngen(name = \"x\", out = \"x.txt\")\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        build_workspace(root.path(), "//pkg:x", &cache).unwrap();
        // direct [a,b], then transitive [b,c] → b deduped → a,b,c.
        assert_eq!(
            std::fs::read_to_string(root.path().join("pkg/x.txt")).unwrap(),
            "a,b,c"
        );
    }

    #[test]
    fn builds_via_a_rule_defined_in_a_loaded_bzl() {
        // The freeze-fix payoff: a custom `rule()` defined in a .bzl and load()ed.
        // Before RuleObj was freezable, freezing the loaded .bzl module errored here.
        if !Path::new("/bin/sh").exists() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let w = |rel: &str, body: &str| {
            let p = root.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        w(
            "tools/rules.bzl",
            "def _impl(ctx):\n    out = ctx.attr.name\n    ctx.actions.run(executable = \"/bin/sh\", arguments = [\"-c\", \"echo built > \" + out], outputs = [out], inputs = [])\n    return [DefaultInfo(files = [out])]\nmyrule = rule(implementation = _impl, attrs = {})\n",
        );
        w(
            "app/BUILD",
            "load(\"//tools:rules.bzl\", \"myrule\")\nmyrule(name = \"thing\")\n",
        );
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        let report = build_workspace(root.path(), "//app:thing", &cache).unwrap();
        assert!(report.produced.contains(&"thing".to_string()));
        assert_eq!(
            std::fs::read_to_string(root.path().join("thing"))
                .unwrap()
                .trim(),
            "built"
        );
    }

    #[test]
    fn affected_query_splits_tests_and_deliverables() {
        // A lib and a test both consume widget.c; analysis-only (no toolchain needed).
        let src = r#"
def _impl(ctx):
    out = ctx.attr.name + ".o"
    ctx.actions.run(executable = "cc", outputs = [out], inputs = [ctx.attr.src], arguments = [])
    return [DefaultInfo(files = [out])]
thing = rule(implementation = _impl, attrs = {"src": 1})
thing(name = "widget", src = "widget.c")
thing(name = "widget_test", src = "widget.c")
"#;
        let labels = |v: &[AffectedTarget]| v.iter().map(|t| t.label.clone()).collect::<Vec<_>>();

        let a = affected(src, "pkg", &["widget.c".to_string()]).unwrap();
        assert_eq!(labels(&a.targets), vec!["//pkg:widget"]); // deliverable
        assert_eq!(labels(&a.tests), vec!["//pkg:widget_test"]); // test (by suffix)

        // An unrelated file impacts nothing — the rdep walk is output-sensitive.
        let none = affected(src, "pkg", &["other.c".to_string()]).unwrap();
        assert!(none.targets.is_empty() && none.tests.is_empty());
    }

    // The D7 path: a `define_config` transform generates the compile command; the rule
    // unpacks it into ctx.actions.run; razel executes it into a real object.
    const BUILD_TRANSFORM: &str = r#"
def _gnu_compile(req):
    return struct(
        executable = req.tool,
        args = ["-c", req.src, "-o", req.out],
        inputs = [req.src],
        outputs = [req.out],
    )

gnu = define_config(name = "gnu", compile = _gnu_compile)

def _impl(ctx):
    out = ctx.attr.name + ".o"
    spec = gnu.compile(struct(tool = "/usr/bin/cc", src = ctx.attr.src, out = out))
    ctx.actions.run(
        executable = spec.executable,
        arguments = spec.args,
        inputs = spec.inputs,
        outputs = spec.outputs,
    )
    return [DefaultInfo(files = spec.outputs)]

cc_obj = rule(implementation = _impl, attrs = {"src": 1})
cc_obj(name = "gadget", src = "gadget.c")
"#;

    #[test]
    fn compiles_via_a_define_config_transform() {
        if !Path::new("/usr/bin/cc").exists() {
            return;
        }
        let exec = tempfile::tempdir().unwrap();
        std::fs::write(exec.path().join("gadget.c"), "int g(void){return 7;}").unwrap();
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        let produced = build_target(BUILD_TRANSFORM, "gadget", exec.path(), &cache).unwrap();
        assert_eq!(produced, vec!["gadget.o"]);
        assert!(exec.path().join("gadget.o").exists());
        // The successful build exercised the define_config transform during analysis (select()
        // resolution); per-analysis configs live in the Session, not a global post-hoc query.
    }

    // A real cc_library: compile N sources, then archive into a static lib (multi-action,
    // single target — no cross-target deps yet).
    const BUILD_LIBRARY: &str = r#"
def _gnu_compile(req):
    return struct(executable = req.tool, args = ["-c", req.src, "-o", req.out],
                  inputs = [req.src], outputs = [req.out])

def _gnu_archive(req):
    return struct(executable = req.ar, args = ["rcs", req.out] + req.objs,
                  inputs = req.objs, outputs = [req.out])

gnu = define_config(name = "gnu", compile = _gnu_compile, archive = _gnu_archive)

def _cc_library_impl(ctx):
    objs = []
    for src in ctx.attr.srcs:
        o = src + ".o"
        c = gnu.compile(struct(tool = "/usr/bin/cc", src = src, out = o))
        ctx.actions.run(executable = c.executable, arguments = c.args, inputs = c.inputs, outputs = c.outputs)
        objs.append(o)
    lib = "lib" + ctx.attr.name + ".a"
    a = gnu.archive(struct(ar = "/usr/bin/ar", objs = objs, out = lib))
    ctx.actions.run(executable = a.executable, arguments = a.args, inputs = a.inputs, outputs = a.outputs)
    return [DefaultInfo(files = [lib])]

cc_library = rule(implementation = _cc_library_impl, attrs = {"srcs": 1})
cc_library(name = "math", srcs = ["add.c", "sub.c"])
"#;

    #[test]
    fn compiles_a_static_library_from_multiple_sources() {
        if !Path::new("/usr/bin/cc").exists() || !Path::new("/usr/bin/ar").exists() {
            return;
        }
        let exec = tempfile::tempdir().unwrap();
        std::fs::write(
            exec.path().join("add.c"),
            "int add(int a,int b){return a+b;}",
        )
        .unwrap();
        std::fs::write(
            exec.path().join("sub.c"),
            "int sub(int a,int b){return a-b;}",
        )
        .unwrap();
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        let produced = build_target(BUILD_LIBRARY, "math", exec.path(), &cache).unwrap();
        assert_eq!(produced, vec!["add.c.o", "sub.c.o", "libmath.a"]);
        let lib = exec.path().join("libmath.a");
        assert!(lib.exists(), "razel did not produce libmath.a");
        assert!(std::fs::metadata(&lib).unwrap().len() > 0);
    }

    // The two-phase milestone: cc_binary depends on cc_library, reads its DefaultInfo
    // (libmath.a), and LINKS it into a runnable binary — transitive build, deps first.
    const BUILD_BINARY: &str = r#"
def _gnu_compile(req):
    return struct(executable = req.tool, args = ["-c", req.src, "-o", req.out],
                  inputs = [req.src], outputs = [req.out])
def _gnu_archive(req):
    return struct(executable = req.ar, args = ["rcs", req.out] + req.objs,
                  inputs = req.objs, outputs = [req.out])
def _gnu_link(req):
    return struct(executable = req.cc, args = ["-o", req.out] + req.objs + req.libs,
                  inputs = req.objs + req.libs, outputs = [req.out])

gnu = define_config(name = "gnu", compile = _gnu_compile, archive = _gnu_archive, link = _gnu_link)

def _cc_library_impl(ctx):
    objs = []
    for src in ctx.attr.srcs:
        o = src + ".o"
        c = gnu.compile(struct(tool = "/usr/bin/cc", src = src, out = o))
        ctx.actions.run(executable = c.executable, arguments = c.args, inputs = c.inputs, outputs = c.outputs)
        objs.append(o)
    lib = "lib" + ctx.attr.name + ".a"
    a = gnu.archive(struct(ar = "/usr/bin/ar", objs = objs, out = lib))
    ctx.actions.run(executable = a.executable, arguments = a.args, inputs = a.inputs, outputs = a.outputs)
    return [DefaultInfo(files = [lib])]
cc_library = rule(implementation = _cc_library_impl, attrs = {"srcs": 1})

def _cc_binary_impl(ctx):
    o = ctx.attr.name + ".o"
    c = gnu.compile(struct(tool = "/usr/bin/cc", src = ctx.attr.src, out = o))
    ctx.actions.run(executable = c.executable, arguments = c.args, inputs = c.inputs, outputs = c.outputs)
    libs = []
    for d in ctx.attr.deps:
        libs = libs + d.files
    out = ctx.attr.name
    l = gnu.link(struct(cc = "/usr/bin/cc", objs = [o], libs = libs, out = out))
    ctx.actions.run(executable = l.executable, arguments = l.args, inputs = l.inputs, outputs = l.outputs)
    return [DefaultInfo(files = [out])]
cc_binary = rule(implementation = _cc_binary_impl, attrs = {"src": 1})

cc_library(name = "math", srcs = ["add.c"])
cc_binary(name = "app", src = "app.c", deps = [":math"])
"#;

    #[test]
    fn cc_binary_links_cc_library_into_a_runnable_binary() {
        if !Path::new("/usr/bin/cc").exists() || !Path::new("/usr/bin/ar").exists() {
            return;
        }
        let exec = tempfile::tempdir().unwrap();
        std::fs::write(
            exec.path().join("add.c"),
            "int add(int a,int b){return a+b;}",
        )
        .unwrap();
        // main returns add(40,2)-42 == 0; resolving `add` requires linking libmath.a.
        std::fs::write(
            exec.path().join("app.c"),
            "int add(int,int); int main(void){return add(40,2)-42;}",
        )
        .unwrap();
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();

        // Build the binary: razel runs math's actions (lib) first, then app's (link).
        let produced = build_target(BUILD_BINARY, "app", exec.path(), &cache).unwrap();
        assert!(
            produced.contains(&"libmath.a".to_string()),
            "dep lib not built: {produced:?}"
        );
        assert!(
            produced.contains(&"app".to_string()),
            "binary not built: {produced:?}"
        );

        // The produced binary actually runs and links correctly (exit 0).
        let app = exec.path().join("app");
        assert!(app.exists());
        let status = std::process::Command::new(&app).status().unwrap();
        assert_eq!(status.code(), Some(0), "linked binary did not run/return 0");
    }

    /// RAZEL_BAZEL_BUILD_COMPAT: a cc_library→cc_binary dep chain (native rules) builds with
    /// EVERY output under `bazel-out/<config>/bin/…` (Bazel's layout), nothing in-tree, and the
    /// binary still links + runs — proving dep references resolve to the bazel-out paths. The
    /// dep's `libmath.a` is read from bazel-out by the link, because both its declared output
    /// and the binary's reference go through `qualify_output`.
    #[test]
    fn bazel_build_compat_puts_outputs_under_bazel_out_and_links() {
        if !Path::new("/usr/bin/cc").exists() {
            return;
        }
        let exec = tempfile::tempdir().unwrap();
        let src = "load(\"@rules_cc//cc:defs.bzl\", \"cc_library\", \"cc_binary\")\n\
             cc_library(name = \"math\", srcs = [\"add.c\"])\n\
             cc_binary(name = \"app\", srcs = [\"app.c\"], deps = [\":math\"])\n";
        std::fs::write(exec.path().join("add.c"), "int add(int a,int b){return a+b;}").unwrap();
        std::fs::write(
            exec.path().join("app.c"),
            "int add(int,int); int main(void){return add(40,2)-42;}",
        )
        .unwrap();
        let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();
        let flags = GlobalFlags { bazel_build_compat: true, ..Default::default() };
        let report = build_bazel_with(src, "app", exec.path(), &cache, flags).unwrap();
        // Every produced output carries the bazel-out/<config>/bin prefix — nothing in-tree.
        assert!(
            report.produced.iter().all(|p| p.starts_with("bazel-out/")),
            "outputs not all under bazel-out: {:?}",
            report.produced
        );
        let appout = report.default_outputs.first().expect("a default output");
        assert!(appout.contains("/bin/") && appout.ends_with("app"), "binary path: {appout}");
        // It physically lands there AND links (libmath.a resolved from bazel-out) + runs.
        let app = exec.path().join(appout);
        assert!(app.exists(), "binary missing at {}", app.display());
        assert!(!exec.path().join("app").exists(), "binary leaked in-tree");
        let status = std::process::Command::new(&app).status().unwrap();
        assert_eq!(status.code(), Some(0), "compat-built binary did not link/run");
    }

    /// Target-pattern expansion: `//...` spans packages, `//pkg:all` stays in its package
    /// (subpackages excluded), `//pkg/...` recurses, and a concrete label passes through.
    #[test]
    fn expand_pattern_spans_packages_and_excludes_subpackages() {
        let root = tempfile::tempdir().unwrap();
        let cc = "load(\"@rules_cc//cc:defs.bzl\", \"cc_library\")\n";
        std::fs::create_dir_all(root.path().join("a/b")).unwrap();
        std::fs::write(root.path().join("a/BUILD"), format!("{cc}cc_library(name = \"x\", srcs = [\"x.c\"])\n")).unwrap();
        std::fs::write(root.path().join("a/b/BUILD"), format!("{cc}cc_library(name = \"y\", srcs = [\"y.c\"])\n")).unwrap();
        let g = GlobalFlags::default;
        let has = |v: &[String], l: &str| v.iter().any(|x| x == l);

        let all = expand_pattern(root.path(), "//...", g()).unwrap();
        assert!(has(&all, "//a:x") && has(&all, "//a/b:y"), "//... spans packages: {all:?}");

        let a = expand_pattern(root.path(), "//a:all", g()).unwrap();
        assert!(has(&a, "//a:x") && !has(&a, "//a/b:y"), "//a:all excludes subpackage: {a:?}");

        let rec = expand_pattern(root.path(), "//a/...", g()).unwrap();
        assert!(has(&rec, "//a:x") && has(&rec, "//a/b:y"), "//a/... recurses: {rec:?}");

        // Concrete label → returned as-is, no discovery.
        assert_eq!(expand_pattern(root.path(), "//a:x", g()).unwrap(), vec!["//a:x".to_string()]);
    }

    /// S5x: the parallel executor. Diamond `base <- {a, b} <- top`. Each dependent CATs
    /// its deps' outputs, so a build that completes AT ALL proves deps ran first (cat fails
    /// on a missing input); `a` and `b` are independent (run concurrently at jobs>=2). The
    /// SAME graph at jobs=1 and jobs=4 must give a byte-identical report + outputs — the
    /// `-j1 == -jN` determinism bar.
    #[test]
    fn parallel_execute_respects_deps_and_is_deterministic_across_jobs() {
        use razel_loading::{AnalyzedAction, AnalyzedTarget};
        let act = |argv: &[&str], ins: &[&str], out: &str| AnalyzedAction {
            mnemonic: "Gen".into(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            inputs: ins.iter().map(|s| s.to_string()).collect(),
            outputs: vec![out.into()],
        };
        let t = |name: &str, deps: &[&str], a: AnalyzedAction, out: &str| AnalyzedTarget {
            name: name.into(),
            deps: deps.iter().map(|s| s.to_string()).collect(),
            actions: vec![a],
            default_info: vec![out.into()],
            ..Default::default()
        };
        let targets = vec![
            t("base", &[], act(&["/bin/sh", "-c", "echo base > base.txt"], &[], "base.txt"), "base.txt"),
            t("a", &["base"], act(&["/bin/sh", "-c", "cat base.txt > a.txt; echo a >> a.txt"], &["base.txt"], "a.txt"), "a.txt"),
            t("b", &["base"], act(&["/bin/sh", "-c", "cat base.txt > b.txt; echo b >> b.txt"], &["base.txt"], "b.txt"), "b.txt"),
            t("top", &["a", "b"], act(&["/bin/sh", "-c", "cat a.txt b.txt > top.txt"], &["a.txt", "b.txt"], "top.txt"), "top.txt"),
        ];
        let run = |jobs: usize| {
            let exec = tempfile::tempdir().unwrap();
            let cache = Cache::new(tempfile::tempdir().unwrap().path()).unwrap();
            let r = execute_jobs(&targets, "top", exec.path(), &cache, jobs).expect("build ok");
            let top = std::fs::read_to_string(exec.path().join("top.txt")).unwrap();
            (r.executed, r.produced, top)
        };
        let (e1, p1, top1) = run(1);
        let (e4, p4, top4) = run(4);
        // Deps respected (cat only succeeds if inputs exist first) — same content either way.
        assert_eq!(top1, "base\na\nbase\nb\n", "ordering/content: {top1:?}");
        // -j1 == -jN: identical executed count, produced list, and output bytes.
        assert_eq!(e1, 4, "fresh cache executes all four");
        assert_eq!(e4, 4, "parallel executes all four");
        assert_eq!(p1, p4, "produced list deterministic across jobs: {p1:?} vs {p4:?}");
        assert_eq!(top1, top4, "output byte-identical across jobs");
    }
}
