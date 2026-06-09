//! Starlark-defined rules + analysis (Phase 3): `rule(implementation, attrs)` returns a
//! callable custom value; instantiating it runs the rule **implementation** with a `ctx`
//! (Bazel dialect: `ctx.attr.*`, `ctx.label`, `ctx.actions.declare_file/run/write`) and
//! captures the registered actions (inputs/outputs) and `DefaultInfo` — the target
//! **analyzes**. Plus `select()` (host-config-lite) and `DefaultInfo`.
//!
//! Analysis runs in the **same eval scope** as instantiation (the impl `Value` never
//! escapes the heap) — sidestepping module freezing. Tier-2.5 simplification; a two-phase
//! freeze model comes when caching / cross-target dep-providers demand it.


use crate::state::{AnalyzedTarget, CcToolchainMode, GlobalFlags, Session, canon_label, pkg_of};
use crate::engine::{
    attr_members, cc_common_members, config_common_members, config_members, coverage_common_members,
    native_members, platform_common_members, razel_build_members, testing_members,
};
use crate::shims::{auto_config_module, rules_cc_module, rules_java_module, rules_skylib_module};
use crate::dialect::rule_globals;
use starlark::environment::{
    FrozenModule, Globals, GlobalsBuilder, LibraryExtension, Module,
};
use starlark::eval::{Evaluator, FileLoader};
use starlark::syntax::{AstModule, Dialect};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};



pub(crate) fn build_globals() -> Globals {
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
    .build()
}


/// The engine's `.bzl`-facing namespaces — razel's own (`native`/`attr`/`razel_build`) plus the Bazel
/// builtin-namespace stubs (D4) that let real upstream `.bzl` resolve. Shared by both globals builders
/// (workspace + inline) so the surface is identical in every analysis path.
pub(crate) fn engine_namespaces(b: &mut GlobalsBuilder) {
    b.namespace("native", native_members);
    b.namespace("attr", attr_members);
    b.namespace("razel_build", razel_build_members);
    b.namespace("config", config_members);
    b.namespace("platform_common", platform_common_members);
    b.namespace("config_common", config_common_members);
    b.namespace("cc_common", cc_common_members);
    b.namespace("coverage_common", coverage_common_members);
    b.namespace("testing", testing_members);
}


/// Resolve a project `.bzl` load to a file under `root`. `//pkg:f.bzl` → `root/pkg/f.bzl`;
/// `:f.bzl` → `root/<current pkg>/f.bzl`. (`@repo` loads go through [`external_bzl_path`] /
/// the synthetic rulesets, not here.)
pub(crate) fn resolve_bzl(root: &Path, label: &str, current_pkg: Option<&str>) -> Result<PathBuf, String> {
    if let Some(rest) = label.strip_prefix("//") {
        let (pkg, file) = rest
            .split_once(':')
            .ok_or_else(|| format!("bad .bzl label `{label}`"))?;
        Ok(root.join(pkg).join(file))
    } else if let Some(file) = label.strip_prefix(':') {
        let pkg = current_pkg.unwrap_or_default();
        Ok(root.join(pkg).join(file))
    } else {
        Err(format!(
            "unsupported load path `{label}` (only //pkg:f.bzl, :f.bzl, or a vendored @repo)"
        ))
    }
}


/// Resolve a vendored external load `@repo//pkg:file` to a real file under `base` (D4). The repo→dir
/// name tolerates the `_`/`-` convention (canonical `@bazel_skylib` ↔ dir `bazel-skylib`): try the
/// name as-is, then with `_`→`-`. `None` if not an `@repo//pkg:file` label or no such file. A real
/// vendored file takes precedence over razel's synthetic shim, so configured corpora run REAL upstream.
pub(crate) fn external_bzl_path(base: &Path, label: &str) -> Option<PathBuf> {
    let rest = label.strip_prefix('@')?;
    let (repo, pkgfile) = rest.split_once("//")?;
    let (pkg, file) = pkgfile.split_once(':')?;
    [repo.to_string(), repo.replace('_', "-")]
        .iter()
        .map(|dir| base.join(dir).join(pkg).join(file))
        .find(|p| p.exists())
}


/// A natively-provided ruleset: `load()`s whose path starts with `prefix`
/// (e.g. `@rules_cc//`, `@rules_rust//`) resolve to `module`, a synthetic module
/// re-exporting razel's native rules under the names real BUILD files import.
pub(crate) struct Ruleset {
    pub(crate) prefix: &'static str,
    pub(crate) module: FrozenModule,
}


