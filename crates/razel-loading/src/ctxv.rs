//! The analysis `ctx` value + ctx.toolchains (split from dialect.rs — the C0 discipline).

use allocative::Allocative;
use starlark::any::ProvidesStaticType;
use starlark::values::{
    Heap, NoSerialize, StarlarkValue, Trace, Value, ValueLike,
    starlark_value,
};
use std::fmt;
use crate::labels::LabelV;
use crate::selects::key_string;


// ---- ctx ------------------------------------------------------------------------


/// The analysis `ctx`. All fields are heap `Value`s so it traces cleanly, no freezing.
#[derive(Debug, NoSerialize, ProvidesStaticType, Allocative, Trace)]
pub(crate) struct Ctx<'v> {
    pub(crate) attr: Value<'v>,
    pub(crate) actions: Value<'v>,
    pub(crate) label: Value<'v>,
    /// `ctx.outputs.<name>` — predeclared output filenames (package-qualified).
    pub(crate) outputs: Value<'v>,
    /// `ctx.files.<name>` — the source files of label/label_list attrs (qualified).
    pub(crate) files: Value<'v>,
    /// `ctx.executable.<name>` — the runnable output of an executable-label attr.
    pub(crate) executable: Value<'v>,
    /// `ctx.toolchains[type]` — the registered host toolchain stand-ins (Layer 1; L3 resolves).
    pub(crate) toolchains: Value<'v>,
    /// `ctx.build_setting_value` — the declared `build_setting_default` (no flag overrides yet).
    pub(crate) build_setting_value: Value<'v>,
    /// `ctx.file.<attr>` — single file of a label attr (first of files; None when absent).
    pub(crate) file: Value<'v>,
    /// `ctx.var` — the Make-variable dict (COMPILATION_MODE/TARGET_CPU/BINDIR).
    pub(crate) var: Value<'v>,
}


/// Absorbed ctx members (`fragments`, …): any access resolves; surfaces only at real use.
pub(crate) fn ctx_absorb<'v>(heap: Heap<'v>) -> Value<'v> {
    heap.alloc(crate::engine::Absorb)
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
            "toolchains" => Some(self.toolchains),
            // Layer-2 members, ABSORBED for analysis-shape (real values = Layer-3 action
            // goldens; registered debt): file/runfiles/expand_location/expand_make_variables,
            // bin_dir/genfiles_dir/var/workspace_name/features.
            "fragments" | "configuration" | "runfiles" | "expand_location"
            | "expand_make_variables" | "bin_dir" | "genfiles_dir" | "coverage_instrumented" => {
                Some(ctx_absorb(_heap))
            }
            "workspace_name" => Some(_heap.alloc("")),
            "features" | "disabled_features" => Some(_heap.alloc(Vec::<Value<'v>>::new())),
            "build_setting_value" => Some(self.build_setting_value),
            "file" => Some(self.file),
            "var" => Some(self.var),
            _ => None,
        }
    }
}


// ---- ctx.toolchains (Layer 1) ------------------------------------------------------------------


/// `ctx.toolchains` — type-label → toolchain stand-in (the registered host rows; real
/// `rule(toolchains=)`-driven RESOLUTION is L3). Supports `ctx.toolchains[type]` and
/// `type in ctx.toolchains`; keys are strings or `Label`s.
#[derive(Debug, NoSerialize, ProvidesStaticType, Allocative, Trace)]
pub(crate) struct ToolchainMap<'v> {
    entries: Vec<(String, Value<'v>)>,
}


impl fmt::Display for ToolchainMap<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<toolchains>")
    }
}


#[starlark_value(type = "toolchain_map")]
impl<'v> StarlarkValue<'v> for ToolchainMap<'v> {
    fn at(&self, index: Value<'v>, heap: Heap<'v>) -> starlark::Result<Value<'v>> {
        let Some(key) = key_string(heap, index) else {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "toolchain key must be a label"
            )));
        };
        self.entries
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
            .ok_or_else(|| {
                starlark::Error::new_other(anyhow::anyhow!(
                    "no toolchain registered for `{key}` (razel host rows: toolchains.rs)"
                ))
            })
    }
    fn is_in(&self, other: Value<'v>) -> starlark::Result<bool> {
        // `key in ctx.toolchains` — key stringification without a heap: strings + LabelV display.
        let key = other
            .unpack_str()
            .map(String::from)
            .or_else(|| other.downcast_ref::<LabelV>().map(|l| l.to_string()));
        Ok(key.is_some_and(|k| self.entries.iter().any(|(e, _)| *e == k)))
    }
}


/// Build the ctx.toolchains map from the registered host rows.
pub(crate) fn toolchain_map<'v>(heap: Heap<'v>) -> Value<'v> {
    let entries = crate::toolchains::toolchain_rows(heap);
    heap.alloc_complex_no_freeze(ToolchainMap { entries })
}


/// `ctx.files` — declared fields, with MISSING attrs defaulting to `[]` (Bazel: every schema
/// attr exists on ctx.files; omitted label_lists are empty).
#[derive(Debug, NoSerialize, ProvidesStaticType, Allocative, Trace)]
pub(crate) struct FilesNs<'v> {
    pub(crate) fields: Vec<(String, Value<'v>)>,
}

impl fmt::Display for FilesNs<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<ctx.files>")
    }
}

#[starlark_value(type = "ctx_files")]
impl<'v> StarlarkValue<'v> for FilesNs<'v> {
    fn get_attr(&self, attribute: &str, heap: Heap<'v>) -> Option<Value<'v>> {
        Some(
            self.fields
                .iter()
                .find(|(k, _)| k == attribute)
                .map(|(_, v)| *v)
                .unwrap_or_else(|| heap.alloc(Vec::<Value<'v>>::new())),
        )
    }
}


/// `ctx.file` — the SINGLE file of a label attr (first of `ctx.files.<attr>`; `None` if absent).
#[derive(Debug, NoSerialize, ProvidesStaticType, Allocative, Trace)]
pub(crate) struct FileNs<'v> {
    pub(crate) fields: Vec<(String, Value<'v>)>,
}

impl fmt::Display for FileNs<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<ctx.file>")
    }
}

#[starlark_value(type = "ctx_file")]
impl<'v> StarlarkValue<'v> for FileNs<'v> {
    fn get_attr(&self, attribute: &str, _heap: Heap<'v>) -> Option<Value<'v>> {
        let v = self.fields.iter().find(|(k, _)| k == attribute).map(|(_, v)| *v);
        Some(match v {
            Some(list) => starlark::values::list::ListRef::from_value(list)
                .and_then(|l| l.iter().next())
                .unwrap_or_else(Value::new_none),
            None => Value::new_none(),
        })
    }
}


/// `ctx.executable` — executable files of executable-label attrs; MISSING attrs are `None`
/// (Bazel: the attr exists; non-executable/omitted → None).
#[derive(Debug, NoSerialize, ProvidesStaticType, Allocative, Trace)]
pub(crate) struct ExecNs<'v> {
    pub(crate) fields: Vec<(String, Value<'v>)>,
}

impl fmt::Display for ExecNs<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<ctx.executable>")
    }
}

#[starlark_value(type = "ctx_executable")]
impl<'v> StarlarkValue<'v> for ExecNs<'v> {
    fn get_attr(&self, attribute: &str, _heap: Heap<'v>) -> Option<Value<'v>> {
        Some(
            self.fields
                .iter()
                .find(|(k, _)| k == attribute)
                .map(|(_, v)| *v)
                .unwrap_or_else(Value::new_none),
        )
    }
}
