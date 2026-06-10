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
    /// `declare_file` returns a real `File` (impls read `.path`); the path keeps razel's
    /// package-computed form (callers pass package-relative or full output paths).
    fn declare_file<'v>(
        #[starlark(this)] _this: Value<'v>,
        filename: String,
        #[starlark(require = named)] sibling: Option<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // Outputs live in the declaring package's output dir (exec-root form) — owner /
        // workspace_root derive from the path.
        let path = match sibling.map(file_path).as_deref().and_then(|s| s.rsplit_once('/')) {
            Some((dir, _)) => format!("{dir}/{filename}"),
            None => crate::state::qualify(session(eval), &filename),
        };
        Ok(eval.heap().alloc(File { path }))
    }
    fn run<'v>(
        #[starlark(this)] _this: Value<'v>,
        #[starlark(require = named)] executable: Option<Value<'v>>,
        #[starlark(require = named)] outputs: Option<Value<'v>>,
        #[starlark(require = named)] inputs: Option<Value<'v>>,
        #[starlark(require = named)] arguments: Option<Value<'v>>,
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
        #[starlark(require = named)] sibling: Option<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // sibling: declare next to an existing file (same directory).
        let path = match sibling.map(file_path).as_deref().and_then(|s| s.rsplit_once('/')) {
            Some((dir, _)) => format!("{dir}/{filename}"),
            None => crate::state::qualify(session(eval), &filename),
        };
        Ok(eval.heap().alloc(File { path }))
    }
    /// `expand_template(template=, output=, substitutions=)` — a real sed-style action.
    fn expand_template<'v>(
        #[starlark(this)] _this: Value<'v>,
        #[starlark(require = named)] template: Value<'v>,
        #[starlark(require = named)] output: Value<'v>,
        #[starlark(require = named)] substitutions: Option<SmallMap<String, String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let tpl = file_path(template);
        let out = file_path(output);
        // One sed program applying every substitution (literal, not regex — escape specials).
        let esc = |s: &str| s.replace(['\\', '/', '&'], "_").replace('\n', " ");
        let prog: String = substitutions
            .map(|m| {
                m.iter()
                    .map(|(k, v)| format!("s/{}/{}/g;", esc(k), esc(v)))
                    .collect()
            })
            .unwrap_or_default();
        with_current(sess, |c| {
            c.actions.push(AnalyzedAction {
                mnemonic: "TemplateExpand".into(),
                argv: vec!["/usr/bin/sed".into(), prog.clone(), tpl.clone()],
                inputs: vec![tpl.clone()],
                outputs: vec![out.clone()],
            })
        });
        Ok(NoneType)
    }
    fn run_shell<'v>(
        #[starlark(this)] _this: Value<'v>,
        #[starlark(require = named)] command: Option<Value<'v>>,
        #[starlark(require = named)] outputs: Option<Value<'v>>,
        #[starlark(require = named)] inputs: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let cmd = command.map(file_path).unwrap_or_default();
        let outs: Vec<String> =
            outputs.map(|v| flatten_values(v).into_iter().map(file_path).collect()).unwrap_or_default();
        let ins: Vec<String> =
            inputs.map(|v| flatten_values(v).into_iter().map(file_path).collect()).unwrap_or_default();
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
        output: Value<'v>,
        content: Option<String>,
        is_executable: Option<bool>,
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
    /// `add(arg)`, the two-positional `add("--flag", value)`, and `format=` (single-`%s`).
    fn add<'v>(
        #[starlark(this)] this: Value<'v>,
        #[starlark(require = pos)] arg: Value<'v>,
        #[starlark(require = pos)] value: Option<Value<'v>>,
        #[starlark(require = named)] format: Option<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        if let Some(a) = this.downcast_ref::<Args>() {
            let fmt = |s: String| match &format {
                Some(f) => f.replace("%s", &s),
                None => s,
            };
            match value {
                // Two-positional: the flag is literal; format applies to the VALUE.
                Some(v) => {
                    a.items.borrow_mut().push(file_path(arg));
                    a.items.borrow_mut().push(fmt(file_path(v)));
                }
                None => a.items.borrow_mut().push(fmt(file_path(arg))),
            }
        }
        Ok(NoneType)
    }
    /// Param-file surface (analysis-shape: recorded as no-ops; the inline argv is kept — the
    /// Layer-3 action golden makes @param-files real; registered debt).
    fn set_param_file_format<'v>(
        #[starlark(this)] _this: Value<'v>,
        #[starlark(require = pos)] _format: String,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn use_param_file<'v>(
        #[starlark(this)] _this: Value<'v>,
        _flag: Option<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    /// `add_joined(values, join_with=...)` — joined-list fidelity.
    fn add_joined<'v>(
        #[starlark(this)] this: Value<'v>,
        #[starlark(require = pos)] arg_or_values: Value<'v>,
        #[starlark(require = pos)] values_pos: Option<Value<'v>>,
        #[starlark(require = named)] join_with: Option<String>,
        #[starlark(require = named)] format_each: Option<String>,
        #[starlark(require = named)] map_each: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        // Bazel allows add_joined("--flag", values, ...) or add_joined(values, ...).
        let (flag, values) = match values_pos {
            Some(v) => (arg_or_values.unpack_str().map(String::from), v),
            None => (None, arg_or_values),
        };
        let map_each = map_each.filter(|v| !v.is_none());
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
        if let Some(fmt) = &format_each {
            strs = strs.iter().map(|s| fmt.replace("%s", s)).collect();
        }
        let joined = strs.join(join_with.as_deref().unwrap_or(" "));
        if let Some(a) = this.downcast_ref::<Args>() {
            let mut items = a.items.borrow_mut();
            if let Some(f) = flag {
                items.push(f);
            }
            if !joined.is_empty() {
                items.push(joined);
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
        // An explicit `map_each = None` means no mapping (Bazel allows it).
        let map_each = map_each.filter(|v| !v.is_none());
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
    outputs: Option<Value<'v>>,
    inputs: Option<Value<'v>>,
    arguments: Option<Value<'v>>,
    mnemonic: Option<String>,
) {
    let sess = session(eval);
    let exe = executable.map(file_path).unwrap_or_else(|| "run".into());
    let mut argv = vec![exe.clone()];
    // arguments may contain plain strings, lists, File values, and args() objects.
    if let Some(args) = arguments {
        argv.extend(flatten_arg(args));
    }
    // inputs/outputs accept lists OR depsets (Bazel's `depset|sequence`).
    let paths = |l: Option<Value<'v>>| -> Vec<String> {
        l.map(|v| flatten_values(v).into_iter().map(file_path).collect()).unwrap_or_default()
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
    /// Files key dicts in real impls (`artifact_label_map[artifact] = …`): identity = path.
    fn write_hash(&self, hasher: &mut starlark::collections::StarlarkHasher) -> starlark::Result<()> {
        use std::hash::Hash;
        self.path.hash(hasher);
        Ok(())
    }
    fn equals(&self, other: Value<'v>) -> starlark::Result<bool> {
        Ok(other.downcast_ref::<File>().is_some_and(|o| o.path == self.path))
    }
    fn get_attr(&self, name: &str, heap: Heap<'v>) -> Option<Value<'v>> {
        let p = self.path.as_str();
        let base = p.rsplit('/').next().unwrap_or(p);
        match name {
            "path" => Some(heap.alloc(p.to_string())),
            // Loading-grade: declared outputs/sources are files, not tree artifacts.
            "is_directory" => Some(Value::new_bool(false)),
            // Bazel short_path: external files are `../<repo>/<pkg>/<file>`.
            "short_path" => Some(match p.strip_prefix("external/") {
                Some(rest) => heap.alloc(format!("../{rest}")),
                None => heap.alloc(p.to_string()),
            }),
            // Source vs generated: razel's loading-grade heuristic — generated paths live under
            // an output prefix (bazel-out/); everything else is a source file.
            "is_source" => Some(Value::new_bool(!p.starts_with("bazel-out/"))),
            // The generating label, derived from the path (package = dir, name = base) —
            // enough for owner.package/owner.name comparisons in real impls.
            "owner" => {
                // `external/<repo>/…` paths derive an external owner label.
                let (repo, rel) = match self.path.strip_prefix("external/") {
                    Some(rest) => match rest.split_once('/') {
                        Some((r, rel)) => (Some(format!("@{r}")), rel),
                        None => (None, self.path.as_str()),
                    },
                    None => (None, self.path.as_str()),
                };
                let (pkg, name) = rel.rsplit_once('/').unwrap_or(("", rel));
                Some(heap.alloc(crate::labels::LabelV {
                    repo,
                    package: pkg.to_string(),
                    name: name.to_string(),
                }))
            }
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
    /// Bazel: an empty depset is falsy.
    fn to_bool(&self) -> bool {
        !self.items.is_empty()
    }
}

starlark::methods_static!(DEPSET_METHODS = depset_methods);


#[starlark::starlark_module]
pub(crate) fn depset_methods(b: &mut MethodsBuilder) {
    fn to_list<'v>(
        #[starlark(this)] this: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // LIVE members (T-001 semantics): File elements keep .path/.extension; consumers
        // that want strings go through file_path at use.
        let items: Vec<Value<'v>> = this
            .downcast_ref::<Depset>()
            .map(|d| d.items.clone())
            .unwrap_or_default();
        Ok(eval.heap().alloc(items))
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

/// A native-rule list attr captured as PLAIN DATA (native bodies can't hold Values): either a
/// stringified list, or select branches deferred to analysis time (Bazel's model — conditions
/// may be declared later in the file).
#[derive(Clone, Debug)]
pub(crate) enum StrAttrPart {
    Plain(Vec<String>),
    Branches(Vec<(String, Vec<String>)>),
}

/// Decompose a native-rule attr value into [`StrAttrPart`]s at DECLARE time.
pub(crate) fn str_attr_parts<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    v: Option<Value<'v>>,
) -> anyhow::Result<Vec<StrAttrPart>> {
    let Some(r) = v else { return Ok(Vec::new()) };
    let heap = eval.heap();
    let strs = |xs: Value<'v>| -> Vec<String> { unpack_strs_any(Some(xs)) };
    let branches = |pairs: &[(Value<'v>, Value<'v>)]| -> anyhow::Result<StrAttrPart> {
        let mut out = Vec::new();
        for (k, val) in pairs {
            let cond = crate::selects::key_string(heap, *k)
                .ok_or_else(|| anyhow::anyhow!("select(): condition key is not a label"))?;
            out.push((cond, strs(*val)));
        }
        Ok(StrAttrPart::Branches(out))
    };
    let decompose_one = |part: Value<'v>| -> anyhow::Result<StrAttrPart> {
        if let Some(b) = part.downcast_ref::<crate::selects::SelectBranches>() {
            branches(&b.branches)
        } else if let Some(b) = part.downcast_ref::<crate::selects::FrozenSelectBranches>() {
            let pairs: Vec<(Value<'v>, Value<'v>)> =
                b.branches.iter().map(|(k, v)| (k.to_value(), v.to_value())).collect();
            branches(&pairs)
        } else {
            Ok(StrAttrPart::Plain(strs(part)))
        }
    };
    if let Some(e) = r.downcast_ref::<crate::selects::SelectExpr>() {
        e.parts.iter().map(|p| decompose_one(*p)).collect()
    } else if let Some(e) = r.downcast_ref::<crate::selects::FrozenSelectExpr>() {
        e.parts.iter().map(|p| decompose_one(p.to_value())).collect()
    } else {
        Ok(vec![decompose_one(r)?])
    }
}

/// Resolve [`StrAttrPart`]s at ANALYSIS time (all conditions declared by now): reconstruct the
/// branch pairs and reuse `pick_branch` (full Bazel semantics — specialization, default).
pub(crate) fn resolve_str_parts<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    parts: &[StrAttrPart],
) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    for part in parts {
        match part {
            StrAttrPart::Plain(xs) => out.extend(xs.iter().cloned()),
            StrAttrPart::Branches(br) => {
                let heap = eval.heap();
                let pairs: Vec<(Value<'v>, Value<'v>)> = br
                    .iter()
                    .map(|(k, xs)| (heap.alloc(k.as_str()), heap.alloc(xs.clone())))
                    .collect();
                let picked = crate::selects::pick_branch(eval, &pairs, false, true)?
                    .unwrap_or_else(Value::new_none);
                out.extend(unpack_strs_any(Some(picked)));
            }
        }
    }
    Ok(out)
}

/// Unpack a `srcs`-style value that may be a LIST or a DEPSET (TF macros pass both),
/// elements string|Label|File — each stringifies to its label/path form.
pub(crate) fn unpack_strs_any<'v>(v: Option<Value<'v>>) -> Vec<String> {
    v.map(|v| {
        flatten_values(v)
            .into_iter()
            .map(|e| e.unpack_str().map(String::from).unwrap_or_else(|| file_path(e)))
            .collect()
    })
    .unwrap_or_default()
}

/// Unpack a `srcs`-style list whose elements may be strings OR Label values
/// (BUILD files pass both); each element stringifies to its label/path form.
pub(crate) fn unpack_strs<'v>(list: Option<UnpackList<Value<'v>>>) -> Vec<String> {
    list.map(|l| {
        l.items
            .into_iter()
            .map(|v| v.unpack_str().map(String::from).unwrap_or_else(|| v.to_string()))
            .collect()
    })
    .unwrap_or_default()
}
