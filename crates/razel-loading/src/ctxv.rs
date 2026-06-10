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
            "fragments" | "configuration" => Some(ctx_absorb(_heap)),
            "build_setting_value" => Some(self.build_setting_value),
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
