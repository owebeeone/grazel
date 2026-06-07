//! Starlark-defined rules + analysis (Phase 3): `rule(implementation, attrs)` returns a
//! callable custom value; instantiating it runs the rule **implementation** with a `ctx`
//! (Bazel dialect: `ctx.attr.*`, `ctx.label`, `ctx.actions.declare_file/run/write`) and
//! captures the registered actions (inputs/outputs) and `DefaultInfo` — the target
//! **analyzes**. Plus `select()` (host-config-lite) and `DefaultInfo`.
//!
//! Analysis runs in the **same eval scope** as instantiation (the impl `Value` never
//! escapes the heap) — sidestepping module freezing. Tier-2.5 simplification; a two-phase
//! freeze model comes when caching / cross-target dep-providers demand it.

use allocative::Allocative;
use starlark::any::ProvidesStaticType;
use starlark::collections::SmallMap;
use starlark::environment::{
    FrozenModule, GlobalsBuilder, LibraryExtension, Methods, MethodsBuilder, Module,
};
use starlark::eval::{Arguments, Evaluator, ReturnFileLoader};
use starlark::syntax::{AstModule, Dialect};
use starlark::values::list::{ListRef, UnpackList};
use starlark::values::none::NoneType;
use starlark::values::structs::AllocStruct;
use starlark::values::{Heap, NoSerialize, StarlarkValue, Trace, Value, starlark_value};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;

/// One action registered by a rule impl (`ctx.actions.run`/`write`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnalyzedAction {
    pub mnemonic: String,
    /// Full command: `[executable, args…]` — what the executor spawns.
    pub argv: Vec<String>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

/// The captured analysis of one target: a rule impl ran and produced these.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnalyzedTarget {
    pub name: String,
    /// Resolved dependency target names (from the `deps` attr).
    pub deps: Vec<String>,
    pub actions: Vec<AnalyzedAction>,
    /// `DefaultInfo(files=…)`.
    pub default_info: Vec<String>,
    /// Headers this target exports to dependents (cc_library `hdrs`, transitively).
    /// Bazel makes these explicit, so they double as the dependents' sandbox inputs.
    pub hdrs: Vec<String>,
}

#[derive(Default)]
struct AnalysisState {
    targets: Vec<AnalyzedTarget>,
    current: Option<AnalyzedTarget>,
}

thread_local! {
    static STATE: RefCell<AnalysisState> = RefCell::new(AnalysisState::default());
}

thread_local! {
    /// Toolchain configs declared via `define_config` (for host-config selection, D7).
    static CONFIGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Names of configs declared via `define_config` in the last `analyze_starlark` run.
pub fn registered_configs() -> Vec<String> {
    CONFIGS.with_borrow(|c| c.clone())
}

thread_local! {
    /// Analyzed targets by name → providers, so a dependent's `deps` reads them
    /// (cross-target provider flow). Requires deps declared before dependents; a forward
    /// reference errors clearly (full dependency ordering is the next two-phase step).
    static RESULTS: RefCell<BTreeMap<String, AnalyzedTarget>> = const { RefCell::new(BTreeMap::new()) };
}

fn with_current<F: FnOnce(&mut AnalyzedTarget)>(f: F) {
    STATE.with_borrow_mut(|s| {
        if let Some(c) = s.current.as_mut() {
            f(c);
        }
    });
}

// ---- ctx.actions ----------------------------------------------------------------

#[derive(Debug, NoSerialize, ProvidesStaticType, Allocative, Trace)]
struct Actions;

impl fmt::Display for Actions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<ctx.actions>")
    }
}

#[starlark_value(type = "actions")]
impl<'v> StarlarkValue<'v> for Actions {
    fn get_methods() -> Option<&'static Methods> {
        Some(ACTIONS_METHODS.methods())
    }
}

starlark::methods_static!(ACTIONS_METHODS = actions_methods);

