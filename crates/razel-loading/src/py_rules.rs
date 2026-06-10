//! @rules_python → razel native rules: py_library / py_binary / py_test (no compile; launcher + PYTHONPATH).
//!
//! Registered in `rules::ruleset_modules` under the `@rules_python//` prefix; a
//! `load("@rules_python//python:defs.bzl", "py_binary")` (or `py_library`/`py_test`)
//! resolves here. Python needs no compilation: a `py_library` records its (and its
//! deps') `.py` sources for dependents; a `py_binary`/`py_test` emits ONE action that
//! writes a `/bin/sh` launcher which sets `PYTHONPATH` to the exec root and execs
//! `python3 <main>` — so package-style imports (`from lib.greeting import greet`)
//! resolve. Shared helpers (`record_target`, `canon_label`, `qualify`, `resolve_dep`,
//! `unpack`, `AnalyzedTarget`/`AnalyzedAction`) live in `crate::rules`; modelled on
//! `rules::cc_rules`.

use crate::state::{AnalyzedAction, AnalyzedTarget, canon_label, native_decl, qualify, session};
use crate::deps::{record_target, resolve_dep};
use crate::values::{unpack, unpack_strs_any};
use starlark::collections::SmallMap;
use starlark::environment::{FrozenModule, GlobalsBuilder, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};
use starlark::values::Value;
use starlark::values::list::UnpackList;
use starlark::values::none::NoneType;

const SH: &str = "/bin/sh";
const PYTHON: &str = "python3";

/// The `.py` sources this target and its deps make available, plus the resolved dep
/// canonical labels. `srcs` are package-qualified; `dep_srcs` are the transitive
/// exported sources of the deps (carried through the `hdrs` channel by `resolve_dep`).
struct PySources {
    srcs: Vec<String>,
    dep_srcs: Vec<String>,
    dep_names: Vec<String>,
}

/// Qualify this target's `srcs` and resolve `deps` to their exported source files
/// (which flow transitively through `DepInfo.hdrs`).
fn gather(
    eval: &mut Evaluator<'_, '_, '_>,
    srcs: Vec<String>,
    deps: Vec<String>,
) -> anyhow::Result<PySources> {
    let sess = session(eval);
    let srcs: Vec<String> = srcs.iter().map(|s| qualify(sess, s)).collect();
    let (mut dep_srcs, mut dep_names) = (Vec::new(), Vec::new());
    for d in &deps {
        let dep = resolve_dep(eval, d)?;
        dep_srcs.extend(dep.field("py_srcs"));
        dep_names.push(dep.canon);
    }
    Ok(PySources {
        srcs,
        dep_srcs,
        dep_names,
    })
}

/// Shell-single-quote a string for safe embedding inside `'...'`.
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[starlark::starlark_module]
pub(crate) fn py_rules(b: &mut GlobalsBuilder) {
    /// `py_library(name, srcs, deps=?, **kw)` — no build action. Records its (and its
    /// deps') `.py` sources so dependents can import them; exposes them via both
    /// `default_info` and the `hdrs` channel (which `resolve_dep` propagates transitively).
    fn native_py_library<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<Value<'v>>,
        #[starlark(require = named)] deps: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        // E0c: record now, analyze in the demand-driven pass (forward refs resolve).
        let label = canon_label(session(eval), &name);
        let (srcs, deps) = (unpack_strs_any(srcs), unpack_strs_any(deps));
        crate::dialect::record_native(eval, label, native_decl(move |eval| {
            let g = gather(eval, srcs.clone(), deps.clone())?;
            let sess = session(eval);
            // Exported sources: own srcs + transitive dep srcs (the PyInfo channel).
            let mut exported = g.srcs.clone();
            exported.extend(g.dep_srcs);
            let mut t = AnalyzedTarget {
                name: canon_label(sess, &name),
                deps: g.dep_names,
                actions: Vec::new(),
                default_info: g.srcs,
                ..Default::default()
            };
            t.set_set("PyInfo", "srcs", exported); // C3a.5: py's own channel, not cc's hdrs
            record_target(sess, t);
            Ok(())
        }))?;
        Ok(NoneType)
    }

    /// `py_binary(name, srcs, deps=?, main=?, **kw)` — emit ONE action that writes a
    /// `/bin/sh` launcher (then `chmod +x`). The launcher sets `PYTHONPATH` to the exec
    /// root (computed from the launcher's own location) and execs `python3 <main>`. The
    /// action's inputs are this target's `.py` srcs + the transitive dep srcs, so they're
    /// present in the sandbox and part of the cache key.
    fn native_py_binary<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<Value<'v>>,
        #[starlark(require = named)] deps: Option<Value<'v>>,
        #[starlark(require = named)] main: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let main = main.and_then(|m| m.unpack_str().map(String::from));
        py_executable(eval, name, srcs, deps, main)
    }

    /// `py_test(...)` — same launcher mechanism as `py_binary`.
    fn native_py_test<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<Value<'v>>,
        #[starlark(require = named)] deps: Option<Value<'v>>,
        #[starlark(require = named)] main: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let main = main.and_then(|m| m.unpack_str().map(String::from));
        py_executable(eval, name, srcs, deps, main)
    }
}

