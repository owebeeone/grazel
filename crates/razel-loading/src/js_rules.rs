//! @razel_js → razel native rules: js_binary / ts_project-lite (S3a, V3sh1).
//!
//! The razel-NATIVE rulepack (gryth's grammar — loaded in E-mode packages via
//! `load("@razel_js//js:defs.bzl", ...)`; a bazel-side compat shim is design C's later
//! work). Registered in `rules::ruleset_modules` under the `@razel_js//` prefix.
//!
//! - `js_binary(name, entry, srcs=[], node_modules=None)`: ONE action assembling a
//!   runfiles dir (`<name>.runfiles/` with the entry+srcs copied in and `node_modules`
//!   symlinked from the workspace — physical adjacency, so ESM resolution works) plus
//!   a launcher script that execs host `node` on the entry. DefaultInfo = the launcher.
//! - `ts_project(name, srcs, node_modules=None)`: ONE tsc action (`-lite`: no
//!   tsconfig modeling yet) compiling srcs to `<name>_out/`; tsc comes from the
//!   target's own node_modules (typescript dep) or host PATH — the cc-host-toolchain
//!   posture: non-hermetic, visible in the argv.
//!
//! Shared helpers from `crate::rules` per the sh_rules/py_rules pattern.

use crate::deps::record_target;
use crate::state::{AnalyzedAction, AnalyzedTarget, Session, canon_label, qualify, session};
use crate::values::unpack_strs;
use starlark::collections::SmallMap;
use starlark::environment::{FrozenModule, GlobalsBuilder, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};
use starlark::values::Value;
use starlark::values::list::UnpackList;
use starlark::values::none::NoneType;

/// The launcher for `js_binary` — one `/bin/sh -c` action emitting one output file.
///
/// V1 runs the entry FROM THE SOURCE TREE: the launcher lands in the entry's package
/// dir, so `$(dirname $0)/<entry>` is the source file, and node's own walk-up finds
/// the workspace's fetch-npm-materialized `node_modules` (physical adjacency — ESM
/// resolution included). No runfiles assembly: the executor sandbox stages declared
/// outputs only, and a Bazel-faithful runfiles tree is the spike's later runfiles
/// step, not S3a. `node_modules`/`srcs` are recorded as inputs (the dep edge as
/// data), not staged.
fn js_binary_action(entry: &str, entry_q: &str, srcs_q: &[String], out_q: &str) -> AnalyzedAction {
    let script = format!(
        "{{ echo '#!/bin/sh'; echo 'exec node \"$(dirname \"$0\")/{entry}\" \"$@\"'; }} > {out_q} && chmod +x {out_q}"
    );
    let mut inputs = vec![entry_q.to_string()];
    inputs.extend(srcs_q.iter().cloned());
    AnalyzedAction {
        mnemonic: "JsBinary".into(),
        argv: vec!["/bin/sh".into(), "-c".into(), script],
        inputs,
        outputs: vec![out_q.to_string()],
    }
}

fn analyze_js_binary(
    sess: &Session,
    name: String,
    entry: Option<String>,
    srcs: Vec<String>,
    node_modules: Option<String>,
) -> anyhow::Result<NoneType> {
    let entry =
        entry.ok_or_else(|| anyhow::anyhow!("js_binary `{name}`: `entry` is required"))?;
    let entry_q = qualify(sess, &entry);
    let srcs_q: Vec<String> = srcs.iter().map(|s| qualify(sess, s)).collect();
    let _ = node_modules; // dep edge as data; resolution is node's walk-up (see above)
    let out_q = qualify(sess, &name);
    record_target(sess, AnalyzedTarget {
        name: canon_label(sess, &name),
        deps: Vec::new(),
        actions: vec![js_binary_action(&entry, &entry_q, &srcs_q, &out_q)],
        default_info: vec![out_q],
        providers: Default::default(),
    });
    Ok(NoneType)
}

fn analyze_ts_project(
    sess: &Session,
    name: String,
    srcs: Vec<String>,
    node_modules: Option<String>,
) -> anyhow::Result<NoneType> {
    if srcs.is_empty() {
        anyhow::bail!("ts_project `{name}`: `srcs` must list the .ts sources");
    }
    let srcs_q: Vec<String> = srcs.iter().map(|s| qualify(sess, s)).collect();
    let out_dir = qualify(sess, &format!("{name}_out"));
    // tsc from the target's own node_modules (the locked typescript), else host PATH.
    let tsc = match node_modules.map(|n| qualify(sess, &n)) {
        Some(nm) => format!("node {nm}/typescript/bin/tsc"),
        None => "tsc".to_string(),
    };
    let outputs: Vec<String> = srcs_q
        .iter()
        .map(|s| {
            let base = s.rsplit('/').next().unwrap_or(s);
            format!("{out_dir}/{}", base.replace(".ts", ".js"))
        })
        .collect();
    let script = format!("{tsc} --outDir {out_dir} {}", srcs_q.join(" "));
    record_target(sess, AnalyzedTarget {
        name: canon_label(sess, &name),
        deps: Vec::new(),
        actions: vec![AnalyzedAction {
            mnemonic: "TsProject".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), script],
            inputs: srcs_q,
            outputs: outputs.clone(),
        }],
        default_info: outputs,
        providers: Default::default(),
    });
    Ok(NoneType)
}

#[starlark::starlark_module]
fn js_rules(b: &mut GlobalsBuilder) {
    fn native_js_binary<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] entry: Option<String>,
        #[starlark(require = named)] srcs: Option<UnpackList<Value<'v>>>,
        #[starlark(require = named)] node_modules: Option<String>,
        #[starlark(require = named)] data: Option<UnpackList<Value<'v>>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let _ = data;
        analyze_js_binary(session(eval), name, entry, unpack_strs(srcs), node_modules)
    }

    fn native_ts_project<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<Value<'v>>>,
        #[starlark(require = named)] node_modules: Option<String>,
        #[starlark(require = named)] deps: Option<UnpackList<Value<'v>>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let _ = deps;
        analyze_ts_project(session(eval), name, unpack_strs(srcs), node_modules)
    }
}

/// The synthetic `@razel_js` module: re-exports under the loaded names.
pub(crate) fn module() -> Result<FrozenModule, String> {
    let globals = GlobalsBuilder::standard().with(js_rules).build();
    Module::with_temp_heap(|module| {
        let ast = AstModule::parse(
            "@razel_js",
            "js_binary = native_js_binary\nts_project = native_ts_project\n".to_owned(),
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
