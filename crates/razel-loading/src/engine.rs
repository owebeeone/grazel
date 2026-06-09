//! The engine namespaces exposed to .bzl: native.* / attr.* / razel_build.*. C0.

use starlark::collections::SmallMap;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::values::list::{ListRef, UnpackList};
use starlark::values::none::NoneType;
use starlark::values::Value;

#[allow(unused_imports)]
use crate::{
    deps::*, dialect::*, glob::*, native_cc::*, providers::*, shims::*, state::*,
    values::*,
};
#[allow(unused_imports)]
use crate::rules::*;


/// The globals available to BUILD and `.bzl` evaluation: `rule()`, `DefaultInfo`,
/// `select`, `define_config`, `glob`, + struct. (cc rules arrive via `load()`.)
/// The Bazel `native.*` namespace (minimal): package/repo introspection + `glob`.
/// razel treats the analyzed package as the only context (main repo), so
/// `package_name` is the current package and `repository_name` is `@`.
#[starlark::starlark_module]
pub(crate) fn native_members(b: &mut GlobalsBuilder) {
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
pub(crate) fn attr_members(b: &mut GlobalsBuilder) {
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
pub(crate) fn razel_build_members(b: &mut GlobalsBuilder) {
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

