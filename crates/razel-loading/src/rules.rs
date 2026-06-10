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
    attr_members, config_common_members, config_members, native_members, razel_build_members,
};
use crate::shims::{auto_config_module, rules_cc_module, rules_java_module, rules_skylib_module};
use crate::dialect::rule_globals;
use starlark::environment::{
    FrozenModule, Globals, GlobalsBuilder, LibraryExtension, Module,
};
use starlark::eval::{Evaluator, FileLoader};
use starlark::values::Value;
use starlark::syntax::{AstModule, Dialect};
use std::cell::RefCell;
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
    .with(bazel_native_rule_globals)
    .build()
}

/// Bazel's native rules as BUILD GLOBALS (no `load()` needed — `cc_library` is a builtin in real
/// BUILD files; TF uses it bare everywhere). Aliased from razel's native cc rules; `cc_test` is
/// loading-grade (the binary backend).
pub(crate) fn bazel_native_rule_globals(b: &mut GlobalsBuilder) {
    let native_cc = GlobalsBuilder::standard().with(crate::native_cc::cc_rules).build();
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
    let native_py = GlobalsBuilder::standard().with(crate::py_rules::py_rules).build();
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
        let cc = GlobalsBuilder::standard().with(crate::native_cc::cc_rules).build();
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
    for ns in ["cc_common", "coverage_common", "testing", "apple_common", "java_common", "proto_common", "platform_common", "proto_common_do_not_use", "py_internal", "android_common", "ApkInfo", "AndroidIdeInfo"] {
        b.set(ns, crate::engine::Absorb);
    }
    // The absorber itself, for razel's HOST .bzl files (host-repos/) to bind symbols with.
    b.set("razel_host_absorb", crate::engine::Absorb);
    razel_host_helpers(b);
}

/// Host-.bzl helper globals: `razel_host_absorb_with({...})` builds an absorber whose NAMED
/// members are real values (the per-member override seam).
#[starlark::starlark_module]
fn razel_host_helpers(b: &mut GlobalsBuilder) {
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
    } else if !label.contains(':') && label.ends_with(".bzl") {
        // Legacy bare-filename form: `load("f.bzl", …)` = same-package relative path.
        let pkg = current_pkg.unwrap_or_default();
        Ok(root.join(pkg).join(label))
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
    session: &'a Session,
    /// The (repo, pkg) of the external module currently being loaded (`None` frames are
    /// workspace modules). Loads written INSIDE `@repo` resolve repo-relatively — Bazel label
    /// semantics (`//pkg:f.bzl` in rules_cc means rules_cc's own pkg, not the workspace's).
    load_ctx: RefCell<Vec<Option<(String, String)>>>,
}

/// `@repo//pkg:file` → `(repo, pkg)`.
fn parse_external(path: &str) -> Option<(String, String)> {
    let rest = path.strip_prefix('@')?;
    let (repo, pkgfile) = rest.split_once("//")?;
    let (pkg, _file) = pkgfile.split_once(':')?;
    Some((repo.to_string(), pkg.to_string()))
}

impl BzlLoader<'_> {
    /// Rewrite a load label written inside an external module to its canonical `@repo//…` form;
    /// workspace-context labels pass through unchanged.
    fn canonicalize(&self, path: &str) -> String {
        let Some(Some((repo, pkg))) = self.load_ctx.borrow().last().cloned() else {
            return path.to_string();
        };
        // Main-repo modules carry `repo == ""` — relative loads resolve against the MODULE's
        // package (not the BUILD package that triggered the load).
        if let Some(rest) = path.strip_prefix("//") {
            if repo.is_empty() { path.to_string() } else { format!("@{repo}//{rest}") }
        } else if let Some(file) = path.strip_prefix(':') {
            if repo.is_empty() {
                format!("//{pkg}:{file}")
            } else {
                format!("@{repo}//{pkg}:{file}")
            }
        } else if !path.starts_with('@') && !path.contains(':') && path.ends_with(".bzl") {
            // Legacy bare-filename form (`load("f.bzl", …)`) = same-package, like `:f.bzl`.
            if repo.is_empty() {
                format!("//{pkg}:{path}")
            } else {
                format!("@{repo}//{pkg}:{path}")
            }
        } else {
            path.to_string()
        }
    }
}

