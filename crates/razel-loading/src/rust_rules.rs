//! @rules_rust → razel native rules: rust_library / rust_binary (rustc).
//!
//! `load("@rules_rust//rust:defs.bzl", "rust_binary"|"rust_library")` resolves to
//! these. Like `rules::cc_rules`, razel provides the rules *natively* (one `rustc`
//! action per target) instead of executing rules_rust's Starlark.
//!
//! A `rust_library` compiles its crate root (`srcs[0]`) to `lib<name>.rlib` and
//! exports it via `default_info`; a dependent reads that rlib through
//! `resolve_dep().libs` and wires it in as `--extern <crate>=<rlib>`. The dep crate
//! name is the dep's canonical-label target segment (`//lib:greet` → `greet`), so
//! the consumer can `use greet::...`. Paths are workspace-root-relative (exec_root =
//! workspace root), matching how cc uses `-iquote .`.

use crate::state::{AnalyzedAction, AnalyzedTarget, canon_label, native_decl, qualify, session};
use crate::deps::{record_target, resolve_dep};
use crate::values::{unpack, unpack_strs};
use starlark::collections::SmallMap;
use starlark::environment::{FrozenModule, GlobalsBuilder, LibraryExtension, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};
use starlark::values::Value;
use starlark::values::list::UnpackList;
use starlark::values::none::NoneType;

const RUSTC: &str = "/usr/bin/rustc";

/// The `rustc` to invoke, resolved to an **absolute** path. Prefer the fixed
/// `/usr/bin/rustc` (matching cc's pinned toolchain paths); when absent — e.g. a
/// rustup install under `~/.cargo/bin` — scan `PATH` for it. The executor runs
/// actions with a cleared env (no `PATH`), so a bare name wouldn't resolve; an
/// absolute path also lets rustc locate its sysroot relative to its own binary.
/// Falls back to the bare name if nothing is found (the test guards on rustc).
fn rustc() -> String {
    if std::path::Path::new(RUSTC).exists() {
        return RUSTC.into();
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            let cand = std::path::Path::new(dir).join("rustc");
            if cand.exists() {
                return cand.to_string_lossy().into_owned();
            }
        }
    }
    "rustc".into()
}

/// The crate name a dependent uses for `--extern` / `use`: the target segment of a
/// canonical label (`//lib:greet` → `greet`, bare `greet` → `greet`).
fn crate_name_of(canon: &str) -> String {
    canon
        .rsplit_once(':')
        .map(|(_, n)| n)
        .unwrap_or(canon)
        .to_string()
}

/// Resolve `deps` to `(--extern crate=rlib args, dep rlib inputs, dep canon names)`.
fn extern_args(
    eval: &mut Evaluator<'_, '_, '_>,
    deps: Vec<String>,
) -> anyhow::Result<(Vec<String>, Vec<String>, Vec<String>)> {
    let (mut args, mut inputs, mut names) = (Vec::new(), Vec::new(), Vec::new());
    for d in &deps {
        let dep = resolve_dep(eval, d)?;
        let crate_name = crate_name_of(&dep.canon);
        // A rust_library exports exactly one rlib in default_info → dep.libs.
        for rlib in &dep.libs {
            args.push("--extern".into());
            args.push(format!("{crate_name}={rlib}"));
            inputs.push(rlib.clone());
        }
        names.push(dep.canon);
    }
    Ok((args, inputs, names))
}

