//! Ruleset shim modules: the synthetic @rules_cc/@rules_java/skylib/auto_config modules. C0.

use crate::state::{CcToolchainMode, session};
use crate::native_cc::cc_rules;
use crate::engine::razel_build_members;
use crate::deps::record_named;
use crate::dialect::rule_globals;
use starlark::collections::SmallMap;
use starlark::environment::{
    FrozenModule, GlobalsBuilder, Module,
};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};
use starlark::values::none::NoneType;
use starlark::values::{
    Value,
};



/// The synthetic `@rules_cc` module, by toolchain mode (§7). **Native** re-exports razel's native
/// rules (host compiler, executable — razel-build's path). **AdoptBazel** serves razel's `cc:defs.bzl`
/// over the engine (Bazel-faithful declared graph — the parity runner's path).
pub(crate) fn rules_cc_module(mode: CcToolchainMode) -> Result<FrozenModule, String> {
    match mode {
        CcToolchainMode::Native => rules_cc_module_native(),
        CcToolchainMode::AdoptBazel => rules_cc_module_adopt_bazel(),
    }
}


/// Native: `cc_binary`/`cc_library` are razel's native rules (executable, host compiler).
pub(crate) fn rules_cc_module_native() -> Result<FrozenModule, String> {
    let globals = GlobalsBuilder::standard().with(cc_rules).with(crate::dialect::rule_globals).build();
    Module::with_temp_heap(|module| {
        let ast = AstModule::parse(
            "@rules_cc",
            "cc_binary = native_cc_binary\ncc_library = native_cc_library\n\
             def _cc_import_impl(ctx):\n    return [DefaultInfo(files = [])]\n\
             cc_import = rule(implementation = _cc_import_impl, attrs = {})\n"
                .to_owned(),
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
pub(crate) fn rules_cc_module_adopt_bazel() -> Result<FrozenModule, String> {
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
pub(crate) fn rules_java_module() -> Result<FrozenModule, String> {
    // the razel_build namespace so java_defs.bzl can use razel_build.action (C1b — engine surface).
    let globals = GlobalsBuilder::standard()
        .with(rule_globals)
        .with(|b| {
            b.namespace("razel_build", razel_build_members);
        })
        .build();
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


/// `@bazel_skylib` rules razel recognizes as no-op/minimal targets: `bzl_library`,
/// the build/diff test wrappers, the `common_settings` build-setting flags, and the
/// small codegen rules. razel enforces no build settings and tracks no .bzl
/// libraries, so these are analysis-visible placeholders (they build to nothing).
/// skylib's *lib* helpers (`selects`/`paths`/`sets`) are pure-Starlark namespaces —
/// handled separately as TF reaches them.
#[starlark::starlark_module]
pub(crate) fn skylib_rules(b: &mut GlobalsBuilder) {
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
pub(crate) fn rules_skylib_module() -> Result<FrozenModule, String> {
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
pub(crate) fn auto_config_fns(b: &mut GlobalsBuilder) {
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
pub(crate) fn auto_config_module(reexport: &str) -> Result<FrozenModule, String> {
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

