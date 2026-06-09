//! @rules_shell → razel native rules: sh_binary / sh_test (wrap the script as the executable).
//!
//! Registered in `rules::ruleset_modules` under the `@rules_shell//` prefix; a
//! `load("@rules_shell//...", ...)` resolves here. A shell script needs no
//! compilation — the build's output *is* the runnable script. The native rule emits
//! a single action that copies the (single) `srcs[0]` script to the target output and
//! marks it executable, so the produced file is directly runnable. Shared helpers
//! live in `crate::rules` (`record_target`, `canon_label`, `qualify`, `unpack`,
//! `AnalyzedTarget`/`AnalyzedAction`); modeled on `rules::cc_rules`.

use crate::rules::{
    AnalyzedAction, AnalyzedTarget, Session, canon_label, qualify, record_target, session, unpack,
};
use starlark::collections::SmallMap;
use starlark::environment::{FrozenModule, GlobalsBuilder, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};
use starlark::values::Value;
use starlark::values::list::UnpackList;
use starlark::values::none::NoneType;

/// `/bin/sh -c "cp <src> <out> && chmod +x <out>"` — copy the script to the target
/// output and make it runnable. (`cp` alone doesn't reliably yield an exec bit in the
/// sandbox, so we chmod explicitly.) Input = the qualified src; output = `out`.
fn install_action(src: &str, out: &str) -> AnalyzedAction {
    let script = format!("cp {src} {out} && chmod +x {out}");
    AnalyzedAction {
        mnemonic: "ShBinary".into(),
        argv: vec!["/bin/sh".into(), "-c".into(), script],
        inputs: vec![src.to_string()],
        outputs: vec![out.to_string()],
    }
}

/// Analyze a `sh_binary`/`sh_test`: take the single primary script `srcs[0]`, install
/// it as the executable output `qualify(name)`, and record the target with that output
/// as its `DefaultInfo`. `data`/`deps` and any unknown kwargs are absorbed.
fn analyze_sh(
    sess: &Session,
    name: String,
    srcs: Option<UnpackList<String>>,
) -> anyhow::Result<NoneType> {
    let srcs = unpack(srcs);
    let src = srcs
        .first()
        .ok_or_else(|| anyhow::anyhow!("sh rule `{name}` needs a script in srcs"))?;
    let src = qualify(sess, src);
    let out = qualify(sess, &name);
    record_target(sess, AnalyzedTarget {
        name: canon_label(sess, &name),
        deps: Vec::new(),
        actions: vec![install_action(&src, &out)],
        default_info: vec![out],
        hdrs: Vec::new(),
        cflags: Vec::new(),
        compile_jars: Vec::new(),
    });
    Ok(NoneType)
}

#[starlark::starlark_module]
fn sh_rules(b: &mut GlobalsBuilder) {
    fn native_sh_binary<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(require = named)] data: Option<UnpackList<String>>,
        #[starlark(require = named)] deps: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let _ = (data, deps);
        analyze_sh(session(eval), name, srcs)
    }

    fn native_sh_test<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(require = named)] data: Option<UnpackList<String>>,
        #[starlark(require = named)] deps: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let _ = (data, deps);
        analyze_sh(session(eval), name, srcs)
    }
}

/// The synthetic `@rules_shell` module: re-exports the native rules under the names
/// real BUILD files `load()` (`sh_binary`, `sh_test`).
pub(crate) fn module() -> Result<FrozenModule, String> {
    let globals = GlobalsBuilder::standard().with(sh_rules).build();
    Module::with_temp_heap(|module| {
        let ast = AstModule::parse(
            "@rules_shell",
            "sh_binary = native_sh_binary\nsh_test = native_sh_test\n".to_owned(),
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
