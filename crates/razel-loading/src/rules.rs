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
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

// C0: state + core types + the host-cc tool layer now live in `state.rs`. Re-exported (`pub(crate)`)
// so the sibling rule modules' `use crate::rules::{…}` keeps resolving until they move into `rules/`.
pub(crate) use crate::state::{
    AnalyzedAction, AnalyzedTarget, CcToolchainMode, GlobalFlags, Session, canon_label, pkg_of,
    qualify, session, with_current, AR,
};
use crate::providers::{fold_compile_jars, fold_headers, fold_runtime_jars};

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
        #[starlark(require = named)] mnemonic: Option<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
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
        with_current(sess, |c| {
            c.actions.push(AnalyzedAction {
                mnemonic: mnemonic.unwrap_or(exe),
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
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let output = file_path(output);
        // Real write: a /bin/sh action printf-ing the content into the output file.
        let script = format!(
            "printf '%s' {} > {}",
            shquote(&content.unwrap_or_default()),
            shquote(&output)
        );
        with_current(sess, |c| {
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

// ---- depset ----------------------------------------------------------------------

/// A `depset` — Bazel's deduplicated transitive set. razel stores the flattened
/// member **paths** (depset members are Files/strings in practice), which keeps it a
/// plain value and makes `.to_list()` / action wiring trivial. Construction folds in
/// `direct` members and the members of each `transitive` depset, de-duplicated.
#[derive(Debug, NoSerialize, ProvidesStaticType, Allocative, Trace)]
struct Depset {
    #[trace(unsafe_ignore)]
    items: Vec<String>,
}

impl fmt::Display for Depset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "depset({:?})", self.items)
    }
}

#[starlark_value(type = "depset")]
impl<'v> StarlarkValue<'v> for Depset {
    fn get_methods() -> Option<&'static Methods> {
        Some(DEPSET_METHODS.methods())
    }
}

starlark::methods_static!(DEPSET_METHODS = depset_methods);

#[starlark::starlark_module]
fn depset_methods(b: &mut MethodsBuilder) {
    fn to_list<'v>(
        #[starlark(this)] this: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let items = this
            .downcast_ref::<Depset>()
            .map(|d| d.items.clone())
            .unwrap_or_default();
        Ok(eval.heap().alloc(items))
    }
}

/// Extract member paths from a `DefaultInfo(files=…)` value: a [`Depset`]'s members,
/// a list's elements (Files/strings), or a single value.
fn extract_files(v: Value) -> Vec<String> {
    if let Some(ds) = v.downcast_ref::<Depset>() {
        return ds.items.clone();
    }
    if let Some(list) = ListRef::from_value(v) {
        return list.iter().map(file_path).collect();
    }
    vec![file_path(v)]
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
        let sess = session(eval);
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
                        let results = sess.results.borrow();
                        for item in list.iter() {
                            let label = item.unpack_str().unwrap_or_default();
                            // Key by canonical label — bare in single-package mode,
                            // //pkg:name in a workspace (matches the native rules).
                            let dep = canon_label(sess, label);
                            let Some(dep_target) = results.get(&dep) else {
                                return Err(anyhow::anyhow!(
                                    "dep `{dep}` not analyzed yet — declare it before its users \
                                     (forward references not yet supported)"
                                )
                                .into());
                            };
                            // `.files` = the dep's DefaultInfo; `.headers` = its transitive CcInfo
                            // exported headers (A2a — the provides fold, so the rule() path sees deps).
                            let files = dep_target.default_info.clone();
                            let headers = fold_headers(&results, &dep);
                            // `.compile_jars` = the dep's transitive JavaInfo compile jars, ORDERED
                            // (B3); `.runtime_jars` = the SEPARATE runtime closure, neverlink-pruned
                            // (B4). No cross-merge — two independent ordered folds.
                            let compile_jars = fold_compile_jars(&results, &dep);
                            let runtime_jars = fold_runtime_jars(&results, &dep);
                            dep_labels.push(dep);
                            providers.push(heap.alloc(AllocStruct([
                                ("files".to_string(), files),
                                ("headers".to_string(), headers),
                                ("compile_jars".to_string(), compile_jars),
                                ("runtime_jars".to_string(), runtime_jars),
                            ])));
                        }
                    }
                    fields.push((key, heap.alloc(providers)));
                }
                _ => fields.push((key, *v)),
            }
        }

        sess.state.borrow_mut().current = Some(AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: dep_labels,
            ..Default::default()
        });

        // ctx.outputs.<attr> — string-valued attrs are predeclared output filenames
        // (package-qualified). ctx.files.<attr> — list-valued attrs are source files
        // (qualified). ctx.executable is empty until an executable-label attr is wired.
        // (razel resolves attribute values directly; the schema is not consulted.)
        let mk_file = |s: &str| heap.alloc_complex_no_freeze(File { path: qualify(sess, s) });
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
            // `ctx.label` is a Label struct (`.package`/`.name`) — razel's cc:defs.bzl reads them
            // for the path model. package is the current pkg (empty in single-package mode).
            label: {
                let pkg = sess.current_pkg.borrow().clone().unwrap_or_default();
                heap.alloc(AllocStruct([
                    ("package".to_string(), heap.alloc(pkg)),
                    ("name".to_string(), heap.alloc(name.clone())),
                ]))
            },
            outputs: heap.alloc(AllocStruct(outputs_fields)),
            files: heap.alloc(AllocStruct(files_fields)),
            executable: heap.alloc(AllocStruct(Vec::<(String, Value<'v>)>::new())),
        });
        eval.eval_function(self.implementation.to_value(), &[ctx], &[])?;

        // Post-eval (after the nested rule impl ran): commit the analyzed target. `sess` is
        // still valid — it borrows the extra target, not `eval`. Short, non-overlapping borrows.
        {
            let mut st = sess.state.borrow_mut();
            if let Some(c) = st.current.take() {
                sess.results.borrow_mut().insert(c.name.clone(), c.clone());
                st.targets.push(c);
            }
        }
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
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }
    fn test_suite<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }
    fn alias<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }
    fn filegroup<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let files: Vec<String> = unpack(srcs).iter().map(|s| qualify(sess, s)).collect();
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            default_info: files.clone(),
            hdrs: files,
            ..Default::default()
        });
        Ok(NoneType)
    }

    /// `DefaultInfo(files=…)` — the standard output provider. `files` may be a list
    /// (of Files/strings) or a `depset`; other kwargs absorbed.
    fn DefaultInfo<'v>(
        #[starlark(require = named)] files: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        if let Some(f) = files {
            let paths = extract_files(f);
            with_current(session(eval), |c| c.default_info = paths);
        }
        Ok(NoneType)
    }

    /// `CcInfo(headers=…)` — the cc provider. razel captures the target's OWN exported headers into
    /// `hdrs`; the transitive set a dependent sees is recovered by folding over deps
    /// ([`fold_headers`]). Other kwargs absorbed. (A2a: the rule()-return providers are captured by
    /// side-effect, mirroring `DefaultInfo`.)
    fn CcInfo<'v>(
        #[starlark(require = named)] headers: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        if let Some(h) = headers {
            let paths = extract_files(h);
            with_current(session(eval), |c| c.hdrs = paths);
        }
        Ok(NoneType)
    }

    /// `JavaInfo(compile_jars=…)` — the java provider (B3 spike). razel captures the target's OWN
    /// exported compile jar(s) (the header/ijar) into `compile_jars`, **ordered**; a dependent's
    /// classpath is the preorder fold over deps ([`fold_compile_jars`] — the OrderedDepset analog).
    /// Other kwargs (runtime_jars, …) absorbed. Mirrors `CcInfo`, the SECOND hardcoded provider — the
    /// B5 ledger signal that Phase C should generalize provider-capture to a schema-driven map.
    fn JavaInfo<'v>(
        #[starlark(require = named)] compile_jars: Option<Value<'v>>,
        #[starlark(require = named)] runtime_jars: Option<Value<'v>>,
        #[starlark(require = named)] neverlink: Option<bool>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        // Two SEPARATE ordered depsets (compile vs runtime) + the neverlink conditional (B4).
        let compile = compile_jars.map(extract_files);
        let runtime = runtime_jars.map(extract_files);
        let nl = neverlink.unwrap_or(false);
        with_current(session(eval), |c| {
            if let Some(j) = compile {
                c.compile_jars = j;
            }
            if let Some(j) = runtime {
                c.runtime_jars = j;
            }
            c.neverlink = nl;
        });
        Ok(NoneType)
    }

    /// `dedup(list)` — the list with duplicate strings removed, **preserving first occurrence**. The
    /// cross-sibling dedup the rule()-path classpath/header assembly needs: a target's transitive
    /// closure folds *per-dep*, so a diamond (`app -> [x,y] -> base`) would otherwise list base's
    /// jar/header twice. `dedup()` makes the merge the engine's job, not silent duplication (F1).
    fn dedup(#[starlark(require = pos)] list: UnpackList<String>) -> anyhow::Result<Vec<String>> {
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for s in list.items {
            if seen.insert(s.clone()) {
                out.push(s);
            }
        }
        Ok(out)
    }

    /// `depset(direct=[], transitive=[depsets], order=…)` — Bazel's transitive set.
    /// razel folds member paths from `direct` + each `transitive` depset, deduped.
    fn depset<'v>(
        #[starlark(require = pos)] direct: Option<Value<'v>>,
        #[starlark(require = named)] transitive: Option<Value<'v>>,
        #[starlark(require = named)] order: Option<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // razel does not model depset traversal order yet (the 4-order family is reserved). WARN
        // loudly on a non-default order rather than silently produce a possibly-wrong sequence (F36).
        if let Some(o) = &order
            && o != "default"
        {
            eprintln!(
                "razel: warning: depset(order={o:?}) — traversal order not yet modeled, treating as \
                 default (F36; RazelGaps)"
            );
        }
        let mut items: Vec<String> = Vec::new();
        let push = |s: String, items: &mut Vec<String>| {
            if !items.contains(&s) {
                items.push(s);
            }
        };
        if let Some(d) = direct
            && let Some(list) = ListRef::from_value(d)
        {
            for it in list.iter() {
                push(file_path(it), &mut items);
            }
        }
        if let Some(t) = transitive
            && let Some(list) = ListRef::from_value(t)
        {
            for dep in list.iter() {
                if let Some(ds) = dep.downcast_ref::<Depset>() {
                    for s in &ds.items {
                        push(s.clone(), &mut items);
                    }
                }
            }
        }
        Ok(eval.heap().alloc_complex_no_freeze(Depset { items }))
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
        session(eval).configs.borrow_mut().push(name);
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
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Vec<String>> {
        do_glob(session(eval), include.items, exclude.map(|l| l.items).unwrap_or_default())
    }
}

