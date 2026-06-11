//! Fetch R1 (RazelFetchPlan §3): WORKSPACE spec extraction — no network, no
//! repository_ctx. `repository_rule` binds to a RECORDER value, the REAL repo-rule wrapper
//! macros (`tf_http_archive` and friends) run with their mirror/patch post-processing, and
//! every instantiation records a [`RepoSpec`]. The lockfile/dry-run writer lives in xtask;
//! this module owns the eval.

use crate::state::{GlobalFlags, Session, session};
use allocative::Allocative;
use starlark::any::ProvidesStaticType;
use starlark::coerce::Coerce;
use starlark::environment::{GlobalsBuilder, Module};
use starlark::eval::{Arguments, Evaluator};
use starlark::starlark_complex_value;
use starlark::syntax::{AstModule, Dialect};
use starlark::values::{
    Freeze, NoSerialize, StarlarkValue, Trace, Value, ValueLifetimeless, ValueLike,
    starlark_value,
};
use std::fmt;
use std::path::Path;

/// A recorded repository-rule attribute value (the lockfile's value algebra — plain data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttrV {
    Str(String),
    List(Vec<String>),
    Dict(Vec<(String, String)>),
    Bool(bool),
    Int(i64),
}

/// One recorded external-repo declaration: `kind` is the repository_rule's implementation
/// function (informational), `attrs` the POST-WRAPPER kwargs in declaration order.
#[derive(Debug, Clone)]
pub struct RepoSpec {
    pub name: String,
    pub kind: String,
    pub attrs: Vec<(String, AttrV)>,
}

fn to_attrv(v: Value) -> Option<AttrV> {
    if v.is_none() {
        return None;
    }
    if let Some(b) = v.unpack_bool() {
        return Some(AttrV::Bool(b));
    }
    if let Some(i) = v.unpack_i32() {
        return Some(AttrV::Int(i as i64));
    }
    if let Some(s) = v.unpack_str() {
        return Some(AttrV::Str(s.to_string()));
    }
    if let Some(l) = starlark::values::list::ListRef::from_value(v) {
        return Some(AttrV::List(
            l.iter().map(|x| x.unpack_str().map(String::from).unwrap_or_else(|| x.to_string())).collect(),
        ));
    }
    if let Some(d) = starlark::values::dict::DictRef::from_value(v) {
        return Some(AttrV::Dict(
            d.iter()
                .map(|(k, x)| {
                    (
                        k.unpack_str().map(String::from).unwrap_or_else(|| k.to_string()),
                        x.unpack_str().map(String::from).unwrap_or_else(|| x.to_string()),
                    )
                })
                .collect(),
        ));
    }
    // Unknown shapes record as their repr — loud in the lockfile, never silently dropped.
    Some(AttrV::Str(v.to_string()))
}

/// The `repository_rule(...)` RECORDER value: calling the rule records a [`RepoSpec`] on
/// the Session instead of executing a repository_ctx. Generic over `V` so it survives
/// `module.freeze()` (repo.bzl is load()ed and frozen, like every .bzl).
#[derive(Debug, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub(crate) struct RepoRuleGen<V: ValueLifetimeless> {
    pub(crate) implementation: V,
}

starlark_complex_value!(pub(crate) RepoRule);

impl<V: ValueLifetimeless> fmt::Display for RepoRuleGen<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<repository_rule>")
    }
}

#[starlark_value(type = "repository_rule")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for RepoRuleGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let named = args.names_map()?;
        let mut name = String::new();
        let mut attrs: Vec<(String, AttrV)> = Vec::new();
        for (k, v) in named.iter() {
            if k.as_str() == "name" {
                name = v.unpack_str().unwrap_or_default().to_string();
                continue;
            }
            if let Some(a) = to_attrv(*v) {
                attrs.push((k.as_str().to_string(), a));
            }
        }
        // The implementation fn's repr names the kind ("<function _tf_http_archive_impl>").
        let kind = self
            .implementation
            .to_value()
            .to_string()
            .trim_start_matches("<function ")
            .trim_end_matches('>')
            .trim()
            .to_string();
        session(eval).repo_specs.borrow_mut().push(RepoSpec { name, kind, attrs });
        Ok(Value::new_none())
    }
}

/// `repository_rule` — a universal `.bzl` global in Bazel (definition is unrestricted;
/// only instantiation is WORKSPACE-scoped). razel binds the RECORDER everywhere: outside
/// extraction an instantiation records into a `repo_specs` nobody reads — harmless.
#[starlark::starlark_module]
pub(crate) fn repo_rule_globals(b: &mut GlobalsBuilder) {
    fn repository_rule<'v>(
        #[starlark(require = named)] implementation: Value<'v>,
        #[starlark(kwargs)] _kw: starlark::collections::SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(eval.heap().alloc(RepoRule { implementation }))
    }
}

/// WORKSPACE-only globals: `workspace(name=)`, toolchain/platform registration no-ops,
/// legacy `bind()`. Everything else is the normal BUILD surface.
#[starlark::starlark_module]
pub(crate) fn workspace_globals(b: &mut GlobalsBuilder) {
    fn workspace(
        #[starlark(require = named)] name: String,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        let _ = name;
        Ok(starlark::values::none::NoneType)
    }

    fn register_toolchains<'v>(
        #[starlark(args)] _a: starlark::values::tuple::UnpackTuple<Value<'v>>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        Ok(starlark::values::none::NoneType)
    }

    fn register_execution_platforms<'v>(
        #[starlark(args)] _a: starlark::values::tuple::UnpackTuple<Value<'v>>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        Ok(starlark::values::none::NoneType)
    }

    /// WORKSPACE `bind()` (legacy //external aliases) — recorded nowhere, absorbed loudly.
    fn bind<'v>(
        #[starlark(kwargs)] _kw: starlark::collections::SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        Ok(starlark::values::none::NoneType)
    }
}

/// Evaluate `<root>/WORKSPACE` (the chain: its load()s pull workspace*.bzl and the real
/// repo.bzl through the normal loader against vendored/host trees) and return every
/// recorded repo spec, in declaration order, plus extraction notes. No network.
pub fn extract_workspace_repos(
    root: &Path,
    flags: GlobalFlags,
) -> Result<(Vec<RepoSpec>, Vec<String>), String> {
    let session = Session::new(Some(root.to_path_buf()), flags);
    let path = ["WORKSPACE", "WORKSPACE.bazel"]
        .iter()
        .map(|f| root.join(f))
        .find(|p| p.exists())
        .ok_or_else(|| format!("no WORKSPACE under {}", root.display()))?;
    let src =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    crate::rules::eval_workspace_src(&session, "WORKSPACE", &src)?;
    let specs = std::mem::take(&mut *session.repo_specs.borrow_mut());
    let notes = std::mem::take(&mut *session.warned.borrow_mut()).into_iter().collect();
    Ok((specs, notes))
}
