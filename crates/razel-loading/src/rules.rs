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
    FrozenModule, Globals, GlobalsBuilder, LibraryExtension, Methods, MethodsBuilder, Module,
};
use starlark::eval::{Arguments, Evaluator, FileLoader};
use starlark::syntax::{AstModule, Dialect};
use starlark::values::list::{ListRef, UnpackList};
use starlark::values::none::NoneType;
use starlark::values::structs::AllocStruct;
use starlark::values::{Heap, NoSerialize, StarlarkValue, Trace, Value, starlark_value};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

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
    /// Analyzed targets by **canonical label** → providers, so a dependent's `deps`
    /// reads them (cross-target/-package provider flow). In single-package mode the
    /// key is the bare name; in workspace mode it's `//pkg:name`.
    static RESULTS: RefCell<BTreeMap<String, AnalyzedTarget>> = const { RefCell::new(BTreeMap::new()) };
}

thread_local! {
    /// Workspace root (set in multi-package mode) — used to read a dep package's
    /// BUILD on demand. `None` ⇒ single-package mode (no package qualification).
    static WORKSPACE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    /// The package currently being evaluated (`None` ⇒ single-package mode).
    static CURRENT_PKG: RefCell<Option<String>> = const { RefCell::new(None) };
    /// Packages whose BUILD has been loaded (cycle/repeat guard).
    static LOADED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Canonicalize a target name/label against the current package. Single-package
/// mode keeps bare names; workspace mode produces `//pkg:name`.
fn canon_label(s: &str) -> String {
    CURRENT_PKG.with_borrow(|p| match p {
        None => s.strip_prefix(':').unwrap_or(s).to_string(),
        Some(pkg) => {
            if let Some(rest) = s.strip_prefix("//") {
                format!("//{rest}")
            } else if let Some(name) = s.strip_prefix(':') {
                format!("//{pkg}:{name}")
            } else {
                format!("//{pkg}:{s}")
            }
        }
    })
}

/// Package-qualify a source/output path (`x.cc` → `pkg/x.cc` in workspace mode).
fn qualify(path: &str) -> String {
    CURRENT_PKG.with_borrow(|p| match p {
        Some(pkg) => format!("{pkg}/{path}"),
        None => path.to_string(),
    })
}

/// The package of a canonical label `//pkg:name`.
fn pkg_of(label: &str) -> Option<String> {
    label
        .strip_prefix("//")?
        .split_once(':')
        .map(|(p, _)| p.to_string())
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

    /// `glob(include, exclude=[])` — match the current package's files (workspace
    /// mode) against the patterns, returning package-relative paths. Requires a
    /// package on disk; errors in single-package (no-dir) mode.
    fn glob<'v>(
        #[starlark(require = pos)] include: UnpackList<String>,
        #[starlark(require = named)] exclude: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<Vec<String>> {
        let include = include.items;
        let exclude = exclude.map(|l| l.items).unwrap_or_default();
        let dir = WORKSPACE
            .with_borrow(|w| w.clone())
            .zip(CURRENT_PKG.with_borrow(|p| p.clone()))
            .map(|(root, pkg)| root.join(&pkg));
        let Some(dir) = dir else {
            return Err(anyhow::anyhow!(
                "glob() needs a package on disk — use the workspace build path"
            ));
        };
        let mut files = Vec::new();
        walk_files(&dir, &dir, &mut files);
        let mut out: Vec<String> = files
            .into_iter()
            .filter(|f| {
                include.iter().any(|p| crate::glob_match(p, f))
                    && !exclude.iter().any(|p| crate::glob_match(p, f))
            })
            .collect();
        out.sort();
        Ok(out)
    }
}