/// File loader for BUILD/`.bzl` evaluation: resolves a `@repo//...` load to its
/// native [`Ruleset`] module, and any other `//pkg:f.bzl`/`:f.bzl` to a project
/// file it reads + evaluates (recursively, with this same loader) — so a BUILD can
/// `load()` a repo's own macros. (`rule()` objects can't be frozen yet, so a `.bzl`
/// that *defines* a rule will fail to freeze; macros over the native rules work.)
pub(crate) struct BzlLoader<'a> {
    rulesets: &'a [Ruleset],
    globals: &'a Globals,
    cache: RefCell<HashMap<String, FrozenModule>>,
    session: &'a Session,
}


impl FileLoader for BzlLoader<'_> {
    fn load(&self, path: &str) -> starlark::Result<FrozenModule> {
        if let Some(m) = self.cache.borrow().get(path) {
            return Ok(m.clone());
        }
        let err = |m: String| starlark::Error::new_other(anyhow::anyhow!(m));
        // D4: a REAL vendored external file (external_base set + the file exists) takes precedence over
        // the synthetic ruleset shim — so a configured corpus runs upstream `.bzl`, not razel's stand-in.
        let real_external = self
            .session
            .global
            .external_base
            .as_deref()
            .and_then(|base| external_bzl_path(base, path));
        let fs = if let Some(p) = real_external {
            p
        } else if let Some(rs) = self.rulesets.iter().find(|r| path.starts_with(r.prefix)) {
            return Ok(rs.module.clone());
        } else {
            let root = self
                .session
                .workspace
                .clone()
                .ok_or_else(|| err(format!("load(\"{path}\") needs workspace mode")))?;
            let cur = self.session.current_pkg.borrow().clone();
            resolve_bzl(&root, path, cur.as_deref()).map_err(err)?
        };
        let src = std::fs::read_to_string(&fs)
            .map_err(|e| err(format!("cannot read {}: {e}", fs.display())))?;

        let frozen = Module::with_temp_heap(|module| -> starlark::Result<FrozenModule> {
            let ast = AstModule::parse(path, src, &Dialect::Extended)?;
            {
                let mut eval = Evaluator::new(&module);
                eval.set_loader(self); // recursive: a .bzl may load other .bzl
                eval.extra = Some(self.session);
                eval.eval_module(ast, self.globals)?;
            }
            module
                .freeze()
                .map_err(|e| starlark::Error::new_other(anyhow::anyhow!("{e:?}")))
        })?;
        self.cache
            .borrow_mut()
            .insert(path.to_string(), frozen.clone());
        Ok(frozen)
    }
}


/// Every natively-provided ruleset, by `load()` prefix. New languages register a
/// row here (the rule logic itself lives in the per-language module). Each maps a
/// `@repo//` to a synthetic module re-exporting razel's native rules.
pub(crate) fn ruleset_modules(cc_toolchain: CcToolchainMode) -> Result<Vec<Ruleset>, String> {
    Ok(vec![
        Ruleset {
            prefix: "@rules_cc//",
            module: rules_cc_module(cc_toolchain)?,
        },
        Ruleset {
            prefix: "@rules_java//",
            module: rules_java_module()?, // razel's java:defs.bzl (no toolchain mode — F16)
        },
        Ruleset {
            prefix: "@bazel_skylib//",
            module: rules_skylib_module()?,
        },
        Ruleset {
            prefix: "@local_config_rocm//",
            module: auto_config_module("if_rocm_is_configured = native_if_not_configured\n")?,
        },
        // CUDA config helper lives in a specific @xla file — register the exact path
        // (not the whole @xla// prefix, which is a real source repo, not a shim).
        Ruleset {
            prefix: "@xla//xla/tsl/platform/default:cuda_build_defs.bzl",
            module: auto_config_module("if_cuda_is_configured = native_if_not_configured\n")?,
        },
        Ruleset {
            prefix: "@rules_rust//",
            module: crate::rust_rules::module()?,
        },
        Ruleset {
            prefix: "@rules_python//",
            module: crate::py_rules::module()?,
        },
        Ruleset {
            prefix: "@rules_shell//",
            module: crate::sh_rules::module()?,
        },
    ])
}


