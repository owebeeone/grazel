//! `rules::load` — split from `rules.rs` (facade in `mod.rs`).

use crate::shims::{auto_config_module, rules_cc_module, rules_java_module, rules_skylib_module};
use crate::state::{CcToolchainMode, GlobalFlags, Session};
use starlark::environment::{FrozenModule, Globals, Module};
use starlark::eval::{Evaluator, FileLoader};
use starlark::syntax::{AstModule, Dialect};
use std::cell::RefCell;
use std::path::{Path, PathBuf};

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
    pub(crate) rulesets: &'a [Ruleset],
    pub(crate) globals: &'a Globals,
    pub(crate) session: &'a Session,
    /// The (repo, pkg) of the external module currently being loaded (`None` frames are
    /// workspace modules). Loads written INSIDE `@repo` resolve repo-relatively — Bazel label
    /// semantics (`//pkg:f.bzl` in rules_cc means rules_cc's own pkg, not the workspace's).
    pub(crate) load_ctx: RefCell<Vec<Option<(String, String)>>>,
}


/// `@repo//pkg:file` → `(repo, pkg)`.
pub(crate) fn parse_external(path: &str) -> Option<(String, String)> {
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


