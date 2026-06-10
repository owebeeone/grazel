//! The engine namespaces exposed to .bzl: native.* / attr.* / razel_build.*. C0.

use crate::state::session;
use crate::glob::do_glob;
use starlark::collections::SmallMap;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::values::list::{ListRef, UnpackList};
use starlark::values::none::NoneType;
use starlark::any::ProvidesStaticType;
use starlark::values::{NoSerialize, Value};



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
    /// `native.filegroup(...)` — real macros wrap the native rules (`tensorflow.bzl`'s
    /// filegroup macro calls `native.filegroup(**kwargs)`). Mirrors the BUILD-global builtin.
    fn filegroup<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let files: Vec<String> =
            crate::values::unpack(srcs).iter().map(|s| crate::state::qualify(sess, s)).collect();
        crate::deps::record_target(sess, crate::state::AnalyzedTarget {
            name: crate::state::canon_label(sess, &name),
            default_info: files,
            ..Default::default()
        });
        Ok(NoneType)
    }
    /// `native.label_flag` / `native.label_setting` — build-setting label flags (declare-only;
    /// build settings are registered debt).
    fn label_flag<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        crate::deps::record_target(sess, crate::state::AnalyzedTarget {
            name: crate::state::canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }
    fn label_setting<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        crate::deps::record_target(sess, crate::state::AnalyzedTarget {
            name: crate::state::canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }
    /// `native.package_group` / `native.config_setting` / `native.exports_files` /
    /// `native.existing_rule(s)` — the BUILD declare-time surface macros reach for.
    fn package_group<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn exports_files<'v>(
        #[starlark(args)] _a: starlark::values::tuple::UnpackTuple<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn existing_rule<'v>(
        #[starlark(require = pos)] name: String,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let sess = session(eval);
        let canon = crate::state::canon_label(sess, &name);
        let known = sess.results.borrow().contains_key(&canon)
            || sess.pending.borrow().contains_key(&canon);
        // Loading-grade: existence signal only (None ⇒ absent; a dict ⇒ present).
        if known {
            Ok(eval.heap().alloc(starlark::values::dict::AllocDict::EMPTY))
        } else {
            Ok(Value::new_none())
        }
    }
    fn existing_rules<'v>(
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(eval.heap().alloc(starlark::values::dict::AllocDict::EMPTY))
    }
    fn config_setting<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] values: Option<SmallMap<String, String>>,
        #[starlark(require = named)] define_values: Option<SmallMap<String, String>>,
        #[starlark(require = named)] flag_values: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let spec = crate::state::ConfigSpec {
            values: values.map(|m| m.into_iter().collect()).unwrap_or_default(),
            define_values: define_values.map(|m| m.into_iter().collect()).unwrap_or_default(),
            group: None,
            unmodeled: flag_values.is_some_and(|v| !v.is_none()),
        };
        sess.config_specs
            .borrow_mut()
            .insert(crate::state::canon_label(sess, &name), spec);
        crate::deps::record_target(sess, crate::state::AnalyzedTarget {
            name: crate::state::canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }
    /// `native.alias(name, actual)` — records a target whose files are the actual's (resolved at
    /// analysis via the dep machinery is Phase-later; loading-grade: name declared).
    fn alias<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] actual: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let actual = match actual {
            Some(v) => {
                let r = crate::dialect::resolve_attr_value(eval, v)?;
                r.unpack_str().map(String::from)
            }
            None => None,
        };
        let sess = session(eval);
        if let Some(a) = actual {
            let canon_actual = crate::state::canon_label(sess, &a);
            sess.aliases
                .borrow_mut()
                .insert(crate::state::canon_label(sess, &name), canon_actual);
        }
        crate::deps::record_target(sess, crate::state::AnalyzedTarget {
            name: crate::state::canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }
}


