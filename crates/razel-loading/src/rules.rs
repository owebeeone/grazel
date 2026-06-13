//! Starlark-defined rules + analysis (Phase 3): `rule(implementation, attrs)` returns a
//! callable custom value; instantiating it runs the rule **implementation** with a `ctx`
//! (Bazel dialect: `ctx.attr.*`, `ctx.label`, `ctx.actions.declare_file/run/write`) and
//! captures the registered actions (inputs/outputs) and `DefaultInfo` — the target
//! **analyzes**. Plus `select()` (host-config-lite) and `DefaultInfo`.
//!
//! Analysis runs in the **same eval scope** as instantiation (the impl `Value` never
//! escapes the heap) — sidestepping module freezing. Tier-2.5 simplification; a two-phase
//! freeze model comes when caching / cross-target dep-providers demand it.

use crate::dialect::rule_globals;
use crate::engine::{
    attr_members, config_common_members, config_members, native_members, razel_build_members,
};
use crate::shims::{auto_config_module, rules_cc_module, rules_java_module, rules_skylib_module};
use crate::state::{AnalyzedTarget, CcToolchainMode, GlobalFlags, Session, canon_label, pkg_of};
use starlark::environment::{FrozenModule, Globals, GlobalsBuilder, LibraryExtension, Module};
use starlark::eval::{Evaluator, FileLoader};
use starlark::syntax::{AstModule, Dialect};
use starlark::values::Value;
use std::cell::RefCell;
use std::path::{Path, PathBuf};

pub(crate) fn build_globals() -> Globals {
    builder_base().build()
}

fn builder_base() -> GlobalsBuilder {
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
    .with(crate::fetch::repo_rule_globals)
    .with(autoload_stub_globals)
}

/// Bazel-AUTOLOADED BUILD globals razel models as RECORD-ONLY placeholders (fetch R4):
/// upstream BUILDs (flatbuffers et al.) declare java targets bare; nothing in the TF cone
/// consumes them, but the package must LOAD. The target exists (deps resolve to an empty
/// placeholder); real java analysis is the java rung's work, via the @rules_java shim.
#[starlark::starlark_module]
fn autoload_stub_globals(b: &mut GlobalsBuilder) {
    fn java_library<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: starlark::collections::SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        crate::deps::record_named(crate::state::session(eval), &name);
        Ok(starlark::values::none::NoneType)
    }

    fn java_binary<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: starlark::collections::SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        crate::deps::record_named(crate::state::session(eval), &name);
        Ok(starlark::values::none::NoneType)
    }

    fn java_test<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: starlark::collections::SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        crate::deps::record_named(crate::state::session(eval), &name);
        Ok(starlark::values::none::NoneType)
    }

    /// Bazel's `fail(*args, msg=None, attr=None, sep=" ")` — the deprecated `attr` keyword
    /// prefixes the message with the attribute name; starlark-rust's builtin rejects it.
    /// SHADOWS the stdlib global (GlobalsBuilder is a map; later sets win — the no-fork
    /// dialect lever, round 44).
    fn fail<'v>(
        #[starlark(args)] args: starlark::values::tuple::UnpackTuple<Value<'v>>,
        #[starlark(require = named)] msg: Option<Value<'v>>,
        #[starlark(require = named)] attr: Option<String>,
        #[starlark(require = named)] sep: Option<String>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        let sep = sep.unwrap_or_else(|| " ".to_string());
        let mut parts: Vec<String> = Vec::new();
        if let Some(m) = msg {
            parts.push(
                m.unpack_str()
                    .map(String::from)
                    .unwrap_or_else(|| m.to_string()),
            );
        }
        parts.extend(args.items.iter().map(|a| {
            a.unpack_str()
                .map(String::from)
                .unwrap_or_else(|| a.to_string())
        }));
        let body = parts.join(&sep);
        match attr {
            Some(a) => Err(anyhow::anyhow!("fail: attribute {a}: {body}")),
            None => Err(anyhow::anyhow!("fail: {body}")),
        }
    }

    /// `toolchain()` declarations (round 43 — grpc registers clang-cl toolchains):
    /// record-only; real toolchain resolution is L3 surface.
    fn toolchain<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: starlark::collections::SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        crate::deps::record_named(crate::state::session(eval), &name);
        Ok(starlark::values::none::NoneType)
    }

    /// The platform-family DECLARE rules (round 43 — grpc declares clang-cl platforms in
    /// its BUILD): record-only placeholders; real platform resolution is L4 surface.
    fn platform<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: starlark::collections::SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        crate::deps::record_named(crate::state::session(eval), &name);
        Ok(starlark::values::none::NoneType)
    }

    fn constraint_setting<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: starlark::collections::SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        crate::deps::record_named(crate::state::session(eval), &name);
        Ok(starlark::values::none::NoneType)
    }

    /// A `constraint_value` IS a selectable condition: it registers a config spec whose
    /// single constraint is ITSELF — `spec.matches` answers via `host_constraint_matches`
    /// (TF selects on `@platforms//os:*` directly; round 43).
    fn constraint_value<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: starlark::collections::SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<starlark::values::none::NoneType> {
        let sess = crate::state::session(eval);
        crate::deps::record_named(sess, &name);
        let canon = crate::state::canon_label(sess, &name);
        let spec = crate::state::ConfigSpec {
            constraint_values: vec![canon.clone()],
            ..Default::default()
        };
        sess.config_specs.borrow_mut().insert(canon, spec);
        Ok(starlark::values::none::NoneType)
    }
}