#[starlark::starlark_module]
fn actions_methods(b: &mut MethodsBuilder) {
    fn declare_file<'v>(
        #[starlark(this)] _this: Value<'v>,
        filename: String,
    ) -> anyhow::Result<String> {
        Ok(filename)
    }
    fn run<'v>(
        #[starlark(this)] _this: Value<'v>,
        #[starlark(require = named)] executable: Option<String>,
        #[starlark(require = named)] outputs: Option<UnpackList<String>>,
        #[starlark(require = named)] inputs: Option<UnpackList<String>>,
        #[starlark(require = named)] arguments: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        let exe = executable.unwrap_or_else(|| "run".into());
        let mut argv = vec![exe.clone()];
        argv.extend(arguments.map(|l| l.items).unwrap_or_default());
        with_current(|c| {
            c.actions.push(AnalyzedAction {
                mnemonic: exe,
                argv,
                inputs: inputs.map(|l| l.items).unwrap_or_default(),
                outputs: outputs.map(|l| l.items).unwrap_or_default(),
            })
        });
        Ok(NoneType)
    }
    fn write<'v>(
        #[starlark(this)] _this: Value<'v>,
        #[starlark(require = named)] output: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        with_current(|c| {
            c.actions.push(AnalyzedAction {
                mnemonic: "write".into(),
                argv: vec!["<write>".into()],
                inputs: Vec::new(),
                outputs: vec![output],
            })
        });
        Ok(NoneType)
    }
}

// ---- ctx ------------------------------------------------------------------------

/// The analysis `ctx`. All fields are heap `Value`s so it traces cleanly, no freezing.
#[derive(Debug, NoSerialize, ProvidesStaticType, Allocative, Trace)]
struct Ctx<'v> {
    attr: Value<'v>,
    actions: Value<'v>,
    label: Value<'v>,
}

impl fmt::Display for Ctx<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<ctx>")
    }
}

#[starlark_value(type = "ctx")]
impl<'v> StarlarkValue<'v> for Ctx<'v> {
    fn get_attr(&self, attribute: &str, _heap: Heap<'v>) -> Option<Value<'v>> {
        match attribute {
            "attr" => Some(self.attr),
            "actions" => Some(self.actions),
            "label" => Some(self.label),
            _ => None,
        }
    }
}

// ---- rule() + DefaultInfo + select ----------------------------------------------

#[derive(Debug, Trace, NoSerialize, ProvidesStaticType, Allocative)]
struct RuleObj<'v> {
    implementation: Value<'v>,
}

impl fmt::Display for RuleObj<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<rule>")
    }
}

#[starlark_value(type = "rule")]
impl<'v> StarlarkValue<'v> for RuleObj<'v> {
    /// `my_rule(name=…, …)` — build a `ctx` and run the impl (same-scope analysis).
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let named = args.names_map()?;
        let heap = eval.heap();
        let mut name = String::new();
        let mut dep_labels: Vec<String> = Vec::new();
        let mut fields: Vec<(String, Value<'v>)> = Vec::new();
        for (k, v) in &named {
            let key = k.as_str().to_string();
            match key.as_str() {
                "name" => {
                    name = v.unpack_str().unwrap_or_default().to_string();
                    fields.push((key, *v));
                }
                // Two-phase provider flow: resolve each dep label to its analyzed
                // DefaultInfo (from the results registry) as a `struct(files = [...])`.
                "deps" => {
                    let mut providers: Vec<Value<'v>> = Vec::new();
                    if let Some(list) = ListRef::from_value(*v) {
                        for item in list.iter() {
                            let label = item.unpack_str().unwrap_or_default();
                            let dep = label.strip_prefix(':').unwrap_or(label).to_string();
                            let files = RESULTS
                                .with_borrow(|r| r.get(&dep).map(|t| t.default_info.clone()));
                            let Some(files) = files else {
                                return Err(anyhow::anyhow!(
                                    "dep `{dep}` not analyzed yet — declare it before its users \
                                     (forward references not yet supported)"
                                )
                                .into());
                            };
                            dep_labels.push(dep);
                            providers.push(heap.alloc(AllocStruct([("files".to_string(), files)])));
                        }
                    }
                    fields.push((key, heap.alloc(providers)));
                }
                _ => fields.push((key, *v)),
            }
        }

        STATE.with_borrow_mut(|s| {
            s.current = Some(AnalyzedTarget {
                name: name.clone(),
                deps: dep_labels,
                ..Default::default()
            })
        });

        let ctx = heap.alloc_complex_no_freeze(Ctx {
            attr: heap.alloc(AllocStruct(fields)),
            actions: heap.alloc_complex_no_freeze(Actions),
            label: heap.alloc(format!("//:{name}")),
        });
        eval.eval_function(self.implementation, &[ctx], &[])?;

        STATE.with_borrow_mut(|s| {
            if let Some(c) = s.current.take() {
                RESULTS.with_borrow_mut(|r| {
                    r.insert(c.name.clone(), c.clone());
                });
                s.targets.push(c);
            }
        });
        Ok(Value::new_none())
    }
}

