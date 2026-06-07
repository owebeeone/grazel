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
use starlark::coerce::Coerce;
use starlark::collections::SmallMap;
use starlark::environment::{
    FrozenModule, Globals, GlobalsBuilder, LibraryExtension, Methods, MethodsBuilder, Module,
};
use starlark::eval::{Arguments, Evaluator, FileLoader};
use starlark::starlark_complex_value;
use starlark::syntax::{AstModule, Dialect};
use starlark::values::list::{ListRef, UnpackList};
use starlark::values::none::NoneType;
use starlark::values::structs::AllocStruct;
use starlark::values::{
    Freeze, Heap, NoSerialize, StarlarkValue, Trace, Value, ValueLifetimeless, ValueLike,
    starlark_value,
};
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
    /// Compile flags this target exports to dependents — its `defines` (`-D…`) and
    /// `includes` (`-I…`), transitively. (Local `copts` are NOT exported.)
    pub cflags: Vec<String>,
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

thread_local! {
    /// CLI-level flags applied to every cc action this analysis produces (Bazel's
    /// `--copt`/`--linkopt`/… and `-c` expanded to compile flags). Set per analyze.
    static GLOBAL: RefCell<GlobalFlags> = RefCell::new(GlobalFlags::default());
}

/// Build-wide flags from the command line that ride every cc action: `copts` prepend
/// to every compile (so `-c opt` / `--copt`/`--cxxopt`/`--conlyopt`/`--define` take
/// effect), `linkopts` append to every link (`--linkopt`). Per-target attrs still apply.
#[derive(Debug, Clone, Default)]
pub struct GlobalFlags {
    pub copts: Vec<String>,
    pub linkopts: Vec<String>,
}

/// Canonicalize a target name/label against the current package. Single-package
/// mode keeps bare names; workspace mode produces `//pkg:name`.
pub(crate) fn canon_label(s: &str) -> String {
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
pub(crate) fn qualify(path: &str) -> String {
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

/// Single-quote a string for safe embedding in a `/bin/sh -c` script.
fn shquote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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
        #[starlark(require = named)] executable: Option<Value<'v>>,
        #[starlark(require = named)] outputs: Option<UnpackList<Value<'v>>>,
        #[starlark(require = named)] inputs: Option<UnpackList<Value<'v>>>,
        #[starlark(require = named)] arguments: Option<UnpackList<Value<'v>>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        let exe = executable.map(file_path).unwrap_or_else(|| "run".into());
        let mut argv = vec![exe.clone()];
        // arguments may contain plain strings, lists, File values, and args() objects.
        for a in arguments.map(|l| l.items).unwrap_or_default() {
            argv.extend(flatten_arg(a));
        }
        let paths = |l: Option<UnpackList<Value<'v>>>| -> Vec<String> {
            l.map(|l| l.items.into_iter().map(file_path).collect())
                .unwrap_or_default()
        };
        with_current(|c| {
            c.actions.push(AnalyzedAction {
                mnemonic: exe,
                argv,
                inputs: paths(inputs),
                outputs: paths(outputs),
            })
        });
        Ok(NoneType)
    }
    /// `ctx.actions.args()` — a mutable argument accumulator (`add`/`add_all`),
    /// flattened into the argv when passed to `run(arguments=[args])`.
    fn args<'v>(
        #[starlark(this)] _this: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(eval.heap().alloc_complex_no_freeze(Args {
            items: RefCell::new(Vec::new()),
        }))
    }
    fn write<'v>(
        #[starlark(this)] _this: Value<'v>,
        #[starlark(require = named)] output: Value<'v>,
        #[starlark(require = named)] content: Option<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        let output = file_path(output);
        // Real write: a /bin/sh action printf-ing the content into the output file.
        let script = format!(
            "printf '%s' {} > {}",
            shquote(&content.unwrap_or_default()),
            shquote(&output)
        );
        with_current(|c| {
            c.actions.push(AnalyzedAction {
                mnemonic: "FileWrite".into(),
                argv: vec!["/bin/sh".into(), "-c".into(), script],
                inputs: Vec::new(),
                outputs: vec![output],
            })
        });
        Ok(NoneType)
    }
}