/// Evaluate a WORKSPACE source (fetch R1, RazelFetchPlan §3): the normal BUILD surface plus
/// the WORKSPACE-only globals (the `repository_rule` recorder, `workspace()`,
/// `register_*` no-ops). Declarations record but never drive; no freeze/harvest — the
/// Session's `repo_specs` are the product.
pub(crate) fn eval_workspace_src(session: &Session, name: &str, src: &str) -> Result<(), String> {
    let rulesets = ruleset_modules(session.global.cc_toolchain)?;
    let globals = builder_base().with(crate::fetch::workspace_globals).build();
    let loader = BzlLoader {
        rulesets: &rulesets,
        globals: &globals,
        session,
        load_ctx: RefCell::new(vec![None]),
    };
    session.bzl_repo_push(None);
    let res = Module::with_temp_heap(|module| -> Result<(), String> {
        crate::dialect::install_decl_store(&module);
        let ast = AstModule::parse(name, detab_leading(src).into_owned(), &Dialect::Extended)
            .map_err(|e| format!("{e}"))?;
        let mut eval = Evaluator::new(&module);
        eval.set_loader(&loader);
        eval.extra = Some(session);
        eval.eval_module(ast, &globals)
            .map_err(|e| format!("{e}"))?;
        Ok(())
    });
    session.bzl_repo_pop();
    res
}