#[allow(non_snake_case)]
#[starlark::starlark_module]
fn rule_globals(b: &mut GlobalsBuilder) {
    /// `rule(implementation, attrs={})` → a callable rule object.
    fn rule<'v>(
        #[starlark(require = named)] implementation: Value<'v>,
        #[starlark(require = named)] attrs: Option<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let _ = attrs;
        Ok(eval
            .heap()
            .alloc_complex_no_freeze(RuleObj { implementation }))
    }

    /// `DefaultInfo(files=[…])` — the standard output provider (other kwargs absorbed).
    fn DefaultInfo<'v>(
        #[starlark(require = named)] files: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        if let Some(f) = files {
            with_current(|c| c.default_info = f.items);
        }
        Ok(NoneType)
    }

    /// `select({cond: value, …})` — host-config-lite: pick `//conditions:default`, else
    /// the first branch. (Real config_setting matching is Phase 8.)
    fn select<'v>(branches: SmallMap<String, Value<'v>>) -> anyhow::Result<Value<'v>> {
        if let Some(v) = branches.get("//conditions:default") {
            return Ok(*v);
        }
        branches
            .values()
            .next()
            .copied()
            .ok_or_else(|| anyhow::anyhow!("select() with no branches"))
    }

    /// `define_config(name, compile, archive=None, link=None)` — declare + register a
    /// toolchain transform (D7). Returns a struct of the transform fns (so a rule can call
    /// `cfg.compile(req)`); also records the name engine-side for host-config selection.
    fn define_config<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] compile: Value<'v>,
        #[starlark(require = named)] archive: Option<Value<'v>>,
        #[starlark(require = named)] link: Option<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        CONFIGS.with_borrow_mut(|c| c.push(name));
        let mut fields: Vec<(String, Value<'v>)> = vec![("compile".to_string(), compile)];
        if let Some(a) = archive {
            fields.push(("archive".to_string(), a));
        }
        if let Some(l) = link {
            fields.push(("link".to_string(), l));
        }
        Ok(eval.heap().alloc(AllocStruct(fields)))
    }
}

// ---- native cc rules (the "build Google's BUILD files" path) -------------------
//
// `load("@rules_cc//cc:cc_binary.bzl", "cc_binary")` resolves to these — razel
// provides cc_library/cc_binary *natively* (via the host gnu/clang toolchain)
// instead of executing rules_cc's Starlark. The declared `srcs`/`hdrs`/`deps` are
// exactly the sandbox's declared inputs, so F12 enforcement holds with no header
// discovery (Bazel already makes you declare them).

const CXX: &str = "/usr/bin/c++";
const AR: &str = "/usr/bin/ar";

fn record_target(t: AnalyzedTarget) {
    RESULTS.with_borrow_mut(|r| {
        r.insert(t.name.clone(), t.clone());
    });
    STATE.with_borrow_mut(|s| s.targets.push(t));
}