// ---- ctx.actions.args() ----------------------------------------------------------

/// A `ctx.actions.args()` accumulator. Mutated in place by `add`/`add_all`; flattened
/// into the argv when the action runs. (Created + consumed within one analysis scope,
/// so it never freezes.)
#[derive(Debug, NoSerialize, ProvidesStaticType, Allocative, Trace)]
struct Args {
    #[allocative(skip)]
    #[trace(unsafe_ignore)]
    items: RefCell<Vec<String>>,
}

impl fmt::Display for Args {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<Args>")
    }
}

#[starlark_value(type = "Args")]
impl<'v> StarlarkValue<'v> for Args {
    fn get_methods() -> Option<&'static Methods> {
        Some(ARGS_METHODS.methods())
    }
}

starlark::methods_static!(ARGS_METHODS = args_methods);

#[starlark::starlark_module]
fn args_methods(b: &mut MethodsBuilder) {
    fn add<'v>(
        #[starlark(this)] this: Value<'v>,
        #[starlark(require = pos)] arg: Value<'v>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        if let Some(a) = this.downcast_ref::<Args>() {
            a.items.borrow_mut().push(file_path(arg));
        }
        Ok(NoneType)
    }
    fn add_all<'v>(
        #[starlark(this)] this: Value<'v>,
        #[starlark(require = pos)] values: Value<'v>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        if let Some(a) = this.downcast_ref::<Args>() {
            for s in flatten_arg(values) {
                a.items.borrow_mut().push(s);
            }
        }
        Ok(NoneType)
    }
}

/// Flatten a `run(arguments=…)` element into argv strings: an [`Args`] yields its
/// accumulated items, a list recurses, a [`File`] yields its path, anything else
/// stringifies.
fn flatten_arg(v: Value) -> Vec<String> {
    if let Some(a) = v.downcast_ref::<Args>() {
        return a.items.borrow().clone();
    }
    if let Some(list) = ListRef::from_value(v) {
        return list.iter().flat_map(flatten_arg).collect();
    }
    vec![file_path(v)]
}

// ---- File (ctx.outputs.*, ctx.files.*) -------------------------------------------

/// A `File` value: a workspace-relative path with Bazel's File fields. razel paths
/// are already workspace-relative, so `short_path` == `path`.
#[derive(Debug, NoSerialize, ProvidesStaticType, Allocative, Trace)]
struct File {
    #[trace(unsafe_ignore)]
    path: String,
}

impl fmt::Display for File {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path)
    }
}

#[starlark_value(type = "File")]
impl<'v> StarlarkValue<'v> for File {
    fn get_attr(&self, name: &str, heap: Heap<'v>) -> Option<Value<'v>> {
        let p = self.path.as_str();
        let base = p.rsplit('/').next().unwrap_or(p);
        match name {
            "path" | "short_path" => Some(heap.alloc(p.to_string())),
            "basename" => Some(heap.alloc(base.to_string())),
            "dirname" => {
                Some(heap.alloc(p.rsplit_once('/').map(|x| x.0).unwrap_or("").to_string()))
            }
            "extension" => Some(
                heap.alloc(
                    base.rsplit_once('.')
                        .map(|(_, e)| e)
                        .unwrap_or("")
                        .to_string(),
                ),
            ),
            _ => None,
        }
    }
}

/// Extract a path string from a value: a [`File`]'s path, a string as-is, else display.
fn file_path(v: Value) -> String {
    if let Some(f) = v.downcast_ref::<File>() {
        return f.path.clone();
    }
    v.unpack_str()
        .map(str::to_string)
        .unwrap_or_else(|| v.to_str())
}

// ---- ctx ------------------------------------------------------------------------

/// The analysis `ctx`. All fields are heap `Value`s so it traces cleanly, no freezing.
#[derive(Debug, NoSerialize, ProvidesStaticType, Allocative, Trace)]
struct Ctx<'v> {
    attr: Value<'v>,
    actions: Value<'v>,
    label: Value<'v>,
    /// `ctx.outputs.<name>` — predeclared output filenames (package-qualified).
    outputs: Value<'v>,
    /// `ctx.files.<name>` — the source files of label/label_list attrs (qualified).
    files: Value<'v>,
    /// `ctx.executable.<name>` — the runnable output of an executable-label attr.
    executable: Value<'v>,
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
            "outputs" => Some(self.outputs),
            "files" => Some(self.files),
            "executable" => Some(self.executable),
            _ => None,
        }
    }
}

