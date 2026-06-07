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

use crate::rules::{
    AnalyzedAction, AnalyzedTarget, canon_label, qualify, record_target, resolve_dep, unpack,
};
use starlark::collections::SmallMap;
use starlark::environment::{FrozenModule, GlobalsBuilder, Module};
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
    deps: Option<UnpackList<String>>,
) -> anyhow::Result<(Vec<String>, Vec<String>, Vec<String>)> {
    let (mut args, mut inputs, mut names) = (Vec::new(), Vec::new(), Vec::new());
    for d in &unpack(deps) {
        let dep = resolve_dep(d)?;
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
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(require = named)] deps: Option<UnpackList<String>>,
        #[starlark(require = named)] edition: Option<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        let srcs: Vec<String> = unpack(srcs).iter().map(|s| qualify(s)).collect();
        let crate_root = srcs
            .first()
            .ok_or_else(|| anyhow::anyhow!("rust_library `{name}` needs at least one src"))?
            .clone();
        let edition = edition.unwrap_or_else(|| "2021".into());
        let (extern_flags, dep_rlibs, dep_names) = extern_args(deps)?;

        let rlib = qualify(&format!("lib{name}.rlib"));
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
        record_target(AnalyzedTarget {
            name: canon_label(&name),
            deps: dep_names,
            actions: vec![AnalyzedAction {
                mnemonic: "Rustc".into(),
                argv,
                inputs,
                outputs: vec![rlib.clone()],
            }],
            default_info: vec![rlib],
            hdrs: Vec::new(),
            cflags: Vec::new(),
        });
        Ok(NoneType)
    }

    /// `rust_binary(name, srcs, deps=[], edition="2021")` → one `rustc` action
    /// compiling `srcs[0]` to the `<name>` executable, linking dep rlibs.
    fn native_rust_binary<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(require = named)] deps: Option<UnpackList<String>>,
        #[starlark(require = named)] edition: Option<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        let srcs: Vec<String> = unpack(srcs).iter().map(|s| qualify(s)).collect();
        let crate_root = srcs
            .first()
            .ok_or_else(|| anyhow::anyhow!("rust_binary `{name}` needs at least one src"))?
            .clone();
        let edition = edition.unwrap_or_else(|| "2021".into());
        let (extern_flags, dep_rlibs, dep_names) = extern_args(deps)?;

        let out = qualify(&name);
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
        record_target(AnalyzedTarget {
            name: canon_label(&name),
            deps: dep_names,
            actions: vec![AnalyzedAction {
                mnemonic: "Rustc".into(),
                argv,
                inputs,
                outputs: vec![out.clone()],
            }],
            default_info: vec![out],
            hdrs: Vec::new(),
            cflags: Vec::new(),
        });
        Ok(NoneType)
    }
}

/// The synthetic `@rules_rust` module: re-exports the native rules under the names
/// real BUILD files `load()` (`rust_binary`, `rust_library`).
pub(crate) fn module() -> Result<FrozenModule, String> {
    let globals = GlobalsBuilder::standard().with(rust_rules).build();
    Module::with_temp_heap(|module| {
        let ast = AstModule::parse(
            "@rules_rust",
            "rust_binary = native_rust_binary\nrust_library = native_rust_library\n".to_owned(),
            &Dialect::Extended,
        )
        .map_err(|e| format!("{e}"))?;
        {
            let mut eval = Evaluator::new(&module);
            eval.eval_module(ast, &globals)
                .map_err(|e| format!("{e}"))?;
        }
        module.freeze().map_err(|e| format!("{e:?}"))
    })
}