impl FileLoader for BzlLoader<'_> {
    fn load(&self, path: &str) -> starlark::Result<FrozenModule> {
        let path = &self.canonicalize(path);
        if let Some(m) = self.session.bzl_cache.borrow().get(path) {
            return Ok(m.clone());
        }
        let err = |m: String| starlark::Error::new_other(anyhow::anyhow!(m));
        // Resolution order: razel's HOST repos (compiled in — `@bazel_tools` etc. are
        // host-reserved, as in Bazel) → a REAL vendored external file (D4: upstream `.bzl`
        // beats razel's synthetic shim) → ruleset shim → workspace file.
        let host = crate::host::host_bzl(path);
        let real_external = if host.is_some() {
            None
        } else {
            self.session
                .global
                .external_base
                .as_deref()
                .and_then(|base| external_bzl_path(base, path))
        };
        let ctx = if host.is_some() || real_external.is_some() {
            parse_external(path)
        } else if let Some(rest) = path.strip_prefix("//") {
            // A workspace .bzl: its own package is the context for ITS relative loads.
            rest.split_once(':').map(|(pkg, _)| (String::new(), pkg.to_string()))
        } else {
            None
        };
        let src = if let Some(content) = host {
            content.to_string()
        } else if let Some(p) = real_external {
            std::fs::read_to_string(&p)
                .map_err(|e| err(format!("cannot read {}: {e}", p.display())))?
        } else if let Some(rs) = self.rulesets.iter().find(|r| path.starts_with(r.prefix)) {
            return Ok(rs.module.clone());
        } else {
            let root = self
                .session
                .workspace
                .clone()
                .ok_or_else(|| err(format!("load(\"{path}\") needs workspace mode")))?;
            let cur = self.session.current_pkg.borrow().clone();
            let p = resolve_bzl(&root, path, cur.as_deref()).map_err(err)?;
            std::fs::read_to_string(&p)
                .map_err(|e| err(format!("cannot read {}: {e}", p.display())))?
        };

        // The nested eval runs with this module's repo context on the stack (popped even on error).
        self.session.current_bzl_repo.borrow_mut().push(ctx.clone());
        self.load_ctx.borrow_mut().push(ctx);
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
        });
        self.load_ctx.borrow_mut().pop();
        self.session.current_bzl_repo.borrow_mut().pop();
        let frozen = frozen?;
        self.session
            .bzl_cache
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
    eval_build_src_in(session, name, src, None, true)
}

/// [`eval_build_src`] with a repo context: an EXTERNAL package's BUILD resolves its loads and
/// `Label()`s against its own repo (`Some((repo, pkg))` — Bazel label semantics).
pub(crate) fn eval_build_src_in(
    session: &Session,
    name: &str,
    src: &str,
    repo_ctx: Option<(String, String)>,
    drive_all: bool,
) -> Result<(), String> {
    let rulesets = ruleset_modules(session.global.cc_toolchain)?;
    let globals = build_globals();
    let loader = BzlLoader {
        rulesets: &rulesets,
        globals: &globals,
        session,
        load_ctx: RefCell::new(vec![repo_ctx.clone()]),
    };
    session.current_bzl_repo.borrow_mut().push(repo_ctx.clone());
    let result = eval_build_src_inner(session, name, src, &loader, &globals, drive_all);
    session.current_bzl_repo.borrow_mut().pop();
    return result;
}

fn eval_build_src_inner(
    session: &Session,
    name: &str,
    src: &str,
    loader: &BzlLoader<'_>,
    globals: &Globals,
    drive_all: bool,
) -> Result<(), String> {
    let ast = match session.ast_cache.borrow_mut().remove(name) {
        Some(ast) => ast,
        None => AstModule::parse(name, src.to_owned(), &Dialect::Extended)
            .map_err(|e| format!("{e}"))?,
    };
    Module::with_temp_heap(|module| {
        crate::dialect::install_decl_store(&module);
        {
            let mut eval = Evaluator::new(&module);
            eval.set_loader(loader);
            eval.extra = Some(session); // builtins read the Session via `session(eval)`
            eval.eval_module(ast, globals).map_err(|e| format!("{e}"))?;
        }
        // E0 phase 2: analyze the recorded declarations, demand-driven (forward refs resolve).
        {
            let mut eval = Evaluator::new(&module);
            eval.set_loader(loader);
            eval.extra = Some(session);
            crate::dialect::drive_decls(&mut eval, drive_all).map_err(|e| format!("{e}"))?;
        }
        // Layer 0: stash the captured provider instances as plain dict/list/tuple values,
        // unroot the (unfreezable) decl store, freeze the module, harvest into the Session.
        crate::dialect::stash_captured_for_freeze(&module, session).map_err(|e| format!("{e}"))?;
        let fm = module.freeze().map_err(|e| format!("freeze: {e:?}"))?;
        if let Ok(owned) = fm.get(crate::dialect::CAPTURED_VAR) {
            index_harvest(&owned, &session.cross_captured, &session.cross_index);
        }
        if let Ok(owned) = fm.get(crate::dialect::DEFERRED_VAR) {
            index_harvest(&owned, &session.deferred_decls, &session.deferred_index);
        }
        Ok(())
    })
}