/// Bazel's native rules as BUILD GLOBALS (no `load()` needed — `cc_library` is a builtin in real
/// BUILD files; TF uses it bare everywhere). Aliased from razel's native cc rules; `cc_test` is
/// loading-grade (the binary backend).
pub(crate) fn bazel_native_rule_globals(b: &mut GlobalsBuilder) {
    let native_cc = GlobalsBuilder::standard()
        .with(crate::native_cc::cc_rules)
        .build();
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
    b.set("cc_libc_top_alias", get("native_cc_libc_top_alias"));
    let native_py = GlobalsBuilder::standard()
        .with(crate::py_rules::py_rules)
        .build();
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
        // skylib `versions.check` consults this (fetch R1's WORKSPACE chain does too).
        // Bazel-7-era claim, consistent with the all-True @bazel_features posture.
        nb.set("bazel_version", "7.4.5");
        // WORKSPACE-macro surface (fetch R1): registration no-ops, namespaced form.
        let ws_g = GlobalsBuilder::standard()
            .with(crate::fetch::workspace_globals)
            .build();
        for name in [
            "register_toolchains",
            "register_execution_platforms",
            "bind",
        ] {
            if let Some((_, v)) = ws_g.iter().find(|(n, _)| *n == name) {
                nb.set(name, v);
            }
        }
        // Platform-family declare rules (round 43) — BUILD globals AND native.* forms.
        let stub_g = GlobalsBuilder::standard()
            .with(autoload_stub_globals)
            .build();
        for name in [
            "platform",
            "constraint_setting",
            "constraint_value",
            "toolchain",
            "java_library",
        ] {
            if let Some((_, v)) = stub_g.iter().find(|(n, _)| *n == name) {
                nb.set(name, v);
            }
        }
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
        let cc = GlobalsBuilder::standard()
            .with(crate::native_cc::cc_rules)
            .build();
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
    for ns in [
        "cc_common",
        "coverage_common",
        "testing",
        "apple_common",
        "java_common",
        "proto_common",
        "platform_common",
        "proto_common_do_not_use",
        "py_internal",
        "android_common",
        "ApkInfo",
        "AndroidIdeInfo",
    ] {
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

/// Bazel accepts TAB indentation (a tab advances to the next multiple-of-8 column);
/// starlark-rust rejects tabs outright. Expand each line's LEADING whitespace run by the
/// Bazel column rule — tabs inside strings/comments after code starts are untouched.
/// Borrow-through when tab-free (the overwhelmingly common case).
pub(crate) fn detab_leading(src: &str) -> std::borrow::Cow<'_, str> {
    if !src.contains('\t') {
        return std::borrow::Cow::Borrowed(src);
    }
    let mut out = String::with_capacity(src.len() + 64);
    for line in src.split_inclusive('\n') {
        let mut col = 0usize;
        let mut rest = line.len();
        let mut had_tab = false;
        for (i, c) in line.char_indices() {
            match c {
                ' ' => col += 1,
                '\t' => {
                    col += 8 - (col % 8);
                    had_tab = true;
                }
                _ => {
                    rest = i;
                    break;
                }
            }
        }
        if had_tab {
            out.extend(std::iter::repeat(' ').take(col));
            out.push_str(&line[rest.min(line.len())..]);
        } else {
            out.push_str(line);
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Resolve a project `.bzl` load to a file under `root`. `//pkg:f.bzl` → `root/pkg/f.bzl`;
/// `:f.bzl` → `root/<current pkg>/f.bzl`. (`@repo` loads go through [`external_bzl_path`] /
/// the synthetic rulesets, not here.)
pub(crate) fn resolve_bzl(
    root: &Path,
    label: &str,
    current_pkg: Option<&str>,
) -> Result<PathBuf, String> {
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

/// Resolve a vendored/fetched external load `@repo//pkg:file` to a real file (D4 + fetch
/// R4): candidates fold over [`GlobalFlags::external_repo_dirs`] — hand-vendored first
/// (with the `_`/`-` name tolerance), the fetched root second. `None` if not an
/// `@repo//pkg:file` label or no such file. A real external file takes precedence over
/// razel's synthetic shim, so configured corpora run REAL upstream.
pub(crate) fn external_bzl_path(global: &GlobalFlags, label: &str) -> Option<PathBuf> {
    let rest = label.strip_prefix('@')?;
    let (repo, pkgfile) = rest.split_once("//")?;
    let (pkg, file) = pkgfile.split_once(':')?;
    global
        .external_repo_dirs(repo)
        .into_iter()
        .map(|dir| dir.join(pkg).join(file))
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
            if repo.is_empty() {
                path.to_string()
            } else {
                format!("@{repo}//{rest}")
            }
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
        // `@//pkg:f.bzl` / `@@//pkg:f.bzl` are the MAIN-repo absolute forms (WORKSPACE
        // files use them) — identical to `//pkg:f.bzl` here.
        let path = if let Some(rest) = path.strip_prefix("@@//") {
            format!("//{rest}")
        } else if let Some(rest) = path.strip_prefix("@//") {
            format!("//{rest}")
        } else {
            path.to_string()
        };
        let path = &self.canonicalize(&path);
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
            external_bzl_path(&self.session.global, path)
        };
        let ctx = if host.is_some() || real_external.is_some() {
            parse_external(path)
        } else if let Some(rest) = path.strip_prefix("//") {
            // A workspace .bzl: its own package is the context for ITS relative loads.
            rest.split_once(':')
                .map(|(pkg, _)| (String::new(), pkg.to_string()))
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
            let cur = self.session.current_pkg();
            let p = resolve_bzl(&root, path, cur.as_deref()).map_err(err)?;
            std::fs::read_to_string(&p)
                .map_err(|e| err(format!("cannot read {}: {e}", p.display())))?
        };

        // P4a single-flight: ONE eval per module per Session even under the pool — provider
        // identities are POINTER identities, so a concurrent double-eval mints two `MyInfo`s
        // and cross-package `dep[P]` ptr-eq breaks. Losers wait, then read the owner's module.
        if let crate::state::BzlBegin::Ready = crate::state::begin_bzl_load(self.session, path)
            && let Some(m) = self.session.bzl_cache.borrow().get(path)
        {
            return Ok(m.clone());
        }
        // The nested eval runs with this module's repo context on the stack (popped even on error).
        self.session.bzl_repo_push(ctx.clone());
        self.load_ctx.borrow_mut().push(ctx);
        let frozen = Module::with_temp_heap(|module| -> starlark::Result<FrozenModule> {
            let ast = AstModule::parse(path, detab_leading(&src).into_owned(), &Dialect::Extended)?;
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
        self.session.bzl_repo_pop();
        let frozen = match frozen {
            Ok(f) => f,
            Err(e) => {
                crate::state::finish_bzl_load(self.session, path, false);
                return Err(e);
            }
        };
        // Cache BEFORE Done — Ready readers consult the cache.
        self.session
            .bzl_cache
            .borrow_mut()
            .insert(path.to_string(), frozen.clone());
        crate::state::finish_bzl_load(self.session, path, true);
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
        // S3a: js/ts via the ECOSYSTEM surface (no-competing-surfaces rule — aspect's
        // rules are the incumbents; razel implements a faithful subset, one module
        // serving both prefixes).
        Ruleset {
            prefix: "@aspect_rules_js//",
            module: crate::js_rules::module()?,
        },
        Ruleset {
            prefix: "@aspect_rules_ts//",
            module: crate::js_rules::module()?,
        },
    ])
}

/// Evaluate one BUILD source with the ruleset loaders + the rule globals.
/// Targets it instantiates are recorded into STATE/RESULTS (re-entrant: a nested
/// cross-package load appends, never clears).
pub(crate) fn eval_build_src(session: &Session, name: &str, src: &str) -> Result<(), String> {
    eval_build_src_in(session, name, src, None, true).map_err(|e| e.msg)
}

/// [`eval_build_src`] with a repo context: an EXTERNAL package's BUILD resolves its loads and
/// `Label()`s against its own repo (`Some((repo, pkg))` — Bazel label semantics).
pub(crate) fn eval_build_src_in(
    session: &Session,
    name: &str,
    src: &str,
    repo_ctx: Option<(String, String)>,
    drive_all: bool,
) -> Result<(), LoadErr> {
    let rulesets = ruleset_modules(session.global.cc_toolchain).map_err(LoadErr::declare)?;
    let globals = build_globals();
    let loader = BzlLoader {
        rulesets: &rulesets,
        globals: &globals,
        session,
        load_ctx: RefCell::new(vec![repo_ctx.clone()]),
    };
    session.bzl_repo_push(repo_ctx.clone());
    let result = eval_build_src_inner(session, name, src, &loader, &globals, drive_all);
    session.bzl_repo_pop();
    return result;
}

fn eval_build_src_inner(
    session: &Session,
    name: &str,
    src: &str,
    loader: &BzlLoader<'_>,
    globals: &Globals,
    drive_all: bool,
) -> Result<(), LoadErr> {
    let ast = match session.ast_cache.borrow_mut().remove(name) {
        Some(ast) => ast,
        None => AstModule::parse(name, detab_leading(src).into_owned(), &Dialect::Extended)
            .map_err(|e| LoadErr::declare(format!("{e}")))?,
    };
    Module::with_temp_heap(|module| {
        crate::dialect::install_decl_store(&module);
        {
            let mut eval = Evaluator::new(&module);
            eval.set_loader(loader);
            eval.extra = Some(session); // builtins read the Session via `session(eval)`
            // DECLARE phase: an error here is Bazel's "package in error" (cacheable).
            eval.eval_module(ast, globals)
                .map_err(|e| LoadErr::declare(format!("{e}")))?;
        }
        // E0 phase 2: analyze the recorded declarations, demand-driven (forward refs resolve).
        // ANALYSIS phase: failures are retryable — the declarations are fine.
        let drive_res = {
            let mut eval = Evaluator::new(&module);
            eval.set_loader(loader);
            eval.extra = Some(session);
            crate::dialect::drive_decls(&mut eval, drive_all)
        };
        if let Err(e) = drive_res {
            let msg = format!("{e}");
            salvage_captured_after_analysis_failure(session, module);
            return Err(LoadErr {
                msg,
                pkg_in_error: false,
            });
        }
        // Layer 0: stash the captured provider instances as plain dict/list/tuple values,
        // unroot the (unfreezable) decl store, freeze the module, harvest into the Session.
        // Conservative: freeze/harvest failures stay retryable.
        crate::dialect::stash_captured_for_freeze(&module, session).map_err(|e| LoadErr {
            msg: format!("{e}"),
            pkg_in_error: false,
        })?;
        let fm = module.freeze().map_err(|e| LoadErr {
            msg: format!("freeze: {e:?}"),
            pkg_in_error: false,
        })?;
        if let Ok(owned) = fm.get(crate::dialect::CAPTURED_VAR) {
            index_harvest(&owned, &session.cross_captured, &session.cross_index);
        }
        if let Ok(owned) = fm.get(crate::dialect::DEFERRED_VAR) {
            index_harvest(&owned, &session.deferred_decls, &session.deferred_index);
        }
        Ok(())
    })
}

fn salvage_captured_after_analysis_failure<'v>(session: &Session, module: Module<'v>) {
    let Ok(has_captures) = crate::dialect::stash_captured_only_for_freeze(&module) else {
        return;
    };
    if !has_captures {
        return;
    }
    let Ok(fm) = module.freeze() else {
        return;
    };
    if let Ok(owned) = fm.get(crate::dialect::CAPTURED_VAR) {
        index_harvest(&owned, &session.cross_captured, &session.cross_index);
    }
}

/// Push a harvest dict and index its label keys → owner position (O(1) demand lookups).
/// P4a: the position is taken from the push UNDER ONE LOCK — a len-then-push across two
/// acquisitions let concurrent harvesters claim the same slot and index the WRONG dict
/// (labels then "miss" while present). Index-after-push means a racing reader can miss a
/// just-harvested label (benign — callers fall back to demand analysis), never mis-map.
fn index_harvest(
    owned: &starlark::values::OwnedFrozenValue,
    store: &crate::state::SyncCell<Vec<starlark::values::OwnedFrozenValue>>,
    index: &crate::state::SyncCell<std::collections::HashMap<String, usize>>,
) {
    let idx = {
        let mut s = store.borrow_mut();
        s.push(owned.clone());
        s.len() - 1
    };
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

/// A load failure, PHASE-TAGGED (round 29): `pkg_in_error` = the failure precedes or is in
/// the DECLARE phase (missing repo/BUILD, read error, eval_module error) — Bazel's "package
/// in error", cached so consumers don't re-evaluate. Analysis-phase (drive) failures stay
/// retryable: the declarations are fine (cross_package_providers' contract).
pub(crate) struct LoadErr {
    pub(crate) msg: String,
    pub(crate) pkg_in_error: bool,
}

impl LoadErr {
    fn declare(msg: impl Into<String>) -> Self {
        LoadErr {
            msg: msg.into(),
            pkg_in_error: true,
        }
    }
}

fn load_package_mode(sess: &Session, pkg: &str, drive_all: bool) -> Result<(), String> {
    match crate::state::acquire_resource(sess, &crate::state::ResKey::Pkg(pkg.to_string())) {
        crate::state::Acquire::Ready
        | crate::state::Acquire::Reentry
        | crate::state::Acquire::CycleProceed => return Ok(()),
        // Package-in-error: the load ran once; serve the cached error (loud, no re-eval).
        crate::state::Acquire::Failed(e) => return Err(e),
        crate::state::Acquire::Own => {}
    }
    // EVERY exit after an owned begin must finish (P4a): an early `?` between begin and
    // finish leaked a dead InFlight entry — sequentially masked by re-entry semantics, under
    // the pool every later waiter parked into the 20s-timeout livelock ("deadlock" at TF
    // scale: each unvendored-repo demand cost every waiter a 20s quantum, forever).
    let res = load_package_body(sess, pkg, drive_all);
    let outcome = match &res {
        Ok(()) => crate::state::FinishOutcome::Ok,
        Err(e) if e.pkg_in_error => crate::state::FinishOutcome::FailCached(e.msg.clone()),
        // Analysis failure must not poison the loaded-set (the guard would silently no-op
        // retries and every later condition/dep would report "not declared").
        Err(_) => crate::state::FinishOutcome::FailRetry,
    };
    crate::state::finish_pkg_load(sess, pkg, outcome);
    res.map_err(|e| e.msg)
}

fn load_package_body(sess: &Session, pkg: &str, drive_all: bool) -> Result<(), LoadErr> {
    // Host-materialized packages (Bazel built-ins) take precedence over vendoring.
    if let Some(src) = crate::host::host_build(pkg) {
        let repo_ctx = pkg.strip_prefix('@').and_then(|rest| {
            rest.split_once("//")
                .map(|(r, sub)| (r.to_string(), sub.to_string()))
        });
        let prev = sess.set_current_pkg(Some(pkg.to_string()));
        let res = eval_build_src_in(sess, &format!("{pkg}/BUILD"), src, repo_ctx, drive_all);
        sess.set_current_pkg(prev);
        return res;
    }
    // External package (`@repo//pkg`): its BUILD lives under the vendored repo's root.
    // Failures up to the eval are PRE-EVAL — the package is in error (cacheable).
    let pkg_dir = if let Some(rest) = pkg.strip_prefix('@') {
        let (repo, sub) = rest
            .split_once("//")
            .ok_or_else(|| LoadErr::declare(format!("bad package `{pkg}`")))?;
        sess.global
            .external_repo_dir(repo)
            .ok_or_else(|| LoadErr::declare(format!("external repo for `{pkg}` not vendored")))?
            .join(sub)
    } else {
        sess.workspace
            .clone()
            .ok_or_else(|| LoadErr::declare("load_package called outside workspace mode"))?
            .join(pkg)
    };
    // Resolve the package file UNCONDITIONALLY (cheap stat probes): the E-mode XOR and
    // boundary guard must hold even when the parallel pre-pass already cached the AST.
    let build_path = crate::workspace::resolve_build_file(&pkg_dir, sess.global.strict_bazel)
        .map_err(LoadErr::declare)?
        .ok_or_else(|| {
            LoadErr::declare(format!(
                "no BUILD in package `{pkg}` ({})",
                pkg_dir.display()
            ))
        })?;
    // E-package in the MAIN repo: the boundary guard (§3c rule 2 — warning during
    // S1 only; a hard ERROR since S3d: boundary divergence must not be warnable).
    if build_path.file_name().is_some_and(|f| f == "BUILD.razel") && !pkg.starts_with('@') {
        if let Some(root) = sess.workspace.as_deref() {
            let ignore = std::fs::read_to_string(root.join(".bazelignore")).ok();
            if let Some(w) = crate::workspace::e_mode_guard(
                crate::workspace::root_is_dual(root),
                ignore.as_deref(),
                pkg,
            ) {
                return Err(LoadErr::declare(w));
            }
        }
    }
    // Pre-parsed AST present? Skip BOTH the read and the parse (the parallel pre-pass).
    let prepared = sess
        .ast_cache
        .borrow()
        .contains_key(&format!("{pkg}/BUILD"));
    let src = if prepared {
        String::new()
    } else {
        std::fs::read_to_string(&build_path).map_err(|e| LoadErr::declare(e.to_string()))?
    };

    // Short borrows around the nested eval (the [R1] discipline): set current_pkg, drop the
    // borrow, recurse, then restore — never hold a Session borrow across `eval_build_src`.
    let repo_ctx = pkg.strip_prefix('@').and_then(|rest| {
        rest.split_once("//")
            .map(|(r, sub)| (r.to_string(), sub.to_string()))
    });
    let prev = sess.set_current_pkg(Some(pkg.to_string()));
    let res = eval_build_src_in(sess, &format!("{pkg}/BUILD"), &src, repo_ctx, drive_all);
    sess.set_current_pkg(prev);
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
/// `load_tree_report_prepared` + the session's FULL loaded-package list (deps included) —
/// the next run seeds its queue with it so workers fan across the shared dep spine instead
/// of queueing behind whoever demands it first.
pub fn load_tree_report_prepared(
    root: &Path,
    flags: GlobalFlags,
    packages: &[String],
    asts: Vec<(String, starlark::syntax::AstModule)>,
) -> Vec<(String, Result<(), String>)> {
    load_tree_report_seeded(root, flags, packages, asts).0
}

pub fn load_tree_report_seeded(
    root: &Path,
    flags: GlobalFlags,
    packages: &[String],
    asts: Vec<(String, starlark::syntax::AstModule)>,
) -> (Vec<(String, Result<(), String>)>, Vec<String>) {
    // P4: N workers over a shared queue against ONE Session (Send+Sync, P1–P3).
    // DEFAULT-ON since round 46 (decision: Gianni — the parity bar held since round 34:
    // driven-work coverage equality, deterministic run-to-run, 0 livelock signatures).
    // RAZEL_LOAD_THREADS=1 reproduces sequential behavior exactly (the escape hatch).
    // Default 6 (Gianni, round 46): the measured knee — 12 workers average ~5 busy cores
    // at current corpus depth (the spine serializes the rest); 6 buys the same wall with
    // half the contention. Bazel-flag mapping of record: this is `--loading_phase_threads`
    // (loading/analysis), NOT `--jobs` (execution-phase actions) — wire when razel-cli
    // grows a tree command.
    let threads = std::env::var("RAZEL_LOAD_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
                .min(6)
        });
    load_tree_report_with_threads(root, flags, packages, asts, threads)
}

/// [`load_tree_report_seeded`] with an EXPLICIT worker count (the parity tests' entry —
/// no env mutation).
pub fn load_tree_report_with_threads(
    root: &Path,
    flags: GlobalFlags,
    packages: &[String],
    asts: Vec<(String, starlark::syntax::AstModule)>,
    threads: usize,
) -> (Vec<(String, Result<(), String>)>, Vec<String>) {
    let (_session, report, loaded) = drive_tree(root, flags, packages, asts, threads);
    (report, loaded)
}

/// Like [`load_tree_report_with_threads`], plus every analyzed target (the Session `results`
/// values) — the input to taut fact serialization and the content-addressed cache.
pub fn load_tree_report_with_targets(
    root: &Path,
    flags: GlobalFlags,
    packages: &[String],
    asts: Vec<(String, starlark::syntax::AstModule)>,
    threads: usize,
) -> (Vec<(String, Result<(), String>)>, Vec<String>, Vec<AnalyzedTarget>) {
    let (session, report, loaded) = drive_tree(root, flags, packages, asts, threads);
    let targets = session.results.borrow().values().cloned().collect();
    (report, loaded, targets)
}

/// Scan BUILD source for main-workspace label package paths: `//pkg/sub:name` → `pkg/sub`,
/// `//pkg/sub` → `pkg/sub`. External (`@repo//…`), scheme (`https://`), and `///` forms are
/// dropped by the preceding-char guard. Returns packages in first-seen order (caller dedups).
/// APPROXIMATE BY DESIGN — it feeds SCHEDULING (wave order) only, never correctness; the
/// restart backstop covers any edge it misses or invents. A crude byte scan beats walking the
/// typed AST here: it catches `load()` paths and `deps`/`data` labels alike, and a spurious
/// edge only over-constrains the order (the safe direction).
pub(crate) fn scan_label_pkgs(src: &str) -> Vec<String> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 1 < b.len() {
        if !(b[i] == b'/' && b[i + 1] == b'/') {
            i += 1;
            continue;
        }
        let prev = if i == 0 { 0u8 } else { b[i - 1] };
        // `xla//`, `@repo//`, `https://`, `///` are not main-workspace package refs.
        if prev.is_ascii_alphanumeric() || prev == b'@' || prev == b':' || prev == b'/' {
            i += 2;
            continue;
        }
        let start = i + 2;
        let mut j = start;
        while j < b.len()
            && (b[j].is_ascii_alphanumeric() || matches!(b[j], b'_' | b'.' | b'-' | b'/'))
        {
            j += 1;
        }
        // The path up to the `:` (or its end) IS the package: `//foo/bar:baz` → `foo/bar`.
        if j > start {
            out.push(src[start..j].to_string());
        }
        i = j.max(i + 2);
    }
    out
}

/// Per-package in-corpus dependency edges (`edges[i]` = indices `i` references and must run
/// AFTER). Parallel read of each package's BUILD source + [`scan_label_pkgs`], mapped to the
/// corpus index. Self-edges and out-of-corpus labels are dropped; each list is sorted+deduped
/// for determinism.
fn package_dep_edges(
    root: &Path,
    packages: &[String],
    threads: usize,
    strict_bazel: bool,
) -> Vec<Vec<usize>> {
    let n = packages.len();
    if n == 0 {
        return Vec::new();
    }
    let index: std::collections::HashMap<&str, usize> =
        packages.iter().enumerate().map(|(i, p)| (p.as_str(), i)).collect();
    let idxs: Vec<usize> = (0..n).collect();
    let chunks: Vec<&[usize]> = idxs.chunks(n.div_ceil(threads.max(1))).collect();
    let parts: Vec<Vec<(usize, Vec<usize>)>> = std::thread::scope(|scope| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                let index = &index;
                scope.spawn(move || {
                    let mut out = Vec::with_capacity(chunk.len());
                    for &i in chunk {
                        let mut deps = Vec::new();
                        if let Ok(Some(path)) =
                            crate::workspace::resolve_build_file(&root.join(&packages[i]), strict_bazel)
                            && let Ok(src) = std::fs::read_to_string(&path)
                        {
                            deps = scan_label_pkgs(&src)
                                .into_iter()
                                .filter_map(|p| index.get(p.as_str()).copied())
                                .filter(|&d| d != i)
                                .collect();
                            deps.sort_unstable();
                            deps.dedup();
                        }
                        out.push((i, deps));
                    }
                    out
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap_or_default()).collect()
    });
    let mut edges = vec![Vec::new(); n];
    for part in parts {
        for (i, ds) in part {
            edges[i] = ds;
        }
    }
    edges
}

/// Topological wave layering (Kahn by layer): wave 0 = packages with no in-corpus deps; wave
/// k = packages all of whose deps sit in waves `< k`. Cycle / leftover members (mutual deps an
/// approximate graph can invent) collapse into one final wave — the restart backstop covers
/// their ordering. Deterministic: indices stay ascending within a wave. O(depth · n).
fn compute_waves(n: usize, edges: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let mut placed = vec![false; n];
    let mut waves: Vec<Vec<usize>> = Vec::new();
    let mut remaining = n;
    while remaining > 0 {
        let wave: Vec<usize> = (0..n)
            .filter(|&i| !placed[i])
            .filter(|&i| edges.get(i).is_none_or(|ds| ds.iter().all(|&d| placed[d])))
            .collect();
        if wave.is_empty() {
            // A cycle (or invented mutual edge): nothing else can become ready. Emit the
            // leftover as a final wave and stop — the backstop handles their order.
            waves.push((0..n).filter(|&i| !placed[i]).collect());
            break;
        }
        for &i in &wave {
            placed[i] = true;
        }
        remaining -= wave.len();
        waves.push(wave);
    }
    waves
}

fn drive_tree(
    root: &Path,
    flags: GlobalFlags,
    packages: &[String],
    asts: Vec<(String, starlark::syntax::AstModule)>,
    threads: usize,
) -> (Session, Vec<(String, Result<(), String>)>, Vec<String>) {
    let session = Session::new(Some(root.to_path_buf()), flags);
    session.ast_cache.borrow_mut().extend(asts);
    if threads <= 1 {
        let report: Vec<(String, Result<(), String>)> = packages
            .iter()
            .map(|pkg| (pkg.clone(), load_package_entry(&session, pkg)))
            .collect();
        let loaded = loaded_done(&session);
        return (session, report, loaded);
    }
    let results = std::sync::Mutex::new(vec![None; packages.len()]);
    let retry = std::sync::Mutex::new(Vec::new());
    // Pull order. Default: ONE flat wave (atomic cursor over all packages — behavior
    // unchanged from the prior shared-queue driver). RAZEL_LOAD_WAVES=1: topological waves
    // with a BARRIER between layers, so a package's in-corpus deps FREEZE (publish their
    // captured provider instances cross-thread) before it runs — eliminating the
    // provider-reanalyze fallbacks that cap multithread scaling (RazelDepsEngineV2). The
    // graph is approximate; the restart rounds below stay as the correctness backstop.
    let wave_mode = std::env::var("RAZEL_LOAD_WAVES").unwrap_or_default();
    let waves: Vec<Vec<usize>> = if wave_mode == "1" || wave_mode == "order" {
        let edges = package_dep_edges(root, packages, threads, session.global.strict_bazel);
        let mut waves = compute_waves(packages.len(), &edges);
        if std::env::var("RAZEL_LOAD_WAVES_DIAG").is_ok() {
            let sizes: Vec<usize> = waves.iter().map(|w| w.len()).collect();
            eprintln!(
                "razel: {} waves over {} packages, sizes {sizes:?}",
                waves.len(),
                packages.len()
            );
        }
        // "order": collapse the layers into ONE barrier-free queue — deps still pulled earlier
        // (bias), but workers never idle at a wave boundary. "1": true barriered waves.
        if wave_mode == "order" {
            waves = vec![waves.into_iter().flatten().collect()];
        }
        waves
    } else {
        vec![(0..packages.len()).collect()]
    };
    for wave in &waves {
        let cursor = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(|| {
                    loop {
                        let k = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(&i) = wave.get(k) else { break };
                        let pkg = &packages[i];
                        let before = session.partial_reads();
                        let r = load_package_entry(&session, pkg);
                        // F4 (restart): an entry that FAILED after consuming cross-thread
                        // partial state (CycleProceed grants, dead declaration waits) is not
                        // a sequential verdict — queue it for the post-drain restart rounds.
                        if r.is_err() && session.partial_reads() > before {
                            retry.lock().expect("retry").push(i);
                        }
                        results
                            .lock()
                            .expect("results")
                            .get_mut(i)
                            .map(|slot| *slot = Some(r));
                    }
                });
            }
        });
    }
    let mut results = results.into_inner().expect("results");
    // Restart rounds, SINGLE-threaded (Skyframe's answer, RazelDemandFutures.md §5): by
    // now the cycle partners are terminal, so each retry sees what a sequential entry
    // would have. Rounds until no progress — termination is structural, no cap to tune.
    let mut retry = retry.into_inner().expect("retry");
    retry.sort_unstable();
    while !retry.is_empty() {
        eprintln!(
            "razel: restarting {} entry load(s) after cross-thread partial reads",
            retry.len()
        );
        let mut progressed = false;
        let mut still_failing = Vec::new();
        for &i in &retry {
            let r = load_package_entry(&session, &packages[i]);
            if r.is_ok() {
                progressed = true;
            } else {
                still_failing.push(i);
            }
            results[i] = Some(r);
        }
        if !progressed {
            break;
        }
        retry = still_failing;
    }
    let report: Vec<(String, Result<(), String>)> = packages
        .iter()
        .cloned()
        .zip(results.into_iter().map(|r| r.unwrap_or(Ok(()))))
        .collect();
    let loaded = loaded_done(&session);
    (session, report, loaded)
}

