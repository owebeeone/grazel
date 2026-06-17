//! `rules::globals` — split from `rules.rs` (facade in `mod.rs`).

use crate::dialect::rule_globals;
use crate::engine::{
    attr_members, config_common_members, config_members, native_members, razel_build_members,
};
use crate::state::{AnalyzedTarget, Session};
use starlark::environment::{Globals, GlobalsBuilder, LibraryExtension, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};
use starlark::values::Value;
use std::cell::RefCell;
use super::*;

pub(crate) fn build_globals() -> Globals {
    builder_base().build()
}


pub(crate) fn builder_base() -> GlobalsBuilder {
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
    .with(engine_namespaces)
    .with(bazel_native_rule_globals)
    .with(crate::fetch::repo_rule_globals)
    .with(autoload_stub_globals)
}


/// Bazel-AUTOLOADED BUILD globals razel models as RECORD-ONLY placeholders (fetch R4):
/// upstream BUILDs (flatbuffers et al.) declare java targets bare; nothing in the TF cone
/// consumes them, but the package must LOAD. The target exists (deps resolve to an empty
/// placeholder); real java analysis is the java rung's work, via the @rules_java shim.
#[starlark::starlark_module]
pub(crate) fn autoload_stub_globals(b: &mut GlobalsBuilder) {
    fn java_library<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: starlark::collections::SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        crate::deps::record_named(crate::state::session(eval), &name);
        Ok(starlark::values::none::NoneType)
    }

    fn java_binary<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: starlark::collections::SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        crate::deps::record_named(crate::state::session(eval), &name);
        Ok(starlark::values::none::NoneType)
    }

    fn java_test<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: starlark::collections::SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        crate::deps::record_named(crate::state::session(eval), &name);
        Ok(starlark::values::none::NoneType)
    }

    /// Bazel's `fail(*args, msg=None, attr=None, sep=" ")` — the deprecated `attr` keyword
    /// prefixes the message with the attribute name; starlark-rust's builtin rejects it.
    /// SHADOWS the stdlib global (GlobalsBuilder is a map; later sets win — the no-fork
    /// dialect lever, round 44).
    fn fail<'v>(
        #[starlark(args)] args: starlark::values::tuple::UnpackTuple<Value<'v>>,
        #[starlark(require = named)] msg: Option<Value<'v>>,
        #[starlark(require = named)] attr: Option<String>,
        #[starlark(require = named)] sep: Option<String>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        let sep = sep.unwrap_or_else(|| " ".to_string());
        let mut parts: Vec<String> = Vec::new();
        if let Some(m) = msg {
            parts.push(
                m.unpack_str()
                    .map(String::from)
                    .unwrap_or_else(|| m.to_string()),
            );
        }
        parts.extend(args.items.iter().map(|a| {
            a.unpack_str()
                .map(String::from)
                .unwrap_or_else(|| a.to_string())
        }));
        let body = parts.join(&sep);
        match attr {
            Some(a) => Err(anyhow::anyhow!("fail: attribute {a}: {body}")),
            None => Err(anyhow::anyhow!("fail: {body}")),
        }
    }

    /// `toolchain()` declarations (round 43 — grpc registers clang-cl toolchains):
    /// record-only; real toolchain resolution is L3 surface.
    fn toolchain<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: starlark::collections::SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        crate::deps::record_named(crate::state::session(eval), &name);
        Ok(starlark::values::none::NoneType)
    }

    /// The platform-family DECLARE rules (round 43 — grpc declares clang-cl platforms in
    /// its BUILD): record-only placeholders; real platform resolution is L4 surface.
    fn platform<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: starlark::collections::SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        crate::deps::record_named(crate::state::session(eval), &name);
        Ok(starlark::values::none::NoneType)
    }

    fn constraint_setting<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: starlark::collections::SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        let canon = {
            let sess = crate::state::session(eval);
            crate::deps::record_named(sess, &name);
            crate::state::canon_label(sess, &name)
        };
        // P5.2 (§11.2): a query node too — q4 traversal reaches constraint nodes by their kind.
        crate::loaded::capture_rule(eval, &canon, "constraint_setting", &[], &_kw);
        Ok(starlark::values::none::NoneType)
    }

    /// A `constraint_value` IS a selectable condition: it registers a config spec whose
    /// single constraint is ITSELF — `spec.matches` answers via `host_constraint_matches`
    /// (TF selects on `@platforms//os:*` directly; round 43).
    fn constraint_value<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: starlark::collections::SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        let canon = {
            let sess = crate::state::session(eval);
            crate::deps::record_named(sess, &name);
            let canon = crate::state::canon_label(sess, &name);
            let spec = crate::state::ConfigSpec {
                constraint_values: vec![canon.clone()],
                ..Default::default()
            };
            sess.config_specs.borrow_mut().insert(canon.clone(), spec);
            canon
        };
        // P5.2 (§11.2): a query node too — so `deps()` reaching it (e.g. `@platforms//:incompatible`
        // as a `target_compatible_with` default-arm VALUE) classifies it by its `constraint_value`
        // rule_class instead of dropping the edge as unresolved.
        crate::loaded::capture_rule(eval, &canon, "constraint_value", &[], &_kw);
        Ok(starlark::values::none::NoneType)
    }
}