// ---- rule() + DefaultInfo + select ----------------------------------------------

/// A `rule()` value. Generic over `V` so it has both an unfrozen form (`RuleObj<'v>`,
/// holding a live `Value`) and a frozen form (`FrozenRuleObj`, holding a `FrozenValue`)
/// — which is what lets a rule **survive `module.freeze()`** and therefore be defined
/// in a `.bzl` and `load()`ed, not just inline. The impl function freezes with it.
#[derive(Debug, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
struct RuleObjGen<V: ValueLifetimeless> {
    implementation: V,
}
starlark_complex_value!(RuleObj);

impl<V: ValueLifetimeless> fmt::Display for RuleObjGen<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<rule>")
    }
}

#[starlark_value(type = "rule")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for RuleObjGen<V>
where
    Self: ProvidesStaticType<'v>,
{
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
                            // Key by canonical label — bare in single-package mode,
                            // //pkg:name in a workspace (matches the native rules).
                            let dep = canon_label(label);
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
                name: canon_label(&name),
                deps: dep_labels,
                ..Default::default()
            })
        });

        // ctx.outputs.<attr> — string-valued attrs are predeclared output filenames
        // (package-qualified). ctx.files.<attr> — list-valued attrs are source files
        // (qualified). ctx.executable is empty until an executable-label attr is wired.
        // (razel resolves attribute values directly; the schema is not consulted.)
        let mk_file = |s: &str| heap.alloc_complex_no_freeze(File { path: qualify(s) });
        let outputs_fields: Vec<(String, Value<'v>)> = named
            .iter()
            .filter_map(|(k, v)| v.unpack_str().map(|s| (k.as_str().to_string(), mk_file(s))))
            .collect();
        let files_fields: Vec<(String, Value<'v>)> = named
            .iter()
            .filter_map(|(k, v)| {
                ListRef::from_value(*v).map(|list| {
                    let items: Vec<Value<'v>> = list
                        .iter()
                        .filter_map(|it| it.unpack_str().map(mk_file))
                        .collect();
                    (k.as_str().to_string(), heap.alloc(items))
                })
            })
            .collect();
        let ctx = heap.alloc_complex_no_freeze(Ctx {
            attr: heap.alloc(AllocStruct(fields)),
            actions: heap.alloc_complex_no_freeze(Actions),
            label: heap.alloc(canon_label(&name)),
            outputs: heap.alloc(AllocStruct(outputs_fields)),
            files: heap.alloc(AllocStruct(files_fields)),
            executable: heap.alloc(AllocStruct(Vec::<(String, Value<'v>)>::new())),
        });
        eval.eval_function(self.implementation.to_value(), &[ctx], &[])?;

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
        // alloc (freezable) — the rule survives module.freeze(), so it can be
        // defined in a .bzl and load()ed, not just used inline.
        Ok(eval.heap().alloc(RuleObjGen { implementation }))
    }

    /// `Label("//pkg:name")` — a minimal Label exposing `.package`/`.name`/
    /// `.workspace_root`/`.workspace_name`. razel treats everything as the main repo,
    /// so workspace_root/workspace_name are empty (matching Bazel on the main repo).
    #[allow(non_snake_case)]
    fn Label<'v>(
        #[starlark(require = pos)] s: String,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let after = s.rsplit_once("//").map(|(_, a)| a).unwrap_or(s.as_str());
        let (pkg, name) = match after.split_once(':') {
            Some((p, n)) => (p.to_string(), n.to_string()),
            None => (
                after.to_string(),
                after.rsplit('/').next().unwrap_or(after).to_string(),
            ),
        };
        let heap = eval.heap();
        Ok(heap.alloc(AllocStruct([
            ("package".to_string(), heap.alloc(pkg)),
            ("name".to_string(), heap.alloc(name)),
            ("workspace_root".to_string(), heap.alloc(String::new())),
            ("workspace_name".to_string(), heap.alloc(String::new())),
        ])))
    }

    /// BUILD package-declaration builtins. razel doesn't enforce visibility/licenses
    /// and tracks no separate file-export set, so these are no-op declarations —
    /// recognized so real BUILD files evaluate. (`package`, `package_group`,
    /// `licenses`, `exports_files`.)
    fn package<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn package_group<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn licenses<'v>(
        #[starlark(require = pos)] _licenses: UnpackList<String>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn exports_files<'v>(
        #[starlark(require = pos)] _files: UnpackList<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// Native build-graph builtins razel recognizes so real BUILD files evaluate.
    /// `config_setting`/`test_suite`/`alias` carry no buildable output here, so they
    /// record an empty target under their label (analysis-visible, builds to nothing);
    /// `filegroup` forwards its `srcs` as its outputs so dependents resolve them.
    fn config_setting<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        record_target(AnalyzedTarget {
            name: canon_label(&name),
            ..Default::default()
        });
        Ok(NoneType)
    }
    fn test_suite<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        record_target(AnalyzedTarget {
            name: canon_label(&name),
            ..Default::default()
        });
        Ok(NoneType)
    }
    fn alias<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        record_target(AnalyzedTarget {
            name: canon_label(&name),
            ..Default::default()
        });
        Ok(NoneType)
    }
    fn filegroup<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        let files: Vec<String> = unpack(srcs).iter().map(|s| qualify(s)).collect();
        record_target(AnalyzedTarget {
            name: canon_label(&name),
            default_info: files.clone(),
            hdrs: files,
            ..Default::default()
        });
        Ok(NoneType)
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
        do_glob(include.items, exclude.map(|l| l.items).unwrap_or_default())
    }
}