#[starlark::starlark_module]
fn rust_rules(b: &mut GlobalsBuilder) {
    /// `rust_library(name, srcs, deps=[], edition="2021")` → one `rustc` action
    /// compiling `srcs[0]` to `lib<name>.rlib`, exported to dependents.
    fn native_rust_library<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<Value<'v>>,
        #[starlark(require = named)] deps: Option<Value<'v>>,
        #[starlark(require = named)] edition: Option<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        // E0c: record now, analyze in the demand-driven pass (forward refs resolve).
        let label = canon_label(session(eval), &name);
        crate::loaded::capture_rule(eval, &label, "rust_library", &[("srcs", srcs), ("deps", deps)], &_kw);
        let srcs = crate::values::str_attr_parts(eval, srcs)?;
        let deps = crate::values::str_attr_parts(eval, deps)?;
        crate::dialect::record_native(eval, label, native_decl(move |eval| {
        let srcs = crate::values::resolve_str_parts(eval, &srcs)?;
        let deps = crate::values::resolve_str_parts(eval, &deps)?;
        let sess = session(eval);
        let srcs: Vec<String> = srcs.iter().map(|s| qualify(sess, s)).collect();
        let crate_root = srcs
            .first()
            .ok_or_else(|| anyhow::anyhow!("rust_library `{name}` needs at least one src"))?
            .clone();
        let edition = edition.unwrap_or_else(|| "2021".into());
        let (extern_flags, dep_rlibs, dep_names) = extern_args(eval, deps.clone())?;
        let sess = session(eval);

        let rlib = qualify(sess, &format!("lib{name}.rlib"));
        let mut argv = vec![
            rustc(),
            "--edition".into(),
            edition,
            "--crate-type".into(),
            "lib".into(),
            "--crate-name".into(),
            name.clone(),
            crate_root,
            "-o".into(),
            rlib.clone(),
        ];
        argv.extend(extern_flags);

        let mut inputs = srcs;
        inputs.extend(dep_rlibs);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: dep_names,
            actions: vec![AnalyzedAction {
                mnemonic: "Rustc".into(),
                argv,
                inputs,
                outputs: vec![rlib.clone()],
            }],
            default_info: vec![rlib],
            providers: Default::default(),
        });
        Ok(())
        }))?;
        Ok(NoneType)
    }

    /// `rust_binary(name, srcs, deps=[], edition="2021")` → one `rustc` action
    /// compiling `srcs[0]` to the `<name>` executable, linking dep rlibs.
    fn native_rust_binary<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<Value<'v>>,
        #[starlark(require = named)] deps: Option<Value<'v>>,
        #[starlark(require = named)] edition: Option<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        // E0c: record now, analyze in the demand-driven pass (forward refs resolve).
        let label = canon_label(session(eval), &name);
        crate::loaded::capture_rule(eval, &label, "rust_binary", &[("srcs", srcs), ("deps", deps)], &_kw);
        let srcs = crate::values::str_attr_parts(eval, srcs)?;
        let deps = crate::values::str_attr_parts(eval, deps)?;
        crate::dialect::record_native(eval, label, native_decl(move |eval| {
        let srcs = crate::values::resolve_str_parts(eval, &srcs)?;
        let deps = crate::values::resolve_str_parts(eval, &deps)?;
        let sess = session(eval);
        let srcs: Vec<String> = srcs.iter().map(|s| qualify(sess, s)).collect();
        let crate_root = srcs
            .first()
            .ok_or_else(|| anyhow::anyhow!("rust_binary `{name}` needs at least one src"))?
            .clone();
        let edition = edition.unwrap_or_else(|| "2021".into());
        let (extern_flags, dep_rlibs, dep_names) = extern_args(eval, deps.clone())?;
        let sess = session(eval);

        let out = qualify(sess, &name);
        let mut argv = vec![
            rustc(),
            "--edition".into(),
            edition,
            "--crate-name".into(),
            name.clone(),
            crate_root,
            "-o".into(),
            out.clone(),
        ];
        argv.extend(extern_flags);

        let mut inputs = srcs;
        inputs.extend(dep_rlibs);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: dep_names,
            actions: vec![AnalyzedAction {
                mnemonic: "Rustc".into(),
                argv,
                inputs,
                outputs: vec![out.clone()],
            }],
            default_info: vec![out],
            providers: Default::default(),
        });
        Ok(())
        }))?;
        Ok(NoneType)
    }

    /// `rust_shared_library(name, srcs, deps=[], edition=…)` → one rustc cdylib
    /// action producing `lib<name>.dylib` (host posture: macOS suffix).
    fn native_rust_shared_library<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<Value<'v>>>,
        #[starlark(require = named)] deps: Option<UnpackList<Value<'v>>>,
        #[starlark(require = named)] edition: Option<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let label = canon_label(session(eval), &name);
        let (srcs, deps) = (unpack_strs(srcs), unpack_strs(deps));
        crate::dialect::record_native(eval, label, native_decl(move |eval| {
        let sess = session(eval);
        let srcs: Vec<String> = srcs.iter().map(|s| qualify(sess, s)).collect();
        let crate_root = srcs
            .first()
            .ok_or_else(|| anyhow::anyhow!("rust_shared_library `{name}` needs at least one src"))?
            .clone();
        let edition = edition.unwrap_or_else(|| "2021".into());
        let (extern_flags, dep_rlibs, dep_names) = extern_args(eval, deps.clone())?;
        let sess = session(eval);
        let dylib = qualify(sess, &format!("lib{name}.dylib"));
        let mut argv = vec![
            rustc(),
            "--edition".into(),
            edition,
            "--crate-type".into(),
            "cdylib".into(),
            "--crate-name".into(),
            name.clone(),
            crate_root,
            "-o".into(),
            dylib.clone(),
        ];
        argv.extend(extern_flags);
        let mut inputs = srcs;
        inputs.extend(dep_rlibs);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: dep_names,
            actions: vec![AnalyzedAction {
                mnemonic: "Rustc".into(),
                argv,
                inputs,
                outputs: vec![dylib.clone()],
            }],
            default_info: vec![dylib],
            providers: Default::default(),
        });
        Ok(())
        }))?;
        Ok(NoneType)
    }

    /// `rust_library_group(name, deps)` — faithful GROUPING rule: no actions,
    /// DefaultInfo = the deps' rlibs (rules_rust's lib-collection shape).
    fn native_rust_library_group<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] deps: Option<UnpackList<Value<'v>>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let label = canon_label(session(eval), &name);
        let deps = unpack_strs(deps);
        crate::dialect::record_native(eval, label, native_decl(move |eval| {
        let (_, dep_rlibs, dep_names) = extern_args(eval, deps.clone())?;
        let sess = session(eval);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: dep_names,
            actions: Vec::new(),
            default_info: dep_rlibs,
            providers: Default::default(),
        });
        Ok(())
        }))?;
        Ok(NoneType)
    }

    /// `rust_doc(name, crate, …)` — ANALYSIS-ONLY today (named hole: the rustdoc
    /// action lands when a consumer demands the docs, not just the load).
    fn native_rust_doc<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: Vec::new(),
            actions: Vec::new(),
            default_info: Vec::new(),
            providers: Default::default(),
        });
        Ok(NoneType)
    }

    /// P3.1: `cargo_build_script` load surface — a STUB target for now (records it so the package
    /// loads and the `:build_script_build` alias/deps resolve); the compile + run pipeline + flags
    /// file land in P3.6/P3.7.
    fn native_cargo_build_script<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let label = canon_label(session(eval), &name);
        crate::dialect::record_native(eval, label, native_decl(move |eval| {
            let sess = session(eval);
            record_target(sess, AnalyzedTarget { name: canon_label(sess, &name), ..Default::default() });
            Ok(())
        }))?;
        Ok(NoneType)
    }

    /// P3.1: `cargo_toml_env_vars` load surface — a STUB target for now; the `CARGO_PKG_*` env-file
    /// it emits lands in P3.5.
    fn native_cargo_toml_env_vars<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let label = canon_label(session(eval), &name);
        crate::dialect::record_native(eval, label, native_decl(move |eval| {
            let sess = session(eval);
            record_target(sess, AnalyzedTarget { name: canon_label(sess, &name), ..Default::default() });
            Ok(())
        }))?;
        Ok(NoneType)
    }
}