/// Shared body of `py_binary`/`py_test`: RECORD the launcher target (E0c — analyzed on demand).
fn py_executable<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    name: String,
    srcs: Option<Value<'v>>,
    deps: Option<Value<'v>>,
    main: Option<String>,
) -> anyhow::Result<NoneType> {
    let label = canon_label(session(eval), &name);
    let (srcs, deps) = (unpack_strs_any(srcs), unpack_strs_any(deps));
    crate::dialect::record_native(eval, label, native_decl(move |eval| {
    let g = gather(eval, srcs.clone(), deps.clone())?;
    let sess = session(eval);
    // Entrypoint: `main` (package-qualified) or the first src (already qualified).
    let entry = match main {
        Some(m) => qualify(sess, &m),
        None => g
            .srcs
            .first()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("py_binary `{name}` has no srcs and no main"))?,
    };

    let out = qualify(sess, &name);
    // Depth of the launcher's package → how many `../` to climb back to the exec root.
    // `out` is `pkg/.../name`; the exec root is that many parents up from the launcher.
    let depth = out.matches('/').count();
    let up = "/..".repeat(depth);

    // The launcher: resolve its own dir, climb to the exec root, set PYTHONPATH there,
    // then exec python3 on the (exec-root-relative) entrypoint so package imports work.
    let launcher = format!(
        "#!/bin/sh\n\
         here=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\n\
         root=$(CDPATH= cd -- \"$here{up}\" && pwd)\n\
         export PYTHONPATH=\"$root${{PYTHONPATH:+:$PYTHONPATH}}\"\n\
         exec {PYTHON} \"$root/{entry}\" \"$@\"\n"
    );

    // ONE action: write the launcher via a real tool (`/bin/sh -c`) and mark it +x.
    let script = format!(
        "printf '%s' {} > {} && chmod +x {}",
        shq(&launcher),
        shq(&out),
        shq(&out)
    );

    let mut inputs = g.srcs.clone();
    inputs.extend(g.dep_srcs);

    let action = AnalyzedAction {
        mnemonic: "PyLauncher".into(),
        argv: vec![SH.into(), "-c".into(), script],
        inputs,
        outputs: vec![out.clone()],
    };
    record_target(sess, AnalyzedTarget {
        name: canon_label(sess, &name),
        deps: g.dep_names,
        actions: vec![action],
        default_info: vec![out],
        providers: Default::default(),
    });
    Ok(())
    }))?;
    Ok(NoneType)
}

/// The synthetic `@rules_python` module: re-exports the native rules under the names
/// real BUILD files `load()` (`py_binary`, `py_library`, `py_test`).
pub(crate) fn module() -> Result<FrozenModule, String> {
    let globals = GlobalsBuilder::standard().with(py_rules).with(crate::dialect::rule_globals).build();
    Module::with_temp_heap(|module| {
        let ast = AstModule::parse(
            "@rules_python",
            "py_binary = native_py_binary\npy_library = native_py_library\npy_test = native_py_test\n\
             PyInfo = provider(fields = [\"transitive_sources\", \"imports\"])\n\
             def _py_proto_stub_impl(ctx):\n    return [DefaultInfo(files = [])]\n\
             py_proto_library = rule(implementation = _py_proto_stub_impl, attrs = {})\n"
                .to_owned(),
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