/// Shared `glob()`/`native.glob()` implementation: scan the current package dir
/// against the include/exclude patterns, package-relative, sorted.
fn do_glob(sess: &Session, include: Vec<String>, exclude: Vec<String>) -> anyhow::Result<Vec<String>> {
    let dir = sess
        .workspace
        .clone()
        .zip(sess.current_pkg.borrow().clone())
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


pub(crate) fn record_target(sess: &Session, t: AnalyzedTarget) {
    sess.results.borrow_mut().insert(t.name.clone(), t.clone());
    sess.state.borrow_mut().targets.push(t);
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
pub(crate) fn resolve_dep(sess: &Session, label: &str) -> anyhow::Result<DepInfo> {
    let canon = canon_label(sess, label);
    let get = || {
        sess.results
            .borrow()
            .get(&canon)
            .map(|t| (t.default_info.clone(), t.hdrs.clone(), t.cflags.clone()))
    };
    let hit = get().or_else(|| {
        // Workspace mode: pull in the dep's package, then retry. The `get()` borrow is
        // dropped before `load_package` recurses into a nested eval (the [R1] discipline —
        // a `results` borrow held across the nested eval would double-borrow-panic).
        if sess.workspace.is_some()
            && let Some(pkg) = pkg_of(&canon)
        {
            let _ = load_package(sess, &pkg);
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
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let srcs: Vec<String> = unpack(srcs).iter().map(|s| qualify(sess, s)).collect();
        let hdrs: Vec<String> = unpack(hdrs).iter().map(|h| qualify(sess, h)).collect();
        let copts = unpack(copts);

        let (mut dep_names, mut dep_hdrs, mut dep_cflags) = (Vec::new(), Vec::new(), Vec::new());
        for d in &unpack(deps) {
            let dep = resolve_dep(sess, d)?;
            dep_hdrs.extend(dep.hdrs);
            dep_cflags.extend(dep.cflags);
            dep_names.push(dep.canon);
        }

        // Exported flags (propagate to dependents): own defines/includes + dep cflags.
        let mut export_cflags = define_flags(defines);
        export_cflags.extend(include_flags(sess, includes));
        export_cflags.extend(dep_cflags);
        // This lib's own compiles see global flags first, then local copts, then exports.
        let mut compile_flags = sess.global.copts.clone();
        compile_flags.extend(copts);
        compile_flags.extend(export_cflags.iter().cloned());

        let mut avail_hdrs = hdrs.clone();
        avail_hdrs.extend(dep_hdrs.iter().cloned());

        let (mut actions, mut objs) = (Vec::new(), Vec::new());
        for s in &srcs {
            let o = format!("{s}.o");
            let mut inputs = vec![s.clone()];
            inputs.extend(avail_hdrs.iter().cloned());
            actions.push(compile_action(&sess.host_cc(), s, &o, &compile_flags, inputs));
            objs.push(o);
        }
        let lib = qualify(sess, &format!("lib{name}.a"));
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
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: dep_names,
            actions,
            default_info: vec![lib],
            hdrs: export_hdrs,
            cflags: export_cflags,
            compile_jars: Vec::new(),
            runtime_jars: Vec::new(),
            neverlink: false,
        });
        Ok(NoneType)
    }

    fn native_cc_binary<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(require = named)] deps: Option<UnpackList<String>>,
        #[starlark(require = named)] copts: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let srcs: Vec<String> = unpack(srcs).iter().map(|s| qualify(sess, s)).collect();
        let (mut dep_names, mut dep_libs, mut dep_hdrs, mut dep_cflags) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for d in &unpack(deps) {
            let dep = resolve_dep(sess, d)?;
            dep_libs.extend(dep.libs);
            dep_hdrs.extend(dep.hdrs);
            dep_cflags.extend(dep.cflags);
            dep_names.push(dep.canon);
        }
        // Binary compiles see global flags + local copts + the deps' exported flags.
        let mut compile_flags = sess.global.copts.clone();
        compile_flags.extend(unpack(copts));
        compile_flags.extend(dep_cflags);

        let (mut actions, mut objs) = (Vec::new(), Vec::new());
        for s in &srcs {
            let o = format!("{s}.o");
            let mut inputs = vec![s.clone()];
            inputs.extend(dep_hdrs.iter().cloned());
            actions.push(compile_action(&sess.host_cc(), s, &o, &compile_flags, inputs));
            objs.push(o);
        }
        let out = qualify(sess, &name);
        let mut link_inputs = objs.clone();
        link_inputs.extend(dep_libs.clone());
        let mut link_argv = vec![sess.host_cc(), "-o".into(), out.clone()];
        link_argv.extend(objs);
        link_argv.extend(dep_libs);
        link_argv.extend(sess.global.linkopts.clone());
        actions.push(AnalyzedAction {
            mnemonic: "CppLink".into(),
            argv: link_argv,
            inputs: link_inputs,
            outputs: vec![out.clone()],
        });
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: dep_names,
            actions,
            default_info: vec![out],
            hdrs: Vec::new(),
            cflags: Vec::new(),
            compile_jars: Vec::new(),
            runtime_jars: Vec::new(),
            neverlink: false,
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
fn include_flags(sess: &Session, includes: Option<UnpackList<String>>) -> Vec<String> {
    unpack(includes)
        .iter()
        .map(|i| format!("-I{}", qualify(sess, i)))
        .collect()
}

/// A C++ compile action. `-iquote .` makes workspace-root-relative quote-includes
/// (`#include "pkg/x.h"`) resolve from the sandbox root (= exec root); `flags` are
/// the target's copts + transitive defines/includes.
fn compile_action(cc: &str, src: &str, obj: &str, flags: &[String], inputs: Vec<String>) -> AnalyzedAction {
    let mut argv = vec![cc.to_string(), "-iquote".into(), ".".into()];
    argv.extend(flags.iter().cloned());
    argv.extend(["-c".into(), src.into(), "-o".into(), obj.into()]);
    AnalyzedAction {
        mnemonic: "CppCompile".into(),
        argv,
        inputs,
        outputs: vec![obj.into()],
    }
}

/// The synthetic `@rules_cc` module, by toolchain mode (§7). **Native** re-exports razel's native
/// rules (host compiler, executable — razel-build's path). **AdoptBazel** serves razel's `cc:defs.bzl`
/// over the engine (Bazel-faithful declared graph — the parity runner's path).
fn rules_cc_module(mode: CcToolchainMode) -> Result<FrozenModule, String> {
    match mode {
        CcToolchainMode::Native => rules_cc_module_native(),
        CcToolchainMode::AdoptBazel => rules_cc_module_adopt_bazel(),
    }
}

/// Native: `cc_binary`/`cc_library` are razel's native rules (executable, host compiler).
fn rules_cc_module_native() -> Result<FrozenModule, String> {
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
            eval.eval_module(ast, &globals).map_err(|e| format!("{e}"))?;
        }
        module.freeze().map_err(|e| format!("{e:?}"))
    })
}

