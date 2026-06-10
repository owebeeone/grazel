//! Starlark value types for rule impls (ctx.actions / Args / File / depset). C0.

use crate::state::{session, with_current, AnalyzedAction};
use allocative::Allocative;
use starlark::any::ProvidesStaticType;
use starlark::collections::SmallMap;
use starlark::environment::{
    Methods, MethodsBuilder,
};
use starlark::eval::Evaluator;
use starlark::values::list::{ListRef, UnpackList};
use starlark::values::none::NoneType;
use starlark::coerce::Coerce;
use starlark::starlark_complex_value;
use starlark::values::{
    Freeze, Heap, NoSerialize, StarlarkValue, Trace, Value, ValueLike,
    starlark_value,
};
use std::cell::RefCell;
use std::fmt;



/// Single-quote a string for safe embedding in a `/bin/sh -c` script.
pub(crate) fn shquote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}


// ---- ctx.actions ----------------------------------------------------------------


#[derive(Debug, NoSerialize, ProvidesStaticType, Allocative, Trace)]
pub(crate) struct Actions;


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
pub(crate) fn actions_methods(b: &mut MethodsBuilder) {
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
        push_run_action(eval, executable, outputs, inputs, arguments, mnemonic);
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
    /// `ctx.actions.symlink(output, target_file)` — a real `ln -sf` action.
    fn symlink<'v>(
        #[starlark(this)] _this: Value<'v>,
        #[starlark(require = named)] output: Value<'v>,
        #[starlark(require = named)] target_file: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let output = file_path(output);
        let target = target_file.map(file_path).unwrap_or_default();
        with_current(sess, |c| {
            c.actions.push(AnalyzedAction {
                mnemonic: "Symlink".into(),
                argv: vec!["/bin/ln".into(), "-sf".into(), target.clone(), output.clone()],
                inputs: vec![target.clone()],
                outputs: vec![output.clone()],
            })
        });
        Ok(NoneType)
    }
    /// `ctx.actions.declare_directory` / `expand_template` / `run_shell` — loading-grade.
    fn declare_directory<'v>(
        #[starlark(this)] _this: Value<'v>,
        filename: String,
    ) -> anyhow::Result<String> {
        Ok(filename)
    }
    fn run_shell<'v>(
        #[starlark(this)] _this: Value<'v>,
        #[starlark(require = named)] command: Option<Value<'v>>,
        #[starlark(require = named)] outputs: Option<UnpackList<Value<'v>>>,
        #[starlark(require = named)] inputs: Option<UnpackList<Value<'v>>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let cmd = command.map(file_path).unwrap_or_default();
        let outs: Vec<String> = outputs.map(|l| l.items.iter().map(|v| file_path(*v)).collect()).unwrap_or_default();
        let ins: Vec<String> = inputs.map(|l| l.items.iter().map(|v| file_path(*v)).collect()).unwrap_or_default();
        with_current(sess, |c| {
            c.actions.push(AnalyzedAction {
                mnemonic: "RunShell".into(),
                argv: vec!["/bin/bash".into(), "-c".into(), cmd.clone()],
                inputs: ins.clone(),
                outputs: outs.clone(),
            })
        });
        Ok(NoneType)
    }
    fn write<'v>(
        #[starlark(this)] _this: Value<'v>,
        #[starlark(require = named)] output: Value<'v>,
        #[starlark(require = named)] content: Option<String>,
        #[starlark(require = named)] is_executable: Option<bool>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let output = file_path(output);
        // Real write: a /bin/sh action printf-ing the content into the output file
        // (+ chmod when is_executable — the rules_rust launcher-script shape).
        let mut script = format!(
            "printf '%s' {} > {}",
            shquote(&content.unwrap_or_default()),
            shquote(&output)
        );
        if is_executable.unwrap_or(false) {
            script.push_str(&format!(" && chmod +x {}", shquote(&output)));
        }
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
pub(crate) struct Args {
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
pub(crate) fn args_methods(b: &mut MethodsBuilder) {
    /// `add(arg)` or the Bazel two-positional `add("--flag", value)`.
    fn add<'v>(
        #[starlark(this)] this: Value<'v>,
        #[starlark(require = pos)] arg: Value<'v>,
        #[starlark(require = pos)] value: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        if let Some(a) = this.downcast_ref::<Args>() {
            a.items.borrow_mut().push(file_path(arg));
            if let Some(v) = value {
                a.items.borrow_mut().push(file_path(v));
            }
        }
        Ok(NoneType)
    }
    /// `add_all(values, before_each=?, format_each=?, map_each=?)` — D3 fidelity (the rules_rust
    /// shapes). `map_each` runs per element and may return a string, a list, or `None` (skip);
    /// `format_each` is Bazel's single-`%s` format; `before_each` interleaves.
    fn add_all<'v>(
        #[starlark(this)] this: Value<'v>,
        #[starlark(require = pos)] values: Value<'v>,
        #[starlark(require = named)] before_each: Option<String>,
        #[starlark(require = named)] format_each: Option<String>,
        #[starlark(require = named)] map_each: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        // Map first (on raw element VALUES — File fields etc. stay live), then stringify.
        let mut strs: Vec<String> = Vec::new();
        for e in flatten_values(values) {
            match map_each {
                Some(f) => {
                    let r = eval
                        .eval_function(f, &[e], &[])
                        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                    if r.is_none() {
                        continue;
                    }
                    if let Some(l) = ListRef::from_value(r) {
                        strs.extend(l.iter().map(file_path));
                    } else {
                        strs.push(file_path(r));
                    }
                }
                None => strs.push(file_path(e)),
            }
        }
        if let Some(a) = this.downcast_ref::<Args>() {
            let mut items = a.items.borrow_mut();
            for s in strs {
                if let Some(b) = &before_each {
                    items.push(b.clone());
                }
                items.push(match &format_each {
                    Some(fmt) => fmt.replace("%s", &s),
                    None => s,
                });
            }
        }
        Ok(NoneType)
    }
}