/// Recursively collect files under `dir` as paths relative to `base` (skipping
/// dot-directories like `.razel-sandbox`/`.razel-cache`).
fn walk_files(dir: &Path, base: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let dot = p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('.'));
        if dot {
            continue;
        }
        if p.is_dir() {
            walk_files(&p, base, out);
        } else if let Ok(rel) = p.strip_prefix(base) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
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
/// Resolve a dep label to (its linkable outputs, exported hdrs, canonical label).
/// In workspace mode a cross-package dep whose package isn't loaded yet is loaded
/// on demand; in single-package mode a forward/cross reference errors clearly.
fn resolve_dep(label: &str) -> anyhow::Result<(Vec<String>, Vec<String>, String)> {
    let canon = canon_label(label);
    let hit = RESULTS.with_borrow(|r| {
        r.get(&canon)
            .map(|t| (t.default_info.clone(), t.hdrs.clone()))
    });
    if let Some((libs, hdrs)) = hit {
        return Ok((libs, hdrs, canon));
    }
    // Workspace mode: pull in the dep's package, then retry.
    if WORKSPACE.with_borrow(|w| w.is_some())
        && let Some(pkg) = pkg_of(&canon)
    {
        load_package(&pkg).map_err(|e| anyhow::anyhow!(e))?;
        if let Some((libs, hdrs)) = RESULTS.with_borrow(|r| {
            r.get(&canon)
                .map(|t| (t.default_info.clone(), t.hdrs.clone()))
        }) {
            return Ok((libs, hdrs, canon));
        }
    }
    Err(anyhow::anyhow!(
        "dep `{label}` not analyzed — declare it before its users (cyclic or missing package)"
    ))
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

        // Package-qualify own srcs/hdrs (workspace mode); resolve deps to canon labels.
        let srcs: Vec<String> = srcs.iter().map(|s| qualify(s)).collect();
        let hdrs: Vec<String> = hdrs.iter().map(|h| qualify(h)).collect();
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
            actions.push(compile_action(s, &o, inputs));
            objs.push(o);
        }
        let lib = qualify(&format!("lib{name}.a"));
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
            name: canon_label(&name),
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

        let srcs: Vec<String> = srcs.iter().map(|s| qualify(s)).collect();
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
            actions.push(compile_action(s, &o, inputs));
            objs.push(o);
        }
        let out = qualify(&name);
        let mut link_inputs = objs.clone();
        link_inputs.extend(dep_libs.clone());
        let mut link_argv = vec![CXX.into(), "-o".into(), out.clone()];
        link_argv.extend(objs);
        link_argv.extend(dep_libs);
        actions.push(AnalyzedAction {
            mnemonic: "CppLink".into(),
            argv: link_argv,
            inputs: link_inputs,
            outputs: vec![out.clone()],
        });
        record_target(AnalyzedTarget {
            name: canon_label(&name),
            deps: dep_names,
            actions,
            default_info: vec![out],
            hdrs: Vec::new(),
        });
        Ok(NoneType)
    }
}

/// A C++ compile action. `-iquote .` makes workspace-root-relative quote-includes
/// (`#include "pkg/x.h"`) resolve from the sandbox root (= exec root).
fn compile_action(src: &str, obj: &str, inputs: Vec<String>) -> AnalyzedAction {
    AnalyzedAction {
        mnemonic: "CppCompile".into(),
        argv: vec![
            CXX.into(),
            "-iquote".into(),
            ".".into(),
            "-c".into(),
            src.into(),
            "-o".into(),
            obj.into(),
        ],
        inputs,
        outputs: vec![obj.into()],
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

/// The globals available to BUILD and `.bzl` evaluation: `rule()`, `DefaultInfo`,
/// `select`, `define_config`, `glob`, + struct. (cc rules arrive via `load()`.)
fn build_globals() -> Globals {
    GlobalsBuilder::extended_by(&[LibraryExtension::StructType])
        .with(rule_globals)
        .build()
}

/// Resolve a `.bzl` load label to a file under `root`. `//pkg:f.bzl` → `root/pkg/f.bzl`;
/// `:f.bzl` → `root/<current pkg>/f.bzl`. External repos other than `@rules_cc` error.
fn resolve_bzl(root: &Path, label: &str) -> Result<PathBuf, String> {
    if let Some(rest) = label.strip_prefix("//") {
        let (pkg, file) = rest
            .split_once(':')
            .ok_or_else(|| format!("bad .bzl label `{label}`"))?;
        Ok(root.join(pkg).join(file))
    } else if let Some(file) = label.strip_prefix(':') {
        let pkg = CURRENT_PKG.with_borrow(|p| p.clone()).unwrap_or_default();
        Ok(root.join(pkg).join(file))
    } else {
        Err(format!(
            "unsupported load path `{label}` (only //pkg:f.bzl, :f.bzl, @rules_cc)"
        ))
    }
}

/// File loader for BUILD/`.bzl` evaluation: resolves `@rules_cc//cc:*.bzl` to the
/// synthetic native module, and any other `//pkg:f.bzl`/`:f.bzl` to a project file
/// it reads + evaluates (recursively, with this same loader) — so a BUILD can
/// `load()` a repo's own macros. (`rule()` objects can't be frozen yet, so a `.bzl`
/// that *defines* a rule will fail to freeze; macros over the native cc rules work.)
struct BzlLoader<'a> {
    rules_cc: &'a FrozenModule,
    globals: &'a Globals,
    cache: RefCell<HashMap<String, FrozenModule>>,
}

impl FileLoader for BzlLoader<'_> {
    fn load(&self, path: &str) -> starlark::Result<FrozenModule> {
        if path.starts_with("@rules_cc//") {
            return Ok(self.rules_cc.clone());
        }
        if let Some(m) = self.cache.borrow().get(path) {
            return Ok(m.clone());
        }
        let err = |m: String| starlark::Error::new_other(anyhow::anyhow!(m));
        let root = WORKSPACE
            .with_borrow(|w| w.clone())
            .ok_or_else(|| err(format!("load(\"{path}\") needs workspace mode")))?;
        let fs = resolve_bzl(&root, path).map_err(err)?;
        let src = std::fs::read_to_string(&fs)
            .map_err(|e| err(format!("cannot read {}: {e}", fs.display())))?;

        let frozen = Module::with_temp_heap(|module| -> starlark::Result<FrozenModule> {
            let ast = AstModule::parse(path, src, &Dialect::Extended)?;
            {
                let mut eval = Evaluator::new(&module);
                eval.set_loader(self); // recursive: a .bzl may load other .bzl
                eval.eval_module(ast, self.globals)?;
            }
            module
                .freeze()
                .map_err(|e| starlark::Error::new_other(anyhow::anyhow!("{e:?}")))
        })?;
        self.cache
            .borrow_mut()
            .insert(path.to_string(), frozen.clone());
        Ok(frozen)
    }
}

