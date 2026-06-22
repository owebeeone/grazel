//! @aspect_rules_js / @aspect_rules_ts → razel native rules: js_binary / ts_project
//! (S3a, V3sh1 — re-surfaced same-day per Gianni's NO-COMPETING-SURFACES rule:
//! anything with an existing bazel-ecosystem surface uses THAT surface; razel
//! implements a faithful SUBSET, never a parallel dialect. Aspect's rules_js/rules_ts
//! are the JS incumbents, so the load lines and attr names below are theirs:
//! `load("@aspect_rules_js//js:defs.bzl", "js_binary")` with `entry_point`;
//! `load("@aspect_rules_ts//ts:defs.bzl", "ts_project")`. The original `@razel_js`
//! name lived for a few hours and died with zero consumers.)
//!
//! Registered in `rules::ruleset_modules` under both `@aspect_rules_js//` and
//! `@aspect_rules_ts//` prefixes (one module exports both names). SUBSET deviations,
//! named: npm deps come from the S2 fetch-npm workspace `node_modules` via node's
//! walk-up (not aspect's npm_translate_lock/linked-targets machinery); tsc resolves
//! at action time from `node_modules/typescript` else host PATH; unknown attrs are
//! absorbed, not modeled.
//!
//! - `js_binary(name, entry_point, srcs=[], data=[])`: ONE action emitting a launcher
//!   that execs the SOURCE entry by its workspace-relative path (`node <entry_q>`, run from
//!   the exec root — robust to the output base; node's walk-up finds the workspace
//!   node_modules; runfiles trees are the spike's later runfiles step). DefaultInfo = the
//!   launcher.
//! - `ts_project(name, srcs, deps=[])`: ONE tsc action (`-lite`: no tsconfig modeling
//!   yet) compiling srcs to `<name>_out/` — the cc-host-toolchain posture:
//!   non-hermetic, visible in the argv.
//!
//! Shared helpers from `crate::rules` per the sh_rules/py_rules pattern.

use crate::deps::record_target;
use crate::state::{
    AnalyzedAction, AnalyzedTarget, Session, canon_label, qualify, qualify_output, session,
};
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
/// V1 runs the entry FROM THE SOURCE TREE via its workspace-relative path (`entry_q`), with
/// the launcher invoked from the exec root (`razel run`/`test` set `cwd` = workspace). This is
/// robust to the output base: the launcher can live in-tree OR under `razel-out/<config>/bin`
/// while the entry stays a source — `node <entry_q>` resolves identically, and node's own
/// walk-up from the entry's dir finds the workspace's fetch-npm-materialized `node_modules`
/// (ESM resolution included). No runfiles assembly: the executor sandbox stages declared
/// outputs only; a Bazel-faithful runfiles tree (which would make the launcher
/// cwd-independent) is the spike's later runfiles step, not S3a. `node_modules`/`srcs` are
/// recorded as inputs (the dep edge as data), not staged.
fn js_binary_action(entry_q: &str, srcs_q: &[String], out_q: &str) -> AnalyzedAction {
    let script = format!(
        "{{ echo '#!/bin/sh'; echo 'exec node \"{entry_q}\" \"$@\"'; }} > {out_q} && chmod +x {out_q}"
    );
    let mut inputs = vec![entry_q.to_string()];
    inputs.extend(srcs_q.iter().cloned());
    AnalyzedAction {
        mnemonic: "JsBinary".into(),
        argv: vec!["/bin/sh".into(), "-c".into(), script],
        inputs,
        outputs: vec![out_q.to_string()],
        description: String::new(),
    }
}

fn analyze_js_binary(
    sess: &Session,
    name: String,
    entry_point: Option<String>,
    srcs: Vec<String>,
) -> anyhow::Result<NoneType> {
    let entry = entry_point
        .ok_or_else(|| anyhow::anyhow!("js_binary `{name}`: `entry_point` is required"))?;
    let entry_q = qualify(sess, &entry);
    let srcs_q: Vec<String> = srcs.iter().map(|s| qualify(sess, s)).collect();
    let out_q = qualify_output(sess, &name);
    record_target(sess, AnalyzedTarget {
        name: canon_label(sess, &name),
        deps: Vec::new(),
        actions: vec![js_binary_action(&entry_q, &srcs_q, &out_q)],
        default_info: vec![out_q],
        providers: Default::default(),
    });
    Ok(NoneType)
}

fn analyze_ts_project(
    sess: &Session,
    name: String,
    srcs: Vec<String>,
) -> anyhow::Result<NoneType> {
    if srcs.is_empty() {
        anyhow::bail!("ts_project `{name}`: `srcs` must list the .ts sources");
    }
    let srcs_q: Vec<String> = srcs.iter().map(|s| qualify(sess, s)).collect();
    let out_dir = qualify_output(sess, &format!("{name}_out"));
    // tsc resolved AT ACTION TIME: the workspace's locked typescript if materialized,
    // else host PATH (constant argv — machine differences don't fork the cache key).
    let tsc = "if [ -f node_modules/typescript/bin/tsc ]; \
               then TSC='node node_modules/typescript/bin/tsc'; else TSC=tsc; fi; $TSC";
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
            description: String::new(),
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
        #[starlark(require = named)] entry_point: Option<String>,
        #[starlark(require = named)] srcs: Option<UnpackList<Value<'v>>>,
        #[starlark(require = named)] data: Option<UnpackList<Value<'v>>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let _ = data;
        analyze_js_binary(session(eval), name, entry_point, unpack_strs(srcs))
    }

    fn native_js_test<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] entry_point: Option<String>,
        #[starlark(require = named)] srcs: Option<UnpackList<Value<'v>>>,
        #[starlark(require = named)] data: Option<UnpackList<Value<'v>>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        // S5: same launcher as js_binary (the test IS its runnable; the test
        // protocol — exit code, test.log, summary — is the `test` verb's job).
        let _ = data;
        analyze_js_binary(session(eval), name, entry_point, unpack_strs(srcs))
    }

    fn native_js_library<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<Value<'v>>>,
        #[starlark(require = named)] deps: Option<UnpackList<Value<'v>>>,
        #[starlark(require = named)] data: Option<UnpackList<Value<'v>>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        // Grouping rule (aspect surface): no actions; DefaultInfo = the srcs.
        // Resolution stays node's walk-up — the library is graph structure.
        let _ = (deps, data);
        let sess = session(eval);
        let srcs_q: Vec<String> = unpack_strs(srcs).iter().map(|s| qualify(sess, s)).collect();
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: Vec::new(),
            actions: Vec::new(),
            default_info: srcs_q,
            providers: Default::default(),
        });
        Ok(NoneType)
    }

    fn native_ts_project<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<Value<'v>>>,
        #[starlark(require = named)] deps: Option<UnpackList<Value<'v>>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let _ = deps;
        analyze_ts_project(session(eval), name, unpack_strs(srcs))
    }
}

/// The synthetic aspect module: re-exports under the names real BUILD files `load()`
/// (one module serves both `@aspect_rules_js//` and `@aspect_rules_ts//` prefixes).
pub(crate) fn module() -> Result<FrozenModule, String> {
    let globals = GlobalsBuilder::standard().with(js_rules).build();
    Module::with_temp_heap(|module| {
        let ast = AstModule::parse(
            "@aspect_rules_js",
            "js_binary = native_js_binary\njs_test = native_js_test\n\
             js_library = native_js_library\nts_project = native_ts_project\n"
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
