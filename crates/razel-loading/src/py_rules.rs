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

use crate::state::{AnalyzedAction, AnalyzedTarget, Session, canon_label, qualify, session};
use crate::deps::{record_target, resolve_dep};
use crate::values::unpack;
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
    sess: &Session,
    srcs: Option<UnpackList<String>>,
    deps: Option<UnpackList<String>>,
) -> anyhow::Result<PySources> {
    let srcs: Vec<String> = unpack(srcs).iter().map(|s| qualify(sess, s)).collect();
    let (mut dep_srcs, mut dep_names) = (Vec::new(), Vec::new());
    for d in &unpack(deps) {
        let dep = resolve_dep(sess, d)?;
        dep_srcs.extend(dep.hdrs);
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
fn py_rules(b: &mut GlobalsBuilder) {
    /// `py_library(name, srcs, deps=?, **kw)` — no build action. Records its (and its
    /// deps') `.py` sources so dependents can import them; exposes them via both
    /// `default_info` and the `hdrs` channel (which `resolve_dep` propagates transitively).
    fn native_py_library<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(require = named)] deps: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let g = gather(sess, srcs, deps)?;
        // Exported sources: own srcs + transitive dep srcs (the `hdrs` channel).
        let mut exported = g.srcs.clone();
        exported.extend(g.dep_srcs);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: g.dep_names,
            actions: Vec::new(),
            default_info: g.srcs,
            hdrs: exported,
            cflags: Vec::new(),
            compile_jars: Vec::new(),
            runtime_jars: Vec::new(),
            neverlink: false,
        });
        Ok(NoneType)
    }

    /// `py_binary(name, srcs, deps=?, main=?, **kw)` — emit ONE action that writes a
    /// `/bin/sh` launcher (then `chmod +x`). The launcher sets `PYTHONPATH` to the exec
    /// root (computed from the launcher's own location) and execs `python3 <main>`. The
    /// action's inputs are this target's `.py` srcs + the transitive dep srcs, so they're
    /// present in the sandbox and part of the cache key.
    fn native_py_binary<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(require = named)] deps: Option<UnpackList<String>>,
        #[starlark(require = named)] main: Option<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        py_executable(session(eval), name, srcs, deps, main)
    }

    /// `py_test(...)` — same launcher mechanism as `py_binary`.
    fn native_py_test<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(require = named)] deps: Option<UnpackList<String>>,
        #[starlark(require = named)] main: Option<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        py_executable(session(eval), name, srcs, deps, main)
    }
}

/// Shared body of `py_binary`/`py_test`: build the launcher target.
fn py_executable(
    sess: &Session,
    name: String,
    srcs: Option<UnpackList<String>>,
    deps: Option<UnpackList<String>>,
    main: Option<String>,
) -> anyhow::Result<NoneType> {
    let g = gather(sess, srcs, deps)?;
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
        hdrs: Vec::new(),
        cflags: Vec::new(),
        compile_jars: Vec::new(),
        runtime_jars: Vec::new(),
        neverlink: false,
    });
    Ok(NoneType)
}

/// The synthetic `@rules_python` module: re-exports the native rules under the names
/// real BUILD files `load()` (`py_binary`, `py_library`, `py_test`).
pub(crate) fn module() -> Result<FrozenModule, String> {
    let globals = GlobalsBuilder::standard().with(py_rules).build();
    Module::with_temp_heap(|module| {
        let ast = AstModule::parse(
            "@rules_python",
            "py_binary = native_py_binary\npy_library = native_py_library\npy_test = native_py_test\n"
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