/// Shared `glob()`/`native.glob()` implementation: scan the current package dir
/// against the include/exclude patterns, package-relative, sorted.
fn do_glob(include: Vec<String>, exclude: Vec<String>) -> anyhow::Result<Vec<String>> {
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

pub(crate) fn record_target(t: AnalyzedTarget) {
    RESULTS.with_borrow_mut(|r| {
        r.insert(t.name.clone(), t.clone());
    });
    STATE.with_borrow_mut(|s| s.targets.push(t));
}

/// What a dep contributes to its users: linkable outputs, exported hdrs, exported
/// compile flags (defines/includes), and its canonical label.
pub(crate) struct DepInfo {
    pub(crate) libs: Vec<String>,
    pub(crate) hdrs: Vec<String>,
    pub(crate) cflags: Vec<String>,
    pub(crate) canon: String,
}

/// Resolve a dep label to its [`DepInfo`]. In workspace mode a cross-package dep
/// whose package isn't loaded yet is loaded on demand; otherwise a forward/cross
/// reference errors clearly.
pub(crate) fn resolve_dep(label: &str) -> anyhow::Result<DepInfo> {
    let canon = canon_label(label);
    let get = || {
        RESULTS.with_borrow(|r| {
            r.get(&canon)
                .map(|t| (t.default_info.clone(), t.hdrs.clone(), t.cflags.clone()))
        })
    };
    let hit = get().or_else(|| {
        // Workspace mode: pull in the dep's package, then retry.
        if WORKSPACE.with_borrow(|w| w.is_some())
            && let Some(pkg) = pkg_of(&canon)
        {
            let _ = load_package(&pkg);
        }
        get()
    });
    let Some((libs, hdrs, cflags)) = hit else {
        return Err(anyhow::anyhow!(
            "dep `{label}` not analyzed — declare it before its users (cyclic or missing package)"
        ));
    };
    Ok(DepInfo {
        libs,
        hdrs,
        cflags,
        canon,
    })
}

#[starlark::starlark_module]
fn cc_rules(b: &mut GlobalsBuilder) {
    // cc_library legitimately has many named attrs (name/srcs/hdrs/deps/copts/...).
    #[allow(clippy::too_many_arguments)]
    fn native_cc_library<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(require = named)] hdrs: Option<UnpackList<String>>,
        #[starlark(require = named)] deps: Option<UnpackList<String>>,
        #[starlark(require = named)] copts: Option<UnpackList<String>>,
        #[starlark(require = named)] defines: Option<UnpackList<String>>,
        #[starlark(require = named)] includes: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        let srcs: Vec<String> = unpack(srcs).iter().map(|s| qualify(s)).collect();
        let hdrs: Vec<String> = unpack(hdrs).iter().map(|h| qualify(h)).collect();
        let copts = unpack(copts);

        let (mut dep_names, mut dep_hdrs, mut dep_cflags) = (Vec::new(), Vec::new(), Vec::new());
        for d in &unpack(deps) {
            let dep = resolve_dep(d)?;
            dep_hdrs.extend(dep.hdrs);
            dep_cflags.extend(dep.cflags);
            dep_names.push(dep.canon);
        }

        // Exported flags (propagate to dependents): own defines/includes + dep cflags.
        let mut export_cflags = define_flags(defines);
        export_cflags.extend(include_flags(includes));
        export_cflags.extend(dep_cflags);
        // This lib's own compiles see global flags first, then local copts, then exports.
        let mut compile_flags = GLOBAL.with_borrow(|g| g.copts.clone());
        compile_flags.extend(copts);
        compile_flags.extend(export_cflags.iter().cloned());

        let mut avail_hdrs = hdrs.clone();
        avail_hdrs.extend(dep_hdrs.iter().cloned());

        let (mut actions, mut objs) = (Vec::new(), Vec::new());
        for s in &srcs {
            let o = format!("{s}.o");
            let mut inputs = vec![s.clone()];
            inputs.extend(avail_hdrs.iter().cloned());
            actions.push(compile_action(s, &o, &compile_flags, inputs));
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
            cflags: export_cflags,
        });
        Ok(NoneType)
    }

    fn native_cc_binary<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(require = named)] deps: Option<UnpackList<String>>,
        #[starlark(require = named)] copts: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        let srcs: Vec<String> = unpack(srcs).iter().map(|s| qualify(s)).collect();
        let (mut dep_names, mut dep_libs, mut dep_hdrs, mut dep_cflags) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for d in &unpack(deps) {
            let dep = resolve_dep(d)?;
            dep_libs.extend(dep.libs);
            dep_hdrs.extend(dep.hdrs);
            dep_cflags.extend(dep.cflags);
            dep_names.push(dep.canon);
        }
        // Binary compiles see global flags + local copts + the deps' exported flags.
        let mut compile_flags = GLOBAL.with_borrow(|g| g.copts.clone());
        compile_flags.extend(unpack(copts));
        compile_flags.extend(dep_cflags);

        let (mut actions, mut objs) = (Vec::new(), Vec::new());
        for s in &srcs {
            let o = format!("{s}.o");
            let mut inputs = vec![s.clone()];
            inputs.extend(dep_hdrs.iter().cloned());
            actions.push(compile_action(s, &o, &compile_flags, inputs));
            objs.push(o);
        }
        let out = qualify(&name);
        let mut link_inputs = objs.clone();
        link_inputs.extend(dep_libs.clone());
        let mut link_argv = vec![CXX.into(), "-o".into(), out.clone()];
        link_argv.extend(objs);
        link_argv.extend(dep_libs);
        link_argv.extend(GLOBAL.with_borrow(|g| g.linkopts.clone()));
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
            cflags: Vec::new(),
        });
        Ok(NoneType)
    }
}