/// All packages the session finished loading (deps included) — the spine list. The wait
/// graph also tracks `.bzl` modules and declarations — packages only here.
fn loaded_done(session: &Session) -> Vec<String> {
    session
        .loaded
        .lock()
        .expect("loaded")
        .res
        .iter()
        .filter_map(|(k, st)| match (k, st) {
            (crate::state::ResKey::Pkg(p), crate::state::PkgState::Done) => Some(p.clone()),
            _ => None,
        })
        .collect()
}

/// PARALLEL read+parse of the packages' BUILD files (pure — no Session state): the
/// load+parse / execute split. Returns `({pkg}/BUILD, ast)` pairs; unparseable files are
/// skipped (the sequential path re-reads and surfaces the error properly).
pub fn prepare_build_asts(
    root: &Path,
    packages: &[String],
    threads: usize,
    strict_bazel: bool,
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
                        // Same resolution as `load_package` (E-mode XOR, bazel precedence);
                        // XOR errors are SKIPPED here so the sequential path surfaces them.
                        let Ok(Some(path)) =
                            crate::workspace::resolve_build_file(&root.join(pkg), strict_bazel)
                        else {
                            continue;
                        };
                        let Ok(src) = std::fs::read_to_string(&path) else {
                            continue;
                        };
                        let name = format!("{pkg}/BUILD");
                        if let Ok(ast) = AstModule::parse(
                            &name,
                            detab_leading(&src).into_owned(),
                            &Dialect::Extended,
                        ) {
                            out.push((name, ast));
                        }
                    }
                    out
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    })
}