/// Push a harvest dict and index its label keys → owner position (O(1) demand lookups).
fn index_harvest(
    owned: &starlark::values::OwnedFrozenValue,
    store: &crate::state::SyncCell<Vec<starlark::values::OwnedFrozenValue>>,
    index: &crate::state::SyncCell<std::collections::HashMap<String, usize>>,
) {
    let idx = store.borrow().len();
    {
        let v = owned.value();
        if let Some(d) = starlark::values::dict::DictRef::from_value(v) {
            let mut ix = index.borrow_mut();
            for (k, _) in d.iter() {
                if let Some(k) = k.unpack_str() {
                    ix.insert(k.to_string(), idx);
                }
            }
        }
    }
    store.borrow_mut().push(owned.clone());
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
    load_package_mode(sess, pkg, false)
}

/// [`load_package`] for the ENTRY package: drives every declaration (tests assert on the whole
/// package); dependency loads analyze on demand only.
pub(crate) fn load_package_entry(sess: &Session, pkg: &str) -> Result<(), String> {
    load_package_mode(sess, pkg, true)
}

fn load_package_mode(sess: &Session, pkg: &str, drive_all: bool) -> Result<(), String> {
    if sess.loaded.borrow().contains(pkg) {
        return Ok(());
    }
    sess.loaded.borrow_mut().insert(pkg.to_string());
    // Host-materialized packages (Bazel built-ins) take precedence over vendoring.
    if let Some(src) = crate::host::host_build(pkg) {
        let repo_ctx = pkg.strip_prefix('@').and_then(|rest| {
            rest.split_once("//").map(|(r, sub)| (r.to_string(), sub.to_string()))
        });
        let prev = sess.current_pkg.borrow_mut().replace(pkg.to_string());
        let res = eval_build_src_in(sess, &format!("{pkg}/BUILD"), src, repo_ctx, drive_all);
        *sess.current_pkg.borrow_mut() = prev;
        if let Err(e) = res {
            sess.loaded.borrow_mut().remove(pkg);
            return Err(e.to_string());
        }
        return Ok(());
    }
    // External package (`@repo//pkg`): its BUILD lives under the vendored repo's root.
    let pkg_dir = if let Some(rest) = pkg.strip_prefix('@') {
        let (repo, sub) = rest.split_once("//").ok_or_else(|| format!("bad package `{pkg}`"))?;
        let base = sess
            .global
            .external_base
            .clone()
            .ok_or_else(|| format!("external package `{pkg}` needs an external base"))?;
        [repo.to_string(), repo.replace('_', "-")]
            .iter()
            .map(|dir| base.join(dir))
            .find(|p| p.exists())
            .ok_or_else(|| format!("external repo for `{pkg}` not vendored"))?
            .join(sub)
    } else {
        sess.workspace
            .clone()
            .ok_or("load_package called outside workspace mode")?
            .join(pkg)
    };
    // Pre-parsed AST present? Skip BOTH the read and the parse (the parallel pre-pass).
    let prepared = sess.ast_cache.borrow().contains_key(&format!("{pkg}/BUILD"));
    let src = if prepared {
        String::new()
    } else {
        let build_path = ["BUILD", "BUILD.bazel"]
            .iter()
            .map(|f| pkg_dir.join(f))
            .find(|p| p.exists())
            .ok_or_else(|| format!("no BUILD in package `{pkg}` ({})", pkg_dir.display()))?;
        std::fs::read_to_string(&build_path).map_err(|e| e.to_string())?
    };

    // Short borrows around the nested eval (the [R1] discipline): set current_pkg, drop the
    // borrow, recurse, then restore — never hold a Session borrow across `eval_build_src`.
    let repo_ctx = pkg.strip_prefix('@').and_then(|rest| {
        rest.split_once("//").map(|(r, sub)| (r.to_string(), sub.to_string()))
    });
    let prev = sess.current_pkg.borrow_mut().replace(pkg.to_string());
    let res = eval_build_src_in(sess, &format!("{pkg}/BUILD"), &src, repo_ctx, drive_all);
    *sess.current_pkg.borrow_mut() = prev;
    // A failed load must not poison the loaded-set (the guard would silently no-op retries
    // and every later condition/dep in the package would report "not declared").
    if res.is_err() {
        sess.loaded.borrow_mut().remove(pkg);
    }
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
    load_package_entry(&session, &top_pkg)?;
    Ok(session.take_targets())
}

