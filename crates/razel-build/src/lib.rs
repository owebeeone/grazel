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

use razel_actions::Action;
use razel_core::Digest;
use razel_exec::{Cache, build_action};
use razel_loading::analyze_starlark;
use std::collections::BTreeMap;
use std::path::Path;

/// Build one Starlark-defined target end-to-end: analyze, then execute each action in
/// `exec_root` (its declared inputs must exist there), caching by content key. Returns the
/// produced output paths.
pub fn build_target(
    build_src: &str,
    target: &str,
    exec_root: &Path,
    cache: &Cache,
) -> Result<Vec<String>, String> {
    let analyzed = analyze_starlark("BUILD", build_src)?;
    let t = analyzed
        .iter()
        .find(|t| t.name == target)
        .ok_or_else(|| format!("no such target: {target}"))?;

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
        produced.extend(act.outputs.clone());
    }
    Ok(produced)
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
}