pub(crate) fn unpack(list: Option<UnpackList<String>>) -> Vec<String> {
    list.map(|l| l.items).unwrap_or_default()
}

/// `defines = ["FOO=1"]` → `["-DFOO=1"]`.
fn define_flags(defines: Option<UnpackList<String>>) -> Vec<String> {
    unpack(defines).iter().map(|d| format!("-D{d}")).collect()
}

/// `includes = ["inc"]` → `["-Ipkg/inc"]` (package-qualified include dirs).
fn include_flags(includes: Option<UnpackList<String>>) -> Vec<String> {
    unpack(includes)
        .iter()
        .map(|i| format!("-I{}", qualify(i)))
        .collect()
}

/// A C++ compile action. `-iquote .` makes workspace-root-relative quote-includes
/// (`#include "pkg/x.h"`) resolve from the sandbox root (= exec root); `flags` are
/// the target's copts + transitive defines/includes.
fn compile_action(src: &str, obj: &str, flags: &[String], inputs: Vec<String>) -> AnalyzedAction {
    let mut argv = vec![CXX.into(), "-iquote".into(), ".".into()];
    argv.extend(flags.iter().cloned());
    argv.extend(["-c".into(), src.into(), "-o".into(), obj.into()]);
    AnalyzedAction {
        mnemonic: "CppCompile".into(),
        argv,
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

/// Record an analysis-visible target with no actions (a build-graph placeholder).
fn record_named(name: &str) {
    record_target(AnalyzedTarget {
        name: canon_label(name),
        ..Default::default()
    });
}

/// `@bazel_skylib` rules razel recognizes as no-op/minimal targets: `bzl_library`,
/// the build/diff test wrappers, the `common_settings` build-setting flags, and the
/// small codegen rules. razel enforces no build settings and tracks no .bzl
/// libraries, so these are analysis-visible placeholders (they build to nothing).
/// skylib's *lib* helpers (`selects`/`paths`/`sets`) are pure-Starlark namespaces —
/// handled separately as TF reaches them.
#[allow(non_snake_case)]
#[starlark::starlark_module]
fn skylib_rules(b: &mut GlobalsBuilder) {
    fn native_bzl_library<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        record_named(&name);
        Ok(NoneType)
    }
    fn native_build_test<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        record_named(&name);
        Ok(NoneType)
    }
    fn native_diff_test<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        record_named(&name);
        Ok(NoneType)
    }
    fn native_string_flag<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        record_named(&name);
        Ok(NoneType)
    }
    fn native_bool_flag<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        record_named(&name);
        Ok(NoneType)
    }
    fn native_int_flag<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        record_named(&name);
        Ok(NoneType)
    }
    fn native_string_list_flag<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        record_named(&name);
        Ok(NoneType)
    }
    fn native_string_setting<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        record_named(&name);
        Ok(NoneType)
    }
    fn native_expand_template<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        record_named(&name);
        Ok(NoneType)
    }
    fn native_copy_file<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        record_named(&name);
        Ok(NoneType)
    }
}