/// The element VALUES of an `add_all` collection (list or depset; a scalar is itself) — kept as
/// values so `map_each` sees live objects (File fields etc.) rather than pre-stringified paths.
fn flatten_values<'v>(v: Value<'v>) -> Vec<Value<'v>> {
    if let Some(list) = ListRef::from_value(v) {
        return list.iter().flat_map(flatten_values).collect();
    }
    if let Some(d) = v.downcast_ref::<Depset>() {
        // Items are live Values — return them directly so map_each sees File/.path etc.
        return d.items.clone();
    }
    vec![v]
}


/// Flatten a `run(arguments=…)` element into argv strings: an [`Args`] yields its
/// Register an action on the current target (inputs/outputs/argv). The shared core behind BOTH
/// `ctx.actions.run` (the Bazel-dialect API) and `razel_build.action` (the engine's named move, C1b) —
/// so the two surfaces are byte-identical by construction.
pub(crate) fn push_run_action<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    executable: Option<Value<'v>>,
    outputs: Option<UnpackList<Value<'v>>>,
    inputs: Option<UnpackList<Value<'v>>>,
    arguments: Option<UnpackList<Value<'v>>>,
    mnemonic: Option<String>,
) {
    let sess = session(eval);
    let exe = executable.map(file_path).unwrap_or_else(|| "run".into());
    let mut argv = vec![exe.clone()];
    // arguments may contain plain strings, lists, File values, and args() objects.
    for a in arguments.map(|l| l.items).unwrap_or_default() {
        argv.extend(flatten_arg(a));
    }
    let paths = |l: Option<UnpackList<Value<'v>>>| -> Vec<String> {
        l.map(|l| l.items.into_iter().map(file_path).collect()).unwrap_or_default()
    };
    with_current(sess, |c| {
        c.actions.push(AnalyzedAction {
            mnemonic: mnemonic.unwrap_or(exe),
            argv,
            inputs: paths(inputs),
            outputs: paths(outputs),
        })
    });
}

/// accumulated items, a list recurses, a [`File`] yields its path, anything else
/// stringifies.
pub(crate) fn flatten_arg(v: Value) -> Vec<String> {
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
#[derive(Debug, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct File {
    pub(crate) path: String,
}
starlark::starlark_simple_value!(File);


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
            // Source vs generated: razel's loading-grade heuristic — generated paths live under
            // an output prefix (bazel-out/); everything else is a source file.
            "is_source" => Some(Value::new_bool(!p.starts_with("bazel-out/"))),
            "owner" => Some(Value::new_none()),
            "root" => Some(heap.alloc(crate::engine::Absorb)),
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
pub(crate) fn file_path(v: Value) -> String {
    if let Some(f) = v.downcast_ref::<File>() {
        return f.path.clone();
    }
    v.unpack_str()
        .map(str::to_string)
        .unwrap_or_else(|| v.to_str())
}


// ---- depset ----------------------------------------------------------------------


/// A `depset` — Bazel's deduplicated transitive set. Elements are kept as live
/// heap Values so `map_each` lambdas can access fields like `.path` on File values.
/// Construction folds in `direct` members and the elements of each `transitive`
/// depset, de-duplicated by their string path. Stringification happens at use
/// (`to_list`, `extract_files`, `Display`), not at construction.
#[derive(Debug, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub(crate) struct DepsetGen<V: starlark::values::ValueLifetimeless> {
    // items holds live GC-visible Values — Trace must NOT be skipped. Freeze-generic: real .bzl
    // build module-level depsets (protobuf), which freeze with their module.
    pub(crate) items: Vec<V>,
}
starlark_complex_value!(pub(crate) Depset);


impl<V: starlark::values::ValueLifetimeless> fmt::Display for DepsetGen<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "depset({} items)", self.items.len())
    }
}


#[starlark_value(type = "depset")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for DepsetGen<V>
where
    Self: starlark::any::ProvidesStaticType<'v>,
{
    fn get_methods() -> Option<&'static Methods> {
        Some(DEPSET_METHODS.methods())
    }
}

starlark::methods_static!(DEPSET_METHODS = depset_methods);


#[starlark::starlark_module]
pub(crate) fn depset_methods(b: &mut MethodsBuilder) {
    fn to_list<'v>(
        #[starlark(this)] this: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // Stringify at use: callers that built string-consuming pipelines keep working.
        let paths: Vec<String> = this
            .downcast_ref::<Depset>()
            .map(|d| d.items.iter().map(|v| file_path(*v)).collect())
            .unwrap_or_default();
        Ok(eval.heap().alloc(paths))
    }
}


/// Extract member paths from a `DefaultInfo(files=…)` value: a [`Depset`]'s members,
/// a list's elements (Files/strings), or a single value. Stringifies at use.
pub(crate) fn extract_files(v: Value) -> Vec<String> {
    if let Some(ds) = v.downcast_ref::<Depset>() {
        return ds.items.iter().map(|v| file_path(*v)).collect();
    }
    if let Some(list) = ListRef::from_value(v) {
        return list.iter().map(file_path).collect();
    }
    vec![file_path(v)]
}



/// `Option<UnpackList<T>>` -> `Vec<T>` (an omitted Starlark list attr means empty). A value helper.
pub(crate) fn unpack(list: Option<UnpackList<String>>) -> Vec<String> {
    list.map(|l| l.items).unwrap_or_default()
}