/// AdoptBazel: `cc_library` is razel's OWN rule — the bundled `cc:defs.bzl` evaluated over the
/// `razel_build` engine (the Bazel-faithful declared graph). `cc_binary` stays native until a `CppLink`
/// golden exists (Phase E). Bundling versions razel's two cc halves (Rust builtins + this `.bzl`)
/// atomically with the binary.
fn rules_cc_module_adopt_bazel() -> Result<FrozenModule, String> {
    let globals = GlobalsBuilder::standard()
        .with(cc_rules) // native_cc_binary (cc_binary's backend until Phase E)
        .with(rule_globals) // rule(), CcInfo, DefaultInfo, depset, …
        .with(|b| {
            b.namespace("razel_build", razel_build_members); // the cc engine seam (Constrain)
        })
        .build();
    Module::with_temp_heap(|module| {
        // F21 — KNOWN GAP, clearly marked: in Adopt-Bazel mode `cc_library` is faithful (the engine),
        // but `cc_binary` falls back to the NATIVE host-compiler backend (bare `/usr/bin/c++` link),
        // which is NOT Bazel's declared graph. Harmless today (no `cc_binary` in any parity corpus),
        // but a `cc_binary` analyzed in this mode silently gets a non-faithful graph. The faithful
        // `CppLink` backend lands in Phase E (the link golden); until then, treat any Adopt-Bazel
        // `cc_binary` result as NOT parity-grade.
        let src = format!("{}\ncc_binary = native_cc_binary\n", include_str!("cc_defs.bzl"));
        let ast =
            AstModule::parse("@rules_cc", src, &Dialect::Extended).map_err(|e| format!("{e}"))?;
        {
            let mut eval = Evaluator::new(&module);
            eval.eval_module(ast, &globals).map_err(|e| format!("{e}"))?;
        }
        module.freeze().map_err(|e| format!("{e:?}"))
    })
}