/// Resolve a dep label to (its linkable outputs, its exported hdrs, bare name).
/// Same-package only for now (`:x`, `x`); cross-package `//pkg:x` errors clearly.
fn resolve_dep(label: &str) -> anyhow::Result<(Vec<String>, Vec<String>, String)> {
    let name = label.rsplit(':').next().unwrap_or(label).to_string();
    RESULTS
        .with_borrow(|r| {
            r.get(&name)
                .map(|t| (t.default_info.clone(), t.hdrs.clone(), name.clone()))
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "dep `{label}` not analyzed yet — declare it before its users \
                 (cross-package deps are a later increment)"
            )
        })
}

#[starlark::starlark_module]
fn cc_rules(b: &mut GlobalsBuilder) {
    fn native_cc_library<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(require = named)] hdrs: Option<UnpackList<String>>,
        #[starlark(require = named)] deps: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        let srcs = srcs.map(|l| l.items).unwrap_or_default();
        let hdrs = hdrs.map(|l| l.items).unwrap_or_default();
        let deps = deps.map(|l| l.items).unwrap_or_default();

        let (mut dep_names, mut dep_hdrs) = (Vec::new(), Vec::new());
        for d in &deps {
            let (_, h, n) = resolve_dep(d)?;
            dep_hdrs.extend(h);
            dep_names.push(n);
        }
        // own + transitive headers are present for compiling this lib's srcs.
        let mut avail_hdrs = hdrs.clone();
        avail_hdrs.extend(dep_hdrs.iter().cloned());

        let (mut actions, mut objs) = (Vec::new(), Vec::new());
        for s in &srcs {
            let o = format!("{s}.o");
            let mut inputs = vec![s.clone()];
            inputs.extend(avail_hdrs.iter().cloned());
            actions.push(AnalyzedAction {
                mnemonic: "CppCompile".into(),
                argv: vec![CXX.into(), "-c".into(), s.clone(), "-o".into(), o.clone()],
                inputs,
                outputs: vec![o.clone()],
            });
            objs.push(o);
        }
        let lib = format!("lib{name}.a");
        let mut ar_argv = vec![AR.into(), "rcs".into(), lib.clone()];
        ar_argv.extend(objs.clone());
        actions.push(AnalyzedAction {
            mnemonic: "CppArchive".into(),
            argv: ar_argv,
            inputs: objs,
            outputs: vec![lib.clone()],
        });

        let mut export_hdrs = hdrs;
        export_hdrs.extend(dep_hdrs);
        record_target(AnalyzedTarget {
            name,
            deps: dep_names,
            actions,
            default_info: vec![lib],
            hdrs: export_hdrs,
        });
        Ok(NoneType)
    }

    fn native_cc_binary<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(require = named)] deps: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        let srcs = srcs.map(|l| l.items).unwrap_or_default();
        let deps = deps.map(|l| l.items).unwrap_or_default();

        let (mut dep_names, mut dep_libs, mut dep_hdrs) = (Vec::new(), Vec::new(), Vec::new());
        for d in &deps {
            let (libs, h, n) = resolve_dep(d)?;
            dep_libs.extend(libs);
            dep_hdrs.extend(h);
            dep_names.push(n);
        }

        let (mut actions, mut objs) = (Vec::new(), Vec::new());
        for s in &srcs {
            let o = format!("{s}.o");
            let mut inputs = vec![s.clone()];
            inputs.extend(dep_hdrs.iter().cloned());
            actions.push(AnalyzedAction {
                mnemonic: "CppCompile".into(),
                argv: vec![CXX.into(), "-c".into(), s.clone(), "-o".into(), o.clone()],
                inputs,
                outputs: vec![o.clone()],
            });
            objs.push(o);
        }
        let mut link_inputs = objs.clone();
        link_inputs.extend(dep_libs.clone());
        let mut link_argv = vec![CXX.into(), "-o".into(), name.clone()];
        link_argv.extend(objs);
        link_argv.extend(dep_libs);
        actions.push(AnalyzedAction {
            mnemonic: "CppLink".into(),
            argv: link_argv,
            inputs: link_inputs,
            outputs: vec![name.clone()],
        });
        record_target(AnalyzedTarget {
            name: name.clone(),
            deps: dep_names,
            actions,
            default_info: vec![name],
            hdrs: Vec::new(),
        });
        Ok(NoneType)
    }
}