/// Evaluate one BUILD source with the cc-rule loader + the rule globals.
/// Targets it instantiates are recorded into STATE/RESULTS (re-entrant: a nested
/// cross-package load appends, never clears).
fn eval_build_src(name: &str, src: &str) -> Result<(), String> {
    let rules_cc = rules_cc_module()?;
    let globals = build_globals();
    let loader = BzlLoader {
        rules_cc: &rules_cc,
        globals: &globals,
        cache: RefCell::new(HashMap::new()),
    };
    let ast =
        AstModule::parse(name, src.to_owned(), &Dialect::Extended).map_err(|e| format!("{e}"))?;
    Module::with_temp_heap(|module| {
        let mut eval = Evaluator::new(&module);
        eval.set_loader(&loader);
        eval.eval_module(ast, &globals)
            .map_err(|e| format!("{e}"))?;
        Ok(())
    })
}

fn reset_analysis() {
    STATE.with_borrow_mut(|s| {
        s.targets.clear();
        s.current = None;
    });
    CONFIGS.with_borrow_mut(|c| c.clear());
    RESULTS.with_borrow_mut(|r| r.clear());
    LOADED.with_borrow_mut(|l| l.clear());
}

/// Evaluate a **real Bazel `BUILD`** that `load()`s cc rules from `@rules_cc`,
/// resolving those loads to razel's native rules (no rules_cc execution, no repo
/// fetch). Single-package (bare-name targets).
pub fn analyze_bazel(build_src: &str) -> Result<Vec<AnalyzedTarget>, String> {
    reset_analysis();
    CURRENT_PKG.with_borrow_mut(|p| *p = None);
    WORKSPACE.with_borrow_mut(|w| *w = None);
    eval_build_src("BUILD", build_src)?;
    Ok(STATE.with_borrow_mut(|s| std::mem::take(&mut s.targets)))
}

/// Load a package's BUILD (once) under workspace mode, evaluating it with that
/// package as context. Cross-package deps trigger further loads via `resolve_dep`.
fn load_package(pkg: &str) -> Result<(), String> {
    if LOADED.with_borrow(|l| l.contains(pkg)) {
        return Ok(());
    }
    LOADED.with_borrow_mut(|l| {
        l.insert(pkg.to_string());
    });
    let root = WORKSPACE
        .with_borrow(|w| w.clone())
        .ok_or("load_package called outside workspace mode")?;
    let build_path = ["BUILD", "BUILD.bazel"]
        .iter()
        .map(|f| root.join(pkg).join(f))
        .find(|p| p.exists())
        .ok_or_else(|| format!("no BUILD in package `{pkg}` ({})", root.join(pkg).display()))?;
    let src = std::fs::read_to_string(&build_path).map_err(|e| e.to_string())?;

    let prev = CURRENT_PKG.with_borrow_mut(|p| p.replace(pkg.to_string()));
    let res = eval_build_src(&format!("{pkg}/BUILD"), &src);
    CURRENT_PKG.with_borrow_mut(|p| *p = prev);
    res
}

/// Analyze a **multi-package** workspace rooted at `root`, starting from
/// `top_label` (`//pkg:name`) and loading dependency packages on demand. Targets
/// are keyed by canonical `//pkg:name` labels with package-qualified paths.
pub fn analyze_workspace(root: &Path, top_label: &str) -> Result<Vec<AnalyzedTarget>, String> {
    reset_analysis();
    WORKSPACE.with_borrow_mut(|w| *w = Some(root.to_path_buf()));
    let top_pkg = pkg_of(&canon_label(top_label))
        .ok_or_else(|| format!("top label must be //pkg:name, got `{top_label}`"))?;
    let res = load_package(&top_pkg);
    WORKSPACE.with_borrow_mut(|w| *w = None);
    CURRENT_PKG.with_borrow_mut(|p| *p = None);
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