/// The synthetic `@rules_java` module (F16): `java_library` is razel's OWN rule — the bundled
/// `java:defs.bzl` over the engine (multi-action Turbine/Javac/JavaSourceJar + JavaInfo). java has no
/// native backend in razel, so there's no toolchain mode; this is the only java impl. Byte-parity vs
/// the golden is Phase D (the structural diff pins the action SHAPE — see tests/java_graph_parity.rs).
fn rules_java_module() -> Result<FrozenModule, String> {
    let globals = GlobalsBuilder::standard().with(rule_globals).build();
    Module::with_temp_heap(|module| {
        let ast = AstModule::parse("@rules_java", include_str!("java_defs.bzl").to_owned(), &Dialect::Extended)
            .map_err(|e| format!("{e}"))?;
        {
            let mut eval = Evaluator::new(&module);
            eval.eval_module(ast, &globals).map_err(|e| format!("{e}"))?;
        }
        module.freeze().map_err(|e| format!("{e:?}"))
    })
}

/// Record an analysis-visible target with no actions (a build-graph placeholder).
fn record_named(sess: &Session, name: &str) {
    record_target(sess, AnalyzedTarget {
        name: canon_label(sess, name),
        ..Default::default()
    });
}

/// `@bazel_skylib` rules razel recognizes as no-op/minimal targets: `bzl_library`,
/// the build/diff test wrappers, the `common_settings` build-setting flags, and the
/// small codegen rules. razel enforces no build settings and tracks no .bzl
/// libraries, so these are analysis-visible placeholders (they build to nothing).
/// skylib's *lib* helpers (`selects`/`paths`/`sets`) are pure-Starlark namespaces —
/// handled separately as TF reaches them.
#[starlark::starlark_module]
fn skylib_rules(b: &mut GlobalsBuilder) {
    fn native_bzl_library<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        record_named(session(eval), &name);
        Ok(NoneType)
    }
    fn native_build_test<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        record_named(session(eval), &name);
        Ok(NoneType)
    }
    fn native_diff_test<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        record_named(session(eval), &name);
        Ok(NoneType)
    }
    fn native_string_flag<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        record_named(session(eval), &name);
        Ok(NoneType)
    }
    fn native_bool_flag<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        record_named(session(eval), &name);
        Ok(NoneType)
    }
    fn native_int_flag<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        record_named(session(eval), &name);
        Ok(NoneType)
    }
    fn native_string_list_flag<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        record_named(session(eval), &name);
        Ok(NoneType)
    }
    fn native_string_setting<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        record_named(session(eval), &name);
        Ok(NoneType)
    }
    fn native_expand_template<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        record_named(session(eval), &name);
        Ok(NoneType)
    }
    fn native_copy_file<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        record_named(session(eval), &name);
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
    fn package_name<'v>(eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<String> {
        Ok(session(eval).current_pkg.borrow().clone().unwrap_or_default())
    }
    fn repository_name() -> anyhow::Result<String> {
        Ok("@".to_string())
    }
    fn glob<'v>(
        #[starlark(require = pos)] include: UnpackList<String>,
        #[starlark(require = named)] exclude: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Vec<String>> {
        do_glob(session(eval), include.items, exclude.map(|l| l.items).unwrap_or_default())
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