/// Evaluate one BUILD source with the ruleset loaders + the rule globals.
/// Targets it instantiates are recorded into STATE/RESULTS (re-entrant: a nested
/// cross-package load appends, never clears).
pub(crate) fn eval_build_src(session: &Session, name: &str, src: &str) -> Result<(), String> {
    let rulesets = ruleset_modules(session.global.cc_toolchain)?;
    let globals = build_globals();
    let loader = BzlLoader {
        rulesets: &rulesets,
        globals: &globals,
        cache: RefCell::new(HashMap::new()),
        session,
    };
    let ast =
        AstModule::parse(name, src.to_owned(), &Dialect::Extended).map_err(|e| format!("{e}"))?;
    Module::with_temp_heap(|module| {
        let mut eval = Evaluator::new(&module);
        eval.set_loader(&loader);
        eval.extra = Some(session); // builtins read the Session via `session(eval)`
        eval.eval_module(ast, &globals)
            .map_err(|e| format!("{e}"))?;
        Ok(())
    })
}


/// Evaluate a **real Bazel `BUILD`** that `load()`s cc rules from `@rules_cc`,
/// resolving those loads to razel's native rules (no rules_cc execution, no repo
/// fetch). Single-package (bare-name targets).
pub fn analyze_bazel(build_src: &str) -> Result<Vec<AnalyzedTarget>, String> {
    analyze_bazel_with(build_src, GlobalFlags::default())
}


/// [`analyze_bazel`] with build-wide [`GlobalFlags`] (the CLI's `--copt`/`-c`/… )
/// applied to every cc action.
pub fn analyze_bazel_with(
    build_src: &str,
    flags: GlobalFlags,
) -> Result<Vec<AnalyzedTarget>, String> {
    let session = Session::new(None, flags);
    eval_build_src(&session, "BUILD", build_src)?;
    Ok(session.take_targets())
}


/// Load a package's BUILD (once) under workspace mode, evaluating it with that
/// package as context. Cross-package deps trigger further loads via `resolve_dep`.
pub(crate) fn load_package(sess: &Session, pkg: &str) -> Result<(), String> {
    if sess.loaded.borrow().contains(pkg) {
        return Ok(());
    }
    sess.loaded.borrow_mut().insert(pkg.to_string());
    let root = sess
        .workspace
        .clone()
        .ok_or("load_package called outside workspace mode")?;
    let build_path = ["BUILD", "BUILD.bazel"]
        .iter()
        .map(|f| root.join(pkg).join(f))
        .find(|p| p.exists())
        .ok_or_else(|| format!("no BUILD in package `{pkg}` ({})", root.join(pkg).display()))?;
    let src = std::fs::read_to_string(&build_path).map_err(|e| e.to_string())?;

    // Short borrows around the nested eval (the [R1] discipline): set current_pkg, drop the
    // borrow, recurse, then restore — never hold a Session borrow across `eval_build_src`.
    let prev = sess.current_pkg.borrow_mut().replace(pkg.to_string());
    let res = eval_build_src(sess, &format!("{pkg}/BUILD"), &src);
    *sess.current_pkg.borrow_mut() = prev;
    res
}


/// Analyze a **multi-package** workspace rooted at `root`, starting from
/// `top_label` (`//pkg:name`) and loading dependency packages on demand. Targets
/// are keyed by canonical `//pkg:name` labels with package-qualified paths.
pub fn analyze_workspace(root: &Path, top_label: &str) -> Result<Vec<AnalyzedTarget>, String> {
    analyze_workspace_with(root, top_label, GlobalFlags::default())
}


/// [`analyze_workspace`] with build-wide [`GlobalFlags`] applied to every cc action.
pub fn analyze_workspace_with(
    root: &Path,
    top_label: &str,
    flags: GlobalFlags,
) -> Result<Vec<AnalyzedTarget>, String> {
    let session = Session::new(Some(root.to_path_buf()), flags);
    let top_pkg = pkg_of(&canon_label(&session, top_label))
        .ok_or_else(|| format!("top label must be //pkg:name, got `{top_label}`"))?;
    load_package(&session, &top_pkg)?;
    Ok(session.take_targets())
}