/// The synthetic `@rules_rust` module: re-exports the native rules under the names real BUILD
/// files `load()` (`rust_binary`, `rust_library`, the `cargo:defs.bzl` natives), plus the
/// `crate_universe/private:selects.bzl` `selects` namespace. Every `@rules_rust//…` load routes
/// here (`ruleset_modules` prefix), so one module serves `rust:defs.bzl`, `cargo:defs.bzl`, and
/// `selects.bzl` alike.
///
/// `selects` is a faithful pure-Starlark port of rules_rust's vendored skylib `selects.bzl`:
/// `with_or`/`with_or_dict` fan tuple keys out to one `select()` arm each. `config_setting_group`
/// (P3.1b) creates `config_setting` targets — deferred with a loud error until a crate in scope
/// needs it (cf. the skylib lib-helper policy in `shims.rs`); blake3 uses bare `select()`.
const RUST_RULES_BZL: &str = r#"
rust_binary = native_rust_binary
rust_library = native_rust_library
rust_shared_library = native_rust_shared_library
rust_library_group = native_rust_library_group
rust_doc = native_rust_doc
rust_doc_test = native_rust_doc
cargo_build_script = native_cargo_build_script
cargo_toml_env_vars = native_cargo_toml_env_vars

def _with_or_dict(input_dict, no_match_error = ""):
    output_dict = {}
    for (key_set, value) in input_dict.items():
        if type(key_set) == type(()):
            for key in key_set:
                if key in output_dict:
                    fail("key " + str(key) + " is used multiple times in " + str(input_dict))
                output_dict[key] = value
        else:
            if key_set in output_dict:
                fail("key " + str(key_set) + " is used multiple times in " + str(input_dict))
            output_dict[key_set] = value
    return output_dict

def _with_or(input_dict, no_match_error = ""):
    return select(_with_or_dict(input_dict, no_match_error), no_match_error = no_match_error)

def _config_setting_group(**kwargs):
    fail("selects.config_setting_group is not yet modeled in razel (no crate in scope needs it; lands when one does)")

selects = struct(
    with_or = _with_or,
    with_or_dict = _with_or_dict,
    config_setting_group = _config_setting_group,
)
"#;

pub(crate) fn module() -> Result<FrozenModule, String> {
    // StructType for `selects = struct(...)`; `rule_globals` for `select()` (used by `with_or`).
    let globals = GlobalsBuilder::extended_by(&[LibraryExtension::StructType])
        .with(crate::dialect::rule_globals)
        .with(rust_rules)
        .build();
    Module::with_temp_heap(|module| {
        let ast = AstModule::parse("@rules_rust", RUST_RULES_BZL.to_owned(), &Dialect::Extended)
            .map_err(|e| format!("{e}"))?;
        {
            let mut eval = Evaluator::new(&module);
            eval.eval_module(ast, &globals)
                .map_err(|e| format!("{e}"))?;
        }
        module.freeze().map_err(|e| format!("{e:?}"))
    })
}