/// The synthetic `@bazel_skylib` module — re-exports the skylib rules under their
/// load names.
fn rules_skylib_module() -> Result<FrozenModule, String> {
    let globals = GlobalsBuilder::standard().with(skylib_rules).build();
    let reexport = "bzl_library = native_bzl_library\n\
build_test = native_build_test\n\
diff_test = native_diff_test\n\
string_flag = native_string_flag\n\
bool_flag = native_bool_flag\n\
int_flag = native_int_flag\n\
string_list_flag = native_string_list_flag\n\
string_setting = native_string_setting\n\
expand_template = native_expand_template\n\
copy_file = native_copy_file\n";
    Module::with_temp_heap(|module| {
        let ast = AstModule::parse("@bazel_skylib", reexport.to_owned(), &Dialect::Extended)
            .map_err(|e| format!("{e}"))?;
        {
            let mut eval = Evaluator::new(&module);
            eval.eval_module(ast, &globals)
                .map_err(|e| format!("{e}"))?;
        }
        module.freeze().map_err(|e| format!("{e:?}"))
    })
}

/// Helpers from Bazel's **auto-configured** repos (`@local_config_rocm`,
/// `@local_config_cuda`, …). Bazel generates these by probing the host for a
/// CUDA/ROCm install; razel has no such toolchain, so every `if_<x>_is_configured`
/// resolves to **not configured** — it returns its false branch (default `[]`), the
/// same value real Bazel yields on a CPU-only checkout.
#[starlark::starlark_module]
fn auto_config_fns(b: &mut GlobalsBuilder) {
    fn native_if_not_configured<'v>(
        #[starlark(require = pos)] _if_true: Value<'v>,
        #[starlark(require = pos)] if_false: Option<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(if_false.unwrap_or_else(|| eval.heap().alloc(Vec::<Value<'v>>::new())))
    }
}