/// Evaluate a `BUILD`/`.bzl` that defines and instantiates Starlark rules, running each
/// rule impl (same-scope analysis); returns the analyzed targets.
pub fn analyze_starlark(name: &str, src: &str) -> Result<Vec<AnalyzedTarget>, String> {
    let session = Session::default();
    let ast =
        AstModule::parse(name, src.to_owned(), &Dialect::Extended).map_err(|e| format!("{e}"))?;
    let globals = GlobalsBuilder::extended_by(&[
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
    .build();
    let res: Result<(), String> = Module::with_temp_heap(|module| {
        let mut eval = Evaluator::new(&module);
        eval.extra = Some(&session);
        eval.eval_module(ast, &globals)
            .map_err(|e| format!("{e}"))?;
        Ok(())
    });
    res?;
    Ok(session.take_targets())
}


#[cfg(test)]
mod tests {
    use super::*;

    // ── fold_field (F3/F24): the LIVE transitive fold, tested directly (not only via the .bzl). ──


    #[test]
    fn starlark_rule_analyzes_by_running_its_impl() {
        let src = r#"
def _impl(ctx):
    out = ctx.actions.declare_file(ctx.attr.name + ".o")
    ctx.actions.run(
        executable = "cc",
        outputs = [out],
        inputs = [ctx.attr.src],
        arguments = ["-c", ctx.attr.src],
    )
    return [DefaultInfo(files = [out])]

cc_thing = rule(implementation = _impl, attrs = {"src": 1})
cc_thing(name = "widget", src = "widget.c")
cc_thing(name = "gadget", src = "gadget.c")
"#;
        let targets = analyze_starlark("BUILD", src).unwrap();
        assert_eq!(targets.len(), 2);
        let w = &targets[0];
        assert_eq!(w.name, "widget");
        assert_eq!(w.actions.len(), 1);
        assert_eq!(w.actions[0].mnemonic, "cc");
        assert_eq!(w.actions[0].inputs, vec!["widget.c"]);
        assert_eq!(w.actions[0].outputs, vec!["widget.o"]);
        assert_eq!(w.default_info, vec!["widget.o"]);
        assert_eq!(targets[1].name, "gadget");
    }

    #[test]
    fn select_picks_default_branch() {
        let src = r#"
def _impl(ctx):
    flags = select({"//conditions:default": ["-O2"], "@cfg//:dbg": ["-g"]})
    ctx.actions.run(executable = "cc", outputs = [ctx.attr.name], inputs = [], arguments = flags)
    return [DefaultInfo(files = [ctx.attr.name])]

thing = rule(implementation = _impl, attrs = {})
thing(name = "x")
"#;
        let targets = analyze_starlark("BUILD", src).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].actions[0].mnemonic, "cc");
    }

    #[test]
    fn dependent_reads_dep_providers_two_phase() {
        // lib declared first; bin's deps=[":lib"] reads lib's analyzed DefaultInfo.
        let src = r#"
def _lib(ctx):
    out = "lib" + ctx.attr.name + ".a"
    ctx.actions.run(executable = "ar", outputs = [out], inputs = [], arguments = ["rcs", out])
    return [DefaultInfo(files = [out])]

def _bin(ctx):
    libs = []
    for d in ctx.attr.deps:
        libs = libs + d.files
    out = ctx.attr.name
    ctx.actions.run(executable = "cc", outputs = [out], inputs = libs, arguments = ["-o", out] + libs)
    return [DefaultInfo(files = [out])]

lib_rule = rule(implementation = _lib, attrs = {})
bin_rule = rule(implementation = _bin, attrs = {})

lib_rule(name = "math")
bin_rule(name = "app", deps = [":math"])
"#;
        let targets = analyze_starlark("BUILD", src).unwrap();
        let app = targets.iter().find(|t| t.name == "app").unwrap();
        assert_eq!(app.deps, vec!["math"]);
        // app linked the dep's analyzed output — the provider flowed across targets.
        assert_eq!(app.actions[0].inputs, vec!["libmath.a"]);
        assert!(app.actions[0].argv.contains(&"libmath.a".to_string()));
    }

    #[test]
    fn forward_dep_reference_errors_clearly() {
        // bin declared before its dep → forward ref → clear error, not silently wrong.
        let src = r#"
def _lib(ctx):
    return [DefaultInfo(files = ["x"])]
def _bin(ctx):
    return [DefaultInfo(files = ctx.attr.deps[0].files)]
lib_rule = rule(implementation = _lib, attrs = {})
bin_rule = rule(implementation = _bin, attrs = {})
bin_rule(name = "app", deps = [":math"])
lib_rule(name = "math")
"#;
        assert!(analyze_starlark("BUILD", src).is_err());
    }
}