/// Bazel's `config.*` build-setting constructors (D4 upstream compat). Stubs: razel doesn't model
/// build settings yet, but real rules call `config.int(...)`/etc. at load (skylib `common_settings`).
#[starlark::starlark_module]
pub(crate) fn config_members(b: &mut GlobalsBuilder) {
    fn int<'v>(#[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn bool<'v>(#[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn string<'v>(#[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn string_list<'v>(#[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    /// `config.exec(exec_group=?)` / `config.target()` / `config.none()` — attr `cfg=` transition
    /// constructors (absorbed; configurations/transitions are not yet applied).
    fn exec<'v>(#[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn target<'v>(#[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn none<'v>(#[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
}

/// Bazel builtin-namespace stubs (D4 upstream compat): `config_common` / `cc_common` /
/// `coverage_common` / `testing`. Real rules reference these globals; razel resolves them so the
/// `.bzl` load. Members are deferred to when rules actually run (not yet exercised).
#[starlark::starlark_module]
pub(crate) fn config_common_members(b: &mut GlobalsBuilder) {
    /// `config_common.toolchain_type(type, mandatory=?)` — an optional-toolchain dependency
    /// declaration (absorbed; toolchain resolution is L3).
    fn toolchain_type<'v>(
        #[starlark(require = pos)] _t: Value<'v>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
}
/// An ABSORBING host-namespace value (`cc_common`, `apple_common`, …): any member access, call,
/// or index resolves to another absorber, so real `.bzl` LOAD regardless of which host-internal
/// member they touch at module level. The absorption surfaces at ANALYSIS time (an absorbed value
/// used as a real input fails with a clear `<host-absorbed>` in the traceback) — registered debt;
/// members gain real semantics as corpora demand them.
#[derive(Debug, ProvidesStaticType, NoSerialize, allocative::Allocative)]
#[allow(dead_code)]
pub(crate) struct Absorb;
starlark::starlark_simple_value!(Absorb);

impl std::fmt::Display for Absorb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<host-absorbed>")
    }
}

#[starlark::values::starlark_value(type = "host_absorbed")]
impl<'v> starlark::values::StarlarkValue<'v> for Absorb {
    fn get_attr(
        &self,
        _attr: &str,
        heap: starlark::values::Heap<'v>,
    ) -> Option<Value<'v>> {
        Some(heap.alloc(Absorb))
    }
    fn invoke(
        &self,
        _me: Value<'v>,
        _args: &starlark::eval::Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        Ok(eval.heap().alloc(Absorb))
    }
    fn at(
        &self,
        _index: Value<'v>,
        heap: starlark::values::Heap<'v>,
    ) -> starlark::Result<Value<'v>> {
        Ok(heap.alloc(Absorb))
    }
}

/// The Bazel `attr.*` namespace: declares a rule's attribute schema. razel's `rule()`
/// resolves attribute *values* directly from the call kwargs (it doesn't enforce the
/// schema), so each `attr.<kind>(...)` is a placeholder descriptor — present so rule
/// definitions evaluate.
#[starlark::starlark_module]
pub(crate) fn attr_members(b: &mut GlobalsBuilder) {
    // D1: each `attr.<kind>(...)` returns a DESCRIPTOR struct carrying its kind + `default` +
    // `mandatory` (was a discarded `None`). `rule()` stores the schema; `invoke` consults it (defaults,
    // mandatory, coercion). The descriptor is a plain struct so it freezes with the rule + reads back
    // via `get_attr` in the loader.
    fn string<'v>(#[starlark(kwargs)] kw: SmallMap<String, Value<'v>>, eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<Value<'v>> {
        Ok(attr_descriptor("string", &kw, eval))
    }
    fn int<'v>(#[starlark(kwargs)] kw: SmallMap<String, Value<'v>>, eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<Value<'v>> {
        Ok(attr_descriptor("int", &kw, eval))
    }
    fn bool<'v>(#[starlark(kwargs)] kw: SmallMap<String, Value<'v>>, eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<Value<'v>> {
        Ok(attr_descriptor("bool", &kw, eval))
    }
    fn label<'v>(#[starlark(kwargs)] kw: SmallMap<String, Value<'v>>, eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<Value<'v>> {
        Ok(attr_descriptor("label", &kw, eval))
    }
    fn label_list<'v>(#[starlark(kwargs)] kw: SmallMap<String, Value<'v>>, eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<Value<'v>> {
        Ok(attr_descriptor("label_list", &kw, eval))
    }
    fn label_keyed_string_dict<'v>(#[starlark(kwargs)] kw: SmallMap<String, Value<'v>>, eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<Value<'v>> {
        Ok(attr_descriptor("label_keyed_string_dict", &kw, eval))
    }
    fn string_list<'v>(#[starlark(kwargs)] kw: SmallMap<String, Value<'v>>, eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<Value<'v>> {
        Ok(attr_descriptor("string_list", &kw, eval))
    }
    fn string_dict<'v>(#[starlark(kwargs)] kw: SmallMap<String, Value<'v>>, eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<Value<'v>> {
        Ok(attr_descriptor("string_dict", &kw, eval))
    }
    fn string_list_dict<'v>(#[starlark(kwargs)] kw: SmallMap<String, Value<'v>>, eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<Value<'v>> {
        Ok(attr_descriptor("string_list_dict", &kw, eval))
    }
    fn output<'v>(#[starlark(kwargs)] kw: SmallMap<String, Value<'v>>, eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<Value<'v>> {
        Ok(attr_descriptor("output", &kw, eval))
    }
    fn output_list<'v>(#[starlark(kwargs)] kw: SmallMap<String, Value<'v>>, eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<Value<'v>> {
        Ok(attr_descriptor("output_list", &kw, eval))
    }
    fn int_list<'v>(#[starlark(kwargs)] kw: SmallMap<String, Value<'v>>, eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<Value<'v>> {
        Ok(attr_descriptor("int_list", &kw, eval))
    }
    fn string_keyed_label_dict<'v>(#[starlark(kwargs)] kw: SmallMap<String, Value<'v>>, eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<Value<'v>> {
        Ok(attr_descriptor("string_keyed_label_dict", &kw, eval))
    }
}