/// The synthetic `@rules_cc` module: re-exports the native rules under the names
/// real BUILD files `load()` (`cc_binary`, `cc_library`).
fn rules_cc_module() -> Result<FrozenModule, String> {
    let globals = GlobalsBuilder::standard().with(cc_rules).build();
    Module::with_temp_heap(|module| {
        let ast = AstModule::parse(
            "@rules_cc",
            "cc_binary = native_cc_binary\ncc_library = native_cc_library\n".to_owned(),
            &Dialect::Extended,
        )
        .map_err(|e| format!("{e}"))?;
        {
            let mut eval = Evaluator::new(&module);
            eval.eval_module(ast, &globals)
                .map_err(|e| format!("{e}"))?;
        }
        module.freeze().map_err(|e| format!("{e:?}"))
    })
}

/// Evaluate a **real Bazel `BUILD`** that `load()`s cc rules from `@rules_cc`,
/// resolving those loads to razel's native rules (no rules_cc execution, no repo
/// fetch). Returns the analyzed targets. Single-package for now.
pub fn analyze_bazel(build_src: &str) -> Result<Vec<AnalyzedTarget>, String> {
    STATE.with_borrow_mut(|s| {
        s.targets.clear();
        s.current = None;
    });
    CONFIGS.with_borrow_mut(|c| c.clear());
    RESULTS.with_borrow_mut(|r| r.clear());

    let builtins = rules_cc_module()?;
    let mut modules: HashMap<&str, &FrozenModule> = HashMap::new();
    for path in [
        "@rules_cc//cc:cc_binary.bzl",
        "@rules_cc//cc:cc_library.bzl",
        "@rules_cc//cc:cc_test.bzl",
        "@rules_cc//cc:defs.bzl",
    ] {
        modules.insert(path, &builtins);
    }
    let loader = ReturnFileLoader { modules: &modules };

    let ast = AstModule::parse("BUILD", build_src.to_owned(), &Dialect::Extended)
        .map_err(|e| format!("{e}"))?;
    let globals = GlobalsBuilder::extended_by(&[LibraryExtension::StructType])
        .with(rule_globals)
        .build();
    let res: Result<(), String> = Module::with_temp_heap(|module| {
        let mut eval = Evaluator::new(&module);
        eval.set_loader(&loader);
        eval.eval_module(ast, &globals)
            .map_err(|e| format!("{e}"))?;
        Ok(())
    });
    res?;
    Ok(STATE.with_borrow_mut(|s| std::mem::take(&mut s.targets)))
}