/// Evaluate a WORKSPACE source (fetch R1, RazelFetchPlan §3): the normal BUILD surface plus
/// the WORKSPACE-only globals (the `repository_rule` recorder, `workspace()`,
/// `register_*` no-ops). Declarations record but never drive; no freeze/harvest — the
/// Session's `repo_specs` are the product.
pub(crate) fn eval_workspace_src(session: &Session, name: &str, src: &str) -> Result<(), String> {
    let rulesets = ruleset_modules(session.global.cc_toolchain)?;
    let globals = builder_base().with(crate::fetch::workspace_globals).build();
    let loader = BzlLoader {
        rulesets: &rulesets,
        globals: &globals,
        session,
        load_ctx: RefCell::new(vec![None]),
    };
    session.bzl_repo_push(None);
    let res = Module::with_temp_heap(|module| -> Result<(), String> {
        crate::dialect::install_decl_store(&module);
        let ast = AstModule::parse(name, detab_leading(src).into_owned(), &Dialect::Extended)
            .map_err(|e| format!("{e}"))?;
        let mut eval = Evaluator::new(&module);
        eval.set_loader(&loader);
        eval.extra = Some(session);
        eval.eval_module(ast, &globals)
            .map_err(|e| format!("{e}"))?;
        Ok(())
    });
    session.bzl_repo_pop();
    res
}


/// Bazel's native rules as BUILD GLOBALS (no `load()` needed — `cc_library` is a builtin in real
/// BUILD files; TF uses it bare everywhere). Aliased from razel's native cc rules; `cc_test` is
/// loading-grade (the binary backend).
pub(crate) fn bazel_native_rule_globals(b: &mut GlobalsBuilder) {
    let native_cc = GlobalsBuilder::standard()
        .with(crate::native_cc::cc_rules)
        .build();
    let get = |name: &str| {
        native_cc
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v)
            .expect("native cc rule registered")
    };
    b.set("cc_library", get("native_cc_library"));
    b.set("cc_binary", get("native_cc_binary"));
    b.set("cc_test", get("native_cc_binary"));
    b.set("cc_libc_top_alias", get("native_cc_libc_top_alias"));
    let native_py = GlobalsBuilder::standard()
        .with(crate::py_rules::py_rules)
        .build();
    let getp = |name: &str| {
        native_py
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v)
            .expect("native py rule registered")
    };
    b.set("py_library", getp("native_py_library"));
    b.set("py_binary", getp("native_py_binary"));
    b.set("py_test", getp("native_py_test"));
}