/// The `razel_build` builtin namespace (RazelStarlarkBoundaryPlan §10 C1): the GENERIC build engine
/// exposed to Starlark — the four-move surface (toolchain / command_line / action / info). C1a ships
/// `command_line`, now **toolchain-parameterized** (was the cc-hardcoded `razel_cc.command_line`): the
/// feature config is resolved from the `toolchain` name, not baked in — the cc-specificity left the
/// surface. (`action` is `ctx.actions.run`, already the unified move; `toolchain`/`info` are C1b/C2.)
#[starlark::starlark_module]
fn razel_build_members(b: &mut GlobalsBuilder) {
    /// `razel_build.command_line(toolchain, action, variables)` → the §8c argv: `Constrain` selects
    /// the toolchain's default features and expands them with `variables` (a dict of `str | [str]`).
    /// `toolchain` names the toolchain ("cc"); the config is resolved from it (no cc-hardcoding).
    fn command_line<'v>(
        toolchain: &str,
        action: &str,
        variables: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        use razel_rulepack::constrain::{VarValue, Vars};
        let config = match toolchain {
            "cc" => razel_cc_toolchain::macos_core_config().map_err(|e| anyhow::anyhow!(e))?,
            other => {
                return Err(anyhow::anyhow!(
                    "razel_build.command_line: unknown toolchain {other:?} — C1a resolves only \"cc\" \
                     (java is template-shaped + uses ctx.actions.run; the toolchain resolver is C1b/D)"
                ));
            }
        };
        let mut vars = Vars::new();
        for (k, v) in &variables {
            if let Some(s) = v.unpack_str() {
                vars.insert(k.clone(), VarValue::Scalar(s.to_string()));
            } else if let Some(list) = ListRef::from_value(*v) {
                let items = list.iter().filter_map(|x| x.unpack_str().map(String::from)).collect();
                vars.insert(k.clone(), VarValue::Sequence(items));
            }
        }
        let enabled = config.select(&[]);
        Ok(eval.heap().alloc(config.full_command_line(&enabled, action, &vars)))
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
        b.namespace("razel_build", razel_build_members);
    })
    .build()
}