/// Evaluate a `BUILD`/`.bzl` that defines and instantiates Starlark rules, running each
/// rule impl (same-scope analysis); returns the analyzed targets.
pub fn analyze_starlark(name: &str, src: &str) -> Result<Vec<AnalyzedTarget>, String> {
    let session = Session::default();
    let ast = AstModule::parse(name, detab_leading(src).into_owned(), &Dialect::Extended)
        .map_err(|e| format!("{e}"))?;
    // ONE globals surface everywhere (round 44: a private duplicate here predated
    // builder_base and silently missed later dialect additions — the shadowed fail()).
    let globals = build_globals();
    let res: Result<(), String> = Module::with_temp_heap(|module| {
        crate::dialect::install_decl_store(&module);
        {
            let mut eval = Evaluator::new(&module);
            eval.extra = Some(&session);
            eval.eval_module(ast, &globals)
                .map_err(|e| format!("{e}"))?;
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

    // ── wave scheduling: the static dep-edge scan + topological layering (pure halves). ──

    #[test]
    fn scan_label_pkgs_extracts_main_workspace_packages_only() {
        let src = r#"
load("//tools/build:defs.bzl", "my_rule")
my_rule(
    name = "x",
    deps = ["//foo/bar:baz", "//foo/bar:qux", ":local", "@xla//ext:lib"],
    data = "//data/pkg",
)
# see https://example.com//not-a-label
"#;
        let pkgs = scan_label_pkgs(src);
        // `//foo/bar` (twice), `//tools/build`, `//data/pkg` — NOT `:local` (no `//`),
        // NOT `@xla//ext` (external, `@`-guarded), NOT `https://…` (`:`-guarded).
        assert!(pkgs.contains(&"foo/bar".to_string()), "deps label pkg, got {pkgs:?}");
        assert!(pkgs.contains(&"tools/build".to_string()), "load() pkg, got {pkgs:?}");
        assert!(pkgs.contains(&"data/pkg".to_string()), "bare //pkg, got {pkgs:?}");
        assert!(!pkgs.iter().any(|p| p.contains("ext")), "external excluded, got {pkgs:?}");
        assert!(!pkgs.iter().any(|p| p.contains("example")), "scheme excluded, got {pkgs:?}");
    }

    #[test]
    fn compute_waves_layers_a_chain_diamond_and_breaks_cycles() {
        // Chain 0←1←2 (edges[i] = deps that must PRECEDE i): waves peel one per layer.
        let chain = vec![vec![], vec![0], vec![1]];
        assert_eq!(compute_waves(3, &chain), vec![vec![0], vec![1], vec![2]]);

        // Diamond: 0 root; 1,2 depend on 0; 3 depends on 1,2.
        let diamond = vec![vec![], vec![0], vec![0], vec![1, 2]];
        assert_eq!(compute_waves(4, &diamond), vec![vec![0], vec![1, 2], vec![3]]);

        // A 2-cycle (0↔1) plus a clean leaf 2: leaf lands wave 0, the cycle collapses into a
        // single trailing wave rather than spinning forever.
        let cyclic = vec![vec![1], vec![0], vec![]];
        let waves = compute_waves(3, &cyclic);
        assert_eq!(waves.first(), Some(&vec![2]), "leaf first, got {waves:?}");
        assert_eq!(waves.last(), Some(&vec![0, 1]), "cycle collapsed last, got {waves:?}");
        // Every index is placed exactly once (partition invariant).
        let mut all: Vec<usize> = waves.iter().flatten().copied().collect();
        all.sort_unstable();
        assert_eq!(all, vec![0, 1, 2]);
    }

    // ── fold_field (F3/F24): the LIVE transitive fold, tested directly (not only via the .bzl). ──

    #[test]
    fn tab_indented_source_parses_like_bazel() {
        // Bazel accepts tab indentation (a tab advances to the next multiple-of-8 column);
        // starlark-rust rejects tabs outright (rules_ml_toolchain's cuda_redist_versions
        // .bzl is tab-indented — the fetch R1 probe wall). Leading tabs expand; tabs
        // inside strings are untouched.
        let src = "def _impl(ctx):\n\treturn [DefaultInfo(files = [\"a\tb\"])]\n\nr = rule(implementation = _impl, attrs = {})\nr(name = \"x\")\n";
        let targets = analyze_starlark("BUILD", src).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(
            targets[0].default_info,
            vec!["a\tb"],
            "string-internal tab preserved"
        );
    }

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
        // Round 40: select sits on the ATTR (Bazel's model — selects are attr values, never
        // impl-time expressions; the retired eager hybrid had let the impl-time form work).
        let src = r#"
config_setting(name = "dbg", values = {"compilation_mode": "dbg"})

def _impl(ctx):
    ctx.actions.run(executable = "cc", outputs = [ctx.attr.name], inputs = [], arguments = ctx.attr.flags)
    return [DefaultInfo(files = [ctx.attr.name])]

thing = rule(implementation = _impl, attrs = {"flags": attr.string_list()})
thing(name = "x", flags = select({"//conditions:default": ["-O2"], ":dbg": ["-g"]}))
"#;
        let targets = analyze_starlark("BUILD", src).unwrap();
        let x = targets.iter().find(|t| t.name.ends_with("x")).unwrap();
        assert_eq!(x.actions[0].mnemonic, "cc");
        assert!(
            x.actions[0].argv.contains(&"-O2".to_string()),
            "default branch picked"
        );
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