/// Evaluate a `BUILD`/`.bzl` that defines and instantiates Starlark rules, running each
/// rule impl (same-scope analysis); returns the analyzed targets.
pub fn analyze_starlark(name: &str, src: &str) -> Result<Vec<AnalyzedTarget>, String> {
    STATE.with_borrow_mut(|s| {
        s.targets.clear();
        s.current = None;
    });
    CONFIGS.with_borrow_mut(|c| c.clear());
    RESULTS.with_borrow_mut(|r| r.clear());
    let ast =
        AstModule::parse(name, src.to_owned(), &Dialect::Extended).map_err(|e| format!("{e}"))?;
    let globals = GlobalsBuilder::extended_by(&[LibraryExtension::StructType])
        .with(rule_globals)
        .build();
    let res: Result<(), String> = Module::with_temp_heap(|module| {
        let mut eval = Evaluator::new(&module);
        eval.eval_module(ast, &globals)
            .map_err(|e| format!("{e}"))?;
        Ok(())
    });
    res?;
    Ok(STATE.with_borrow_mut(|s| std::mem::take(&mut s.targets)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starlark_rule_analyzes_by_running_its_impl() {
        let src = r#"
def _impl(ctx):
    out = ctx.actions.declare_file(ctx.attr.name + ".o")
    ctx.actions.run(
        executable = "cc",
        outputs = [out],
        inputs = [ctx.attr.src],
        arguments = ["-c", ctx.attr.src],
    )
    return [DefaultInfo(files = [out])]

cc_thing = rule(implementation = _impl, attrs = {"src": 1})
cc_thing(name = "widget", src = "widget.c")
cc_thing(name = "gadget", src = "gadget.c")
"#;
        let targets = analyze_starlark("BUILD", src).unwrap();
        assert_eq!(targets.len(), 2);
        let w = &targets[0];
        assert_eq!(w.name, "widget");
        assert_eq!(w.actions.len(), 1);
        assert_eq!(w.actions[0].mnemonic, "cc");
        assert_eq!(w.actions[0].inputs, vec!["widget.c"]);
        assert_eq!(w.actions[0].outputs, vec!["widget.o"]);
        assert_eq!(w.default_info, vec!["widget.o"]);
        assert_eq!(targets[1].name, "gadget");
    }

    #[test]
    fn select_picks_default_branch() {
        let src = r#"
def _impl(ctx):
    flags = select({"//conditions:default": ["-O2"], "@cfg//:dbg": ["-g"]})
    ctx.actions.run(executable = "cc", outputs = [ctx.attr.name], inputs = [], arguments = flags)
    return [DefaultInfo(files = [ctx.attr.name])]

thing = rule(implementation = _impl, attrs = {})
thing(name = "x")
"#;
        let targets = analyze_starlark("BUILD", src).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].actions[0].mnemonic, "cc");
    }

    #[test]
    fn dependent_reads_dep_providers_two_phase() {
        // lib declared first; bin's deps=[":lib"] reads lib's analyzed DefaultInfo.
        let src = r#"
def _lib(ctx):
    out = "lib" + ctx.attr.name + ".a"
    ctx.actions.run(executable = "ar", outputs = [out], inputs = [], arguments = ["rcs", out])
    return [DefaultInfo(files = [out])]

def _bin(ctx):
    libs = []
    for d in ctx.attr.deps:
        libs = libs + d.files
    out = ctx.attr.name
    ctx.actions.run(executable = "cc", outputs = [out], inputs = libs, arguments = ["-o", out] + libs)
    return [DefaultInfo(files = [out])]

lib_rule = rule(implementation = _lib, attrs = {})
bin_rule = rule(implementation = _bin, attrs = {})

lib_rule(name = "math")
bin_rule(name = "app", deps = [":math"])
"#;
        let targets = analyze_starlark("BUILD", src).unwrap();
        let app = targets.iter().find(|t| t.name == "app").unwrap();
        assert_eq!(app.deps, vec!["math"]);
        // app linked the dep's analyzed output — the provider flowed across targets.
        assert_eq!(app.actions[0].inputs, vec!["libmath.a"]);
        assert!(app.actions[0].argv.contains(&"libmath.a".to_string()));
    }

    #[test]
    fn forward_dep_reference_errors_clearly() {
        // bin declared before its dep → forward ref → clear error, not silently wrong.
        let src = r#"
def _lib(ctx):
    return [DefaultInfo(files = ["x"])]
def _bin(ctx):
    return [DefaultInfo(files = ctx.attr.deps[0].files)]
lib_rule = rule(implementation = _lib, attrs = {})
bin_rule = rule(implementation = _bin, attrs = {})
bin_rule(name = "app", deps = [":math"])
lib_rule(name = "math")
"#;
        assert!(analyze_starlark("BUILD", src).is_err());
    }
}