/// Resolve a `.bzl` load label to a file under `root`. `//pkg:f.bzl` → `root/pkg/f.bzl`;
/// `:f.bzl` → `root/<current pkg>/f.bzl`. External repos other than `@rules_cc` error.
fn resolve_bzl(root: &Path, label: &str, current_pkg: Option<&str>) -> Result<PathBuf, String> {
    if let Some(rest) = label.strip_prefix("//") {
        let (pkg, file) = rest
            .split_once(':')
            .ok_or_else(|| format!("bad .bzl label `{label}`"))?;
        Ok(root.join(pkg).join(file))
    } else if let Some(file) = label.strip_prefix(':') {
        let pkg = current_pkg.unwrap_or_default();
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
    session: &'a Session,
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
        let root = self
            .session
            .workspace
            .clone()
            .ok_or_else(|| err(format!("load(\"{path}\") needs workspace mode")))?;
        let cur = self.session.current_pkg.borrow().clone();
        let fs = resolve_bzl(&root, path, cur.as_deref()).map_err(err)?;
        let src = std::fs::read_to_string(&fs)
            .map_err(|e| err(format!("cannot read {}: {e}", fs.display())))?;

        let frozen = Module::with_temp_heap(|module| -> starlark::Result<FrozenModule> {
            let ast = AstModule::parse(path, src, &Dialect::Extended)?;
            {
                let mut eval = Evaluator::new(&module);
                eval.set_loader(self); // recursive: a .bzl may load other .bzl
                eval.extra = Some(self.session);
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
fn ruleset_modules(cc_toolchain: CcToolchainMode) -> Result<Vec<Ruleset>, String> {
    Ok(vec![
        Ruleset {
            prefix: "@rules_cc//",
            module: rules_cc_module(cc_toolchain)?,
        },
        Ruleset {
            prefix: "@rules_java//",
            module: rules_java_module()?, // razel's java:defs.bzl (no toolchain mode — F16)
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
fn eval_build_src(session: &Session, name: &str, src: &str) -> Result<(), String> {
    let rulesets = ruleset_modules(session.global.cc_toolchain)?;
    let globals = build_globals();
    let loader = BzlLoader {
        rulesets: &rulesets,
        globals: &globals,
        cache: RefCell::new(HashMap::new()),
        session,
    };
    let ast =
        AstModule::parse(name, src.to_owned(), &Dialect::Extended).map_err(|e| format!("{e}"))?;
    Module::with_temp_heap(|module| {
        let mut eval = Evaluator::new(&module);
        eval.set_loader(&loader);
        eval.extra = Some(session); // builtins read the Session via `session(eval)`
        eval.eval_module(ast, &globals)
            .map_err(|e| format!("{e}"))?;
        Ok(())
    })
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
    let session = Session::new(None, flags);
    eval_build_src(&session, "BUILD", build_src)?;
    Ok(session.take_targets())
}

/// Load a package's BUILD (once) under workspace mode, evaluating it with that
/// package as context. Cross-package deps trigger further loads via `resolve_dep`.
fn load_package(sess: &Session, pkg: &str) -> Result<(), String> {
    if sess.loaded.borrow().contains(pkg) {
        return Ok(());
    }
    sess.loaded.borrow_mut().insert(pkg.to_string());
    let root = sess
        .workspace
        .clone()
        .ok_or("load_package called outside workspace mode")?;
    let build_path = ["BUILD", "BUILD.bazel"]
        .iter()
        .map(|f| root.join(pkg).join(f))
        .find(|p| p.exists())
        .ok_or_else(|| format!("no BUILD in package `{pkg}` ({})", root.join(pkg).display()))?;
    let src = std::fs::read_to_string(&build_path).map_err(|e| e.to_string())?;

    // Short borrows around the nested eval (the [R1] discipline): set current_pkg, drop the
    // borrow, recurse, then restore — never hold a Session borrow across `eval_build_src`.
    let prev = sess.current_pkg.borrow_mut().replace(pkg.to_string());
    let res = eval_build_src(sess, &format!("{pkg}/BUILD"), &src);
    *sess.current_pkg.borrow_mut() = prev;
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
    let session = Session::new(Some(root.to_path_buf()), flags);
    let top_pkg = pkg_of(&canon_label(&session, top_label))
        .ok_or_else(|| format!("top label must be //pkg:name, got `{top_label}`"))?;
    load_package(&session, &top_pkg)?;
    Ok(session.take_targets())
}

/// Evaluate a `BUILD`/`.bzl` that defines and instantiates Starlark rules, running each
/// rule impl (same-scope analysis); returns the analyzed targets.
pub fn analyze_starlark(name: &str, src: &str) -> Result<Vec<AnalyzedTarget>, String> {
    let session = Session::default();
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
    .with(|b| {
        b.namespace("razel_build", razel_build_members);
    })
    .build();
    let res: Result<(), String> = Module::with_temp_heap(|module| {
        let mut eval = Evaluator::new(&module);
        eval.extra = Some(&session);
        eval.eval_module(ast, &globals)
            .map_err(|e| format!("{e}"))?;
        Ok(())
    });
    res?;
    Ok(session.take_targets())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── fold_field (F3/F24): the LIVE transitive fold, tested directly (not only via the .bzl). ──


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
