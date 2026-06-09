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
use starlark::values::{
    Heap, NoSerialize, StarlarkValue, Trace, Value, ValueLike,
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
#[derive(Debug, NoSerialize, ProvidesStaticType, Allocative, Trace)]
pub(crate) struct File {
    #[trace(unsafe_ignore)]
    pub(crate) path: String,
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
pub(crate) fn file_path(v: Value) -> String {
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
pub(crate) struct Depset {
    #[trace(unsafe_ignore)]
    pub(crate) items: Vec<String>,
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
pub(crate) fn depset_methods(b: &mut MethodsBuilder) {
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
pub(crate) fn extract_files(v: Value) -> Vec<String> {
    if let Some(ds) = v.downcast_ref::<Depset>() {
        return ds.items.clone();
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