/// The TREE-LOAD driver (L6 coverage metric): load every given package in ONE session
/// (shared .bzl cache / config space — the realistic shape), returning per-package results.
/// A package failure doesn't stop the sweep; the report is the point.
pub fn load_tree_report(
    root: &Path,
    flags: GlobalFlags,
    packages: &[String],
) -> Vec<(String, Result<(), String>)> {
    load_tree_report_prepared(root, flags, packages, Vec::new())
}

/// `load_tree_report` with PRE-PARSED BUILD ASTs (from [`prepare_build_asts`]): the load+parse
/// half runs in parallel; the eval half stays sequential and consumes the cache.
pub fn load_tree_report_prepared(
    root: &Path,
    flags: GlobalFlags,
    packages: &[String],
    asts: Vec<(String, starlark::syntax::AstModule)>,
) -> Vec<(String, Result<(), String>)> {
    let session = Session::new(Some(root.to_path_buf()), flags);
    session.ast_cache.borrow_mut().extend(asts);
    packages
        .iter()
        .map(|pkg| (pkg.clone(), load_package_entry(&session, pkg)))
        .collect()
}

/// PARALLEL read+parse of the packages' BUILD files (pure — no Session state): the
/// load+parse / execute split. Returns `({pkg}/BUILD, ast)` pairs; unparseable files are
/// skipped (the sequential path re-reads and surfaces the error properly).
pub fn prepare_build_asts(
    root: &Path,
    packages: &[String],
    threads: usize,
) -> Vec<(String, starlark::syntax::AstModule)> {
    let n = threads.max(1);
    let chunks: Vec<&[String]> = packages.chunks(packages.len().div_ceil(n)).collect();
    std::thread::scope(|scope| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                scope.spawn(move || {
                    let mut out = Vec::new();
                    for pkg in chunk {
                        let Some(path) = ["BUILD", "BUILD.bazel"]
                            .iter()
                            .map(|f| root.join(pkg).join(f))
                            .find(|p| p.exists())
                        else {
                            continue;
                        };
                        let Ok(src) = std::fs::read_to_string(&path) else { continue };
                        let name = format!("{pkg}/BUILD");
                        if let Ok(ast) = AstModule::parse(&name, src, &Dialect::Extended) {
                            out.push((name, ast));
                        }
                    }
                    out
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    })
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
        crate::dialect::install_decl_store(&module);
        {
            let mut eval = Evaluator::new(&module);
            eval.extra = Some(&session);
            eval.eval_module(ast, &globals).map_err(|e| format!("{e}"))?;
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
        // razelV3: conditions must be DECLARED config_settings (the stub tolerated unknowns);
        // under the default config (fastbuild) the non-matching :dbg falls through to default.
        let src = r#"
config_setting(name = "dbg", values = {"compilation_mode": "dbg"})

def _impl(ctx):
    flags = select({"//conditions:default": ["-O2"], ":dbg": ["-g"]})
    ctx.actions.run(executable = "cc", outputs = [ctx.attr.name], inputs = [], arguments = flags)
    return [DefaultInfo(files = [ctx.attr.name])]

thing = rule(implementation = _impl, attrs = {})
thing(name = "x")
"#;
        let targets = analyze_starlark("BUILD", src).unwrap();
        let x = targets.iter().find(|t| t.name.ends_with("x")).unwrap();
        assert_eq!(x.actions[0].mnemonic, "cc");
        assert!(x.actions[0].argv.contains(&"-O2".to_string()), "default branch picked");
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
    fn forward_dep_reference_analyzes() {
        // E0: bin declared before its dep → the demand-driven pass analyzes :math first.
        // (Inverts the pre-E0 pin that forward refs must error — RazelV3Plan §2.)
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
        let targets = analyze_starlark("BUILD", src).unwrap();
        assert!(targets.iter().any(|t| t.name.ends_with("app")));
    }
}
