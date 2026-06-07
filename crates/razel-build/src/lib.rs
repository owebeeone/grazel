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
use razel_loading::analyze_starlark;
// Re-exported so the daemon/clients can hold warm analysis (the analyze/execute split).
pub use razel_loading::AnalyzedTarget;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

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

/// Execute a pre-analyzed target graph: order deps-first and run every action in
/// `exec_root` (cache hit → 0 exec). Separated from [`analyze_build`] so callers
/// (the daemon) can reuse warm analysis across builds.
pub fn execute(
    targets: &[AnalyzedTarget],
    target: &str,
    exec_root: &Path,
    cache: &Cache,
) -> Result<BuildReport, String> {
    let by_name: HashMap<String, AnalyzedTarget> = targets
        .iter()
        .map(|t| (t.name.clone(), t.clone()))
        .collect();

    let mut order = Vec::new();
    collect_order(target, &by_name, &mut order, &mut HashSet::new())?;

    let mut produced = Vec::new();
    let mut executed = 0;
    for tname in &order {
        for act in &by_name[tname].actions {
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
    }
    Ok(BuildReport { produced, executed })
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
        // define_config registered the toolchain config engine-side (for selection).
        assert_eq!(razel_loading::registered_configs(), vec!["gnu".to_string()]);
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
}