/// Build an attribute descriptor (D1): a struct `{kind, default, mandatory}` the loader reads when
/// instantiating a rule. `default` is `None` when unset (distinct from a `None` default value, which
/// rules don't use); `mandatory` defaults to false.
fn attr_descriptor<'v>(
    kind: &str,
    kw: &SmallMap<String, Value<'v>>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> Value<'v> {
    let heap = eval.heap();
    let default = kw.get("default").copied().unwrap_or_else(Value::new_none);
    let mandatory = kw.get("mandatory").and_then(|v| v.unpack_bool()).unwrap_or(false);
    heap.alloc(starlark::values::structs::AllocStruct([
        ("kind".to_string(), heap.alloc(kind)),
        ("default".to_string(), default),
        ("mandatory".to_string(), heap.alloc(mandatory)),
    ]))
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
        // C3b: the toolchain name resolves via the toolchain registry — the engine no longer names a
        // language. (Adding a command-line-shaped toolchain is a row in `toolchains`, not here.)
        let config = crate::toolchains::resolve_toolchain(toolchain).map_err(|e| anyhow::anyhow!(e))?;
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

    /// `razel_build.action(executable, arguments, inputs, outputs, mnemonic)` → register an action on
    /// the current target — the four-move API's ACTION move (C1b). Identical to `ctx.actions.run`
    /// (both call `values::push_run_action`), but the engine's named surface: razel's bundled `.bzl`
    /// register through `razel_build`, so cc + java ride one engine surface.
    fn action<'v>(
        #[starlark(require = named)] executable: Option<Value<'v>>,
        #[starlark(require = named)] outputs: Option<UnpackList<Value<'v>>>,
        #[starlark(require = named)] inputs: Option<UnpackList<Value<'v>>>,
        #[starlark(require = named)] arguments: Option<UnpackList<Value<'v>>>,
        #[starlark(require = named)] mnemonic: Option<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        crate::values::push_run_action(eval, executable, outputs, inputs, arguments, mnemonic);
        Ok(NoneType)
    }

    /// `razel_build.info(provider, fields)` — the generic provider constructor (C3/info). Captures a
    /// provider onto the current target: each field's value is wrapped per the registry's `FieldKind`
    /// (a list → `Set`/`OrderedDepset`, a bool → `Scalar`) and written to the provider map. Replaces
    /// the hardcoded `CcInfo`/`JavaInfo` capture builtins — the engine no longer names a language to
    /// capture providers; the schema is the registry's.
    fn info<'v>(
        #[starlark(require = pos)] provider: String,
        #[starlark(require = pos)] fields: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        use razel_dds::{FieldId, FieldKind, FieldValue, ProviderTypeId, Scalar};
        let registry = crate::registry::builtin_registry();
        let ty = ProviderTypeId::new(provider.as_str());
        let strs = |v: Value<'v>| -> Vec<Scalar> {
            crate::values::extract_files(v).into_iter().map(Scalar::Str).collect()
        };
        let mut captured: Vec<(String, FieldValue)> = Vec::new();
        for (field, value) in &fields {
            let fv = match registry.kind(&ty, &FieldId::new(field.as_str())) {
                Some(FieldKind::Set) => FieldValue::Set(strs(*value).into_iter().collect()),
                Some(FieldKind::OrderedDepset) => FieldValue::OrderedDepset(strs(*value)),
                Some(FieldKind::Scalar) => {
                    FieldValue::Scalar(Scalar::Bool(value.unpack_bool().unwrap_or(false)))
                }
                None => continue, // not in the provider's schema — ignore (forward-compatible)
            };
            captured.push((field.clone(), fv));
        }
        crate::state::with_current(crate::state::session(eval), |c| {
            for (f, fv) in captured {
                c.set_provider(&provider, &f, fv);
            }
        });
        Ok(NoneType)
    }
}

