//! Native cc rules (host-compiler backend): cc_library/cc_binary actions + flag helpers. C0.

use crate::state::{AR, AnalyzedAction, AnalyzedTarget, Session, canon_label, qualify, session};
use crate::deps::{record_target, resolve_dep};
use crate::values::unpack;
use starlark::collections::SmallMap;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::values::list::UnpackList;
use starlark::values::none::NoneType;
use starlark::values::Value;



#[starlark::starlark_module]
pub(crate) fn cc_rules(b: &mut GlobalsBuilder) {
    // cc_library legitimately has many named attrs (name/srcs/hdrs/deps/copts/...).
    #[allow(clippy::too_many_arguments)]
    fn native_cc_library<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(require = named)] hdrs: Option<UnpackList<String>>,
        #[starlark(require = named)] deps: Option<UnpackList<String>>,
        #[starlark(require = named)] copts: Option<UnpackList<String>>,
        #[starlark(require = named)] defines: Option<UnpackList<String>>,
        #[starlark(require = named)] includes: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let srcs: Vec<String> = unpack(srcs).iter().map(|s| qualify(sess, s)).collect();
        let hdrs: Vec<String> = unpack(hdrs).iter().map(|h| qualify(sess, h)).collect();
        let copts = unpack(copts);

        let (mut dep_names, mut dep_hdrs, mut dep_cflags) = (Vec::new(), Vec::new(), Vec::new());
        for d in &unpack(deps) {
            let dep = resolve_dep(sess, d)?;
            dep_hdrs.extend(dep.field("headers"));
            dep_cflags.extend(dep.field("cflags"));
            dep_names.push(dep.canon);
        }

        // OWN exported flags (defines/includes); the transitive set is the DDS fold for dependents
        // (C2d — store own, fold transitive). dep_cflags is already that transitive closure.
        let mut own_cflags = define_flags(defines);
        own_cflags.extend(include_flags(sess, includes));
        // This lib's own compiles see global flags first, then local copts, then own + dep exports.
        let mut compile_flags = sess.global.copts.clone();
        compile_flags.extend(copts);
        compile_flags.extend(own_cflags.iter().cloned());
        compile_flags.extend(dep_cflags.iter().cloned());

        let mut avail_hdrs = hdrs.clone();
        avail_hdrs.extend(dep_hdrs.iter().cloned());

        let (mut actions, mut objs) = (Vec::new(), Vec::new());
        for s in &srcs {
            let o = format!("{s}.o");
            let mut inputs = vec![s.clone()];
            inputs.extend(avail_hdrs.iter().cloned());
            actions.push(compile_action(&sess.host_cc(), s, &o, &compile_flags, inputs));
            objs.push(o);
        }
        let lib = qualify(sess, &format!("lib{name}.a"));
        let mut ar_argv = vec![AR.into(), "rcs".into(), lib.clone()];
        ar_argv.extend(objs.clone());
        actions.push(AnalyzedAction {
            mnemonic: "CppArchive".into(),
            argv: ar_argv,
            inputs: objs,
            outputs: vec![lib.clone()],
        });

        // Store OWN providers (C2d); dependents recover the transitive closure via the DDS fold.
        let mut t = AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: dep_names,
            actions,
            default_info: vec![lib],
            ..Default::default()
        };
        t.set_set("CcInfo", "hdrs", hdrs);
        t.set_set("CcInfo", "cflags", own_cflags);
        record_target(sess, t);
        Ok(NoneType)
    }

    fn native_cc_binary<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(require = named)] deps: Option<UnpackList<String>>,
        #[starlark(require = named)] copts: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let srcs: Vec<String> = unpack(srcs).iter().map(|s| qualify(sess, s)).collect();
        let (mut dep_names, mut dep_libs, mut dep_hdrs, mut dep_cflags) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for d in &unpack(deps) {
            let dep = resolve_dep(sess, d)?;
            dep_hdrs.extend(dep.field("headers"));
            dep_cflags.extend(dep.field("cflags"));
            dep_libs.extend(dep.libs);
            dep_names.push(dep.canon);
        }
        // Binary compiles see global flags + local copts + the deps' exported flags.
        let mut compile_flags = sess.global.copts.clone();
        compile_flags.extend(unpack(copts));
        compile_flags.extend(dep_cflags);

        let (mut actions, mut objs) = (Vec::new(), Vec::new());
        for s in &srcs {
            let o = format!("{s}.o");
            let mut inputs = vec![s.clone()];
            inputs.extend(dep_hdrs.iter().cloned());
            actions.push(compile_action(&sess.host_cc(), s, &o, &compile_flags, inputs));
            objs.push(o);
        }
        let out = qualify(sess, &name);
        let mut link_inputs = objs.clone();
        link_inputs.extend(dep_libs.clone());
        let mut link_argv = vec![sess.host_cc(), "-o".into(), out.clone()];
        link_argv.extend(objs);
        link_argv.extend(dep_libs);
        link_argv.extend(sess.global.linkopts.clone());
        actions.push(AnalyzedAction {
            mnemonic: "CppLink".into(),
            argv: link_argv,
            inputs: link_inputs,
            outputs: vec![out.clone()],
        });
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: dep_names,
            actions,
            default_info: vec![out],
            providers: Default::default(),
        });
        Ok(NoneType)
    }
}



/// `defines = ["FOO=1"]` → `["-DFOO=1"]`.
pub(crate) fn define_flags(defines: Option<UnpackList<String>>) -> Vec<String> {
    unpack(defines).iter().map(|d| format!("-D{d}")).collect()
}


/// `includes = ["inc"]` → `["-Ipkg/inc"]` (package-qualified include dirs).
pub(crate) fn include_flags(sess: &Session, includes: Option<UnpackList<String>>) -> Vec<String> {
    unpack(includes)
        .iter()
        .map(|i| format!("-I{}", qualify(sess, i)))
        .collect()
}


/// A C++ compile action. `-iquote .` makes workspace-root-relative quote-includes
/// (`#include "pkg/x.h"`) resolve from the sandbox root (= exec root); `flags` are
/// the target's copts + transitive defines/includes.
pub(crate) fn compile_action(cc: &str, src: &str, obj: &str, flags: &[String], inputs: Vec<String>) -> AnalyzedAction {
    let mut argv = vec![cc.to_string(), "-iquote".into(), ".".into()];
    argv.extend(flags.iter().cloned());
    argv.extend(["-c".into(), src.into(), "-o".into(), obj.into()]);
    AnalyzedAction {
        mnemonic: "CppCompile".into(),
        argv,
        inputs,
        outputs: vec![obj.into()],
    }
}

