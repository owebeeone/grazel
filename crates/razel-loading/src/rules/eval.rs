//! `rules::eval` — split from `rules.rs` (facade in `mod.rs`).

use crate::state::{AnalyzedTarget, GlobalFlags, Session};
use starlark::environment::{Globals, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};
use std::cell::RefCell;
use super::*;

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


pub(crate) fn eval_build_src_inner(
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


pub(crate) fn salvage_captured_after_analysis_failure<'v>(session: &Session, module: Module<'v>) {
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
pub(crate) fn index_harvest(
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