/// The engine's `.bzl`-facing namespaces — razel's own (`native`/`attr`/`razel_build`) plus the Bazel
/// builtin-namespace stubs (D4) that let real upstream `.bzl` resolve. Shared by both globals builders
/// (workspace + inline) so the surface is identical in every analysis path.
pub(crate) fn engine_namespaces(b: &mut GlobalsBuilder) {
    b.namespace("native", |nb| {
        native_members(nb);
        // skylib `versions.check` consults this (fetch R1's WORKSPACE chain does too).
        // Bazel-7-era claim, consistent with the all-True @bazel_features posture.
        nb.set("bazel_version", "7.4.5");
        // WORKSPACE-macro surface (fetch R1): registration no-ops, namespaced form.
        let ws_g = GlobalsBuilder::standard()
            .with(crate::fetch::workspace_globals)
            .build();
        for name in [
            "register_toolchains",
            "register_execution_platforms",
            "bind",
        ] {
            if let Some((_, v)) = ws_g.iter().find(|(n, _)| *n == name) {
                nb.set(name, v);
            }
        }
        // Platform-family declare rules (round 43) — BUILD globals AND native.* forms.
        let stub_g = GlobalsBuilder::standard()
            .with(autoload_stub_globals)
            .build();
        for name in [
            "platform",
            "constraint_setting",
            "constraint_value",
            "toolchain",
            "java_library",
        ] {
            if let Some((_, v)) = stub_g.iter().find(|(n, _)| *n == name) {
                nb.set(name, v);
            }
        }
        // Real macros wrap the BUILD-global builtins via `native.X` — alias them in wholesale
        // (the BUILD globals and `native.*` are the same functions in Bazel).
        let dialect_g = GlobalsBuilder::standard().with(rule_globals).build();
        for name in [
            "genrule",
            "test_suite",
            "config_setting",
            "exports_files",
            "package_group",
            "licenses",
            "razel_config_setting_group",
        ] {
            if let Some((_, v)) = dialect_g.iter().find(|(n, _)| *n == name) {
                nb.set(name, v);
            }
        }
        let cc = GlobalsBuilder::standard()
            .with(crate::native_cc::cc_rules)
            .build();
        for (alias, src) in [
            ("cc_library", "native_cc_library"),
            ("cc_binary", "native_cc_binary"),
            ("cc_test", "native_cc_binary"),
        ] {
            if let Some((_, v)) = cc.iter().find(|(n, _)| *n == src) {
                nb.set(alias, v);
            }
        }
    });
    b.namespace("attr", attr_members);
    b.namespace("razel_build", razel_build_members);
    b.namespace("config", config_members);
    b.namespace("config_common", |b| {
        config_common_members(b);
        // Provider-shaped constants rules reference (feature flags absorb).
        b.set("FeatureFlagInfo", crate::engine::Absorb);
        b.set("config_feature_flag_transition", crate::engine::Absorb);
    });
    // Foreign host namespaces ABSORB (any member resolves; surfaces only at analysis use —
    // registered debt). config/attr/native/razel_build stay explicit + typed.
    for ns in [
        "cc_common",
        "coverage_common",
        "testing",
        "apple_common",
        "java_common",
        "proto_common",
        "platform_common",
        "proto_common_do_not_use",
        "py_internal",
        "android_common",
        "ApkInfo",
        "AndroidIdeInfo",
    ] {
        b.set(ns, crate::engine::Absorb);
    }
    // The absorber itself, for razel's HOST .bzl files (host-repos/) to bind symbols with.
    b.set("razel_host_absorb", crate::engine::Absorb);
    razel_host_helpers(b);
}


/// Host-.bzl helper globals: `razel_host_absorb_with({...})` builds an absorber whose NAMED
/// members are real values (the per-member override seam).
#[starlark::starlark_module]
pub(crate) fn razel_host_helpers(b: &mut GlobalsBuilder) {
    fn razel_host_absorb_with<'v>(
        #[starlark(require = pos)] overrides: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let d = starlark::values::dict::DictRef::from_value(overrides)
            .ok_or_else(|| anyhow::anyhow!("razel_host_absorb_with takes a dict"))?;
        let overrides: Vec<(String, Value<'v>)> = d
            .iter()
            .map(|(k, v)| {
                k.unpack_str()
                    .map(|k| (k.to_string(), v))
                    .ok_or_else(|| anyhow::anyhow!("override keys are strings"))
            })
            .collect::<Result<_, _>>()?;
        Ok(eval.heap().alloc(crate::engine::AbsorbWith { overrides }))
    }
}


/// Evaluate a `BUILD`/`.bzl` that defines and instantiates Starlark rules, running each
/// rule impl (same-scope analysis); returns the analyzed targets.
pub fn analyze_starlark(name: &str, src: &str) -> Result<Vec<AnalyzedTarget>, String> {
    let session = Session::default();
    let ast = AstModule::parse(name, detab_leading(src).into_owned(), &Dialect::Extended)
        .map_err(|e| format!("{e}"))?;
    // ONE globals surface everywhere (round 44: a private duplicate here predated
    // builder_base and silently missed later dialect additions — the shadowed fail()).
    let globals = build_globals();
    let res: Result<(), String> = Module::with_temp_heap(|module| {
        crate::dialect::install_decl_store(&module);
        {
            let mut eval = Evaluator::new(&module);
            eval.extra = Some(&session);
            eval.eval_module(ast, &globals)
                .map_err(|e| format!("{e}"))?;
        }
        // E0 phase 2: analyze the recorded declarations, demand-driven (forward refs resolve).
        {
            let mut eval = Evaluator::new(&module);
            eval.extra = Some(&session);
            crate::dialect::drive_decls(&mut eval, true).map_err(|e| format!("{e}"))?;
        }
        crate::dialect::stash_captured_for_freeze(&module, &session).map_err(|e| format!("{e}"))?;
        let fm = module.freeze().map_err(|e| format!("freeze: {e:?}"))?;
        if let Ok(owned) = fm.get(crate::dialect::CAPTURED_VAR) {
            index_harvest(&owned, &session.cross_captured, &session.cross_index);
        }
        Ok(())
    });
    res?;
    Ok(session.take_targets())
}