/// A synthetic auto-config repo module: maps every `if_<x>_is_configured` name to the
/// not-configured helper.
fn auto_config_module(reexport: &str) -> Result<FrozenModule, String> {
    let globals = GlobalsBuilder::standard().with(auto_config_fns).build();
    Module::with_temp_heap(|module| {
        let ast = AstModule::parse("@local_config", reexport.to_owned(), &Dialect::Extended)
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
/// The Bazel `native.*` namespace (minimal): package/repo introspection + `glob`.
/// razel treats the analyzed package as the only context (main repo), so
/// `package_name` is the current package and `repository_name` is `@`.
#[starlark::starlark_module]
fn native_members(b: &mut GlobalsBuilder) {
    fn package_name() -> anyhow::Result<String> {
        Ok(CURRENT_PKG.with_borrow(|p| p.clone()).unwrap_or_default())
    }
    fn repository_name() -> anyhow::Result<String> {
        Ok("@".to_string())
    }
    fn glob<'v>(
        #[starlark(require = pos)] include: UnpackList<String>,
        #[starlark(require = named)] exclude: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<Vec<String>> {
        do_glob(include.items, exclude.map(|l| l.items).unwrap_or_default())
    }
}

/// The Bazel `attr.*` namespace: declares a rule's attribute schema. razel's `rule()`
/// resolves attribute *values* directly from the call kwargs (it doesn't enforce the
/// schema), so each `attr.<kind>(...)` is a placeholder descriptor — present so rule
/// definitions evaluate.
#[starlark::starlark_module]
fn attr_members(b: &mut GlobalsBuilder) {
    fn string<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn int<'v>(#[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn bool<'v>(#[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn label<'v>(#[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn label_list<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn label_keyed_string_dict<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn string_list<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn string_dict<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn string_list_dict<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn output<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn output_list<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
}

fn build_globals() -> Globals {
    GlobalsBuilder::extended_by(&[
        LibraryExtension::StructType,
        LibraryExtension::Print,
        LibraryExtension::Map,
        LibraryExtension::Filter,
        LibraryExtension::Debug,
        LibraryExtension::Json,
        LibraryExtension::Partial,
    ])
    .with(rule_globals)
    .with(|b| {
        b.namespace("native", native_members);
        b.namespace("attr", attr_members);
    })
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

/// A natively-provided ruleset: `load()`s whose path starts with `prefix`
/// (e.g. `@rules_cc//`, `@rules_rust//`) resolve to `module`, a synthetic module
/// re-exporting razel's native rules under the names real BUILD files import.
pub(crate) struct Ruleset {
    pub(crate) prefix: &'static str,
    pub(crate) module: FrozenModule,
}

/// File loader for BUILD/`.bzl` evaluation: resolves a `@repo//...` load to its
/// native [`Ruleset`] module, and any other `//pkg:f.bzl`/`:f.bzl` to a project
/// file it reads + evaluates (recursively, with this same loader) — so a BUILD can
/// `load()` a repo's own macros. (`rule()` objects can't be frozen yet, so a `.bzl`
/// that *defines* a rule will fail to freeze; macros over the native rules work.)
struct BzlLoader<'a> {
    rulesets: &'a [Ruleset],
    globals: &'a Globals,
    cache: RefCell<HashMap<String, FrozenModule>>,
}

impl FileLoader for BzlLoader<'_> {
    fn load(&self, path: &str) -> starlark::Result<FrozenModule> {
        if let Some(rs) = self.rulesets.iter().find(|r| path.starts_with(r.prefix)) {
            return Ok(rs.module.clone());
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

/// Every natively-provided ruleset, by `load()` prefix. New languages register a
/// row here (the rule logic itself lives in the per-language module). Each maps a
/// `@repo//` to a synthetic module re-exporting razel's native rules.
fn ruleset_modules() -> Result<Vec<Ruleset>, String> {
    Ok(vec![
        Ruleset {
            prefix: "@rules_cc//",
            module: rules_cc_module()?,
        },
        Ruleset {
            prefix: "@bazel_skylib//",
            module: rules_skylib_module()?,
        },
        Ruleset {
            prefix: "@local_config_rocm//",
            module: auto_config_module("if_rocm_is_configured = native_if_not_configured\n")?,
        },
        // CUDA config helper lives in a specific @xla file — register the exact path
        // (not the whole @xla// prefix, which is a real source repo, not a shim).
        Ruleset {
            prefix: "@xla//xla/tsl/platform/default:cuda_build_defs.bzl",
            module: auto_config_module("if_cuda_is_configured = native_if_not_configured\n")?,
        },
        Ruleset {
            prefix: "@rules_rust//",
            module: crate::rust_rules::module()?,
        },
        Ruleset {
            prefix: "@rules_python//",
            module: crate::py_rules::module()?,
        },
        Ruleset {
            prefix: "@rules_shell//",
            module: crate::sh_rules::module()?,
        },
    ])
}

/// Evaluate one BUILD source with the ruleset loaders + the rule globals.
/// Targets it instantiates are recorded into STATE/RESULTS (re-entrant: a nested
/// cross-package load appends, never clears).
fn eval_build_src(name: &str, src: &str) -> Result<(), String> {
    let rulesets = ruleset_modules()?;
    let globals = build_globals();
    let loader = BzlLoader {
        rulesets: &rulesets,
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
    analyze_bazel_with(build_src, GlobalFlags::default())
}

/// [`analyze_bazel`] with build-wide [`GlobalFlags`] (the CLI's `--copt`/`-c`/… )
/// applied to every cc action.
pub fn analyze_bazel_with(
    build_src: &str,
    flags: GlobalFlags,
) -> Result<Vec<AnalyzedTarget>, String> {
    reset_analysis();
    CURRENT_PKG.with_borrow_mut(|p| *p = None);
    WORKSPACE.with_borrow_mut(|w| *w = None);
    GLOBAL.with_borrow_mut(|g| *g = flags);
    let res = eval_build_src("BUILD", build_src);
    GLOBAL.with_borrow_mut(|g| *g = GlobalFlags::default());
    res?;
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
    analyze_workspace_with(root, top_label, GlobalFlags::default())
}

/// [`analyze_workspace`] with build-wide [`GlobalFlags`] applied to every cc action.
pub fn analyze_workspace_with(
    root: &Path,
    top_label: &str,
    flags: GlobalFlags,
) -> Result<Vec<AnalyzedTarget>, String> {
    reset_analysis();
    WORKSPACE.with_borrow_mut(|w| *w = Some(root.to_path_buf()));
    GLOBAL.with_borrow_mut(|g| *g = flags);
    let top_pkg = pkg_of(&canon_label(top_label))
        .ok_or_else(|| format!("top label must be //pkg:name, got `{top_label}`"))?;
    let res = load_package(&top_pkg);
    WORKSPACE.with_borrow_mut(|w| *w = None);
    CURRENT_PKG.with_borrow_mut(|p| *p = None);
    GLOBAL.with_borrow_mut(|g| *g = GlobalFlags::default());
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
    let globals = GlobalsBuilder::extended_by(&[
        LibraryExtension::StructType,
        LibraryExtension::Print,
        LibraryExtension::Map,
        LibraryExtension::Filter,
        LibraryExtension::Debug,
        LibraryExtension::Json,
        LibraryExtension::Partial,
    ])
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
