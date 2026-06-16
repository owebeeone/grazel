//! `decls::resolve` — split from `decls.rs` (facade in `mod.rs`).

use crate::labels::LabelV;
use crate::provider_values::DepTarget;
use crate::state::{canon_label, qualify, session};
use razel_dds::InstanceId;
use starlark::eval::Evaluator;
use starlark::values::list::ListRef;
use starlark::values::{
    Value, ValueLike,
};
// ---- rule() + DefaultInfo + select ----------------------------------------------
use super::*;

/// Resolve a label/label_list attr VALUE (passed or default) to DepTarget struct(s) — shared by
/// the kwargs arm and the schema-defaults pass (implicit label attrs like rules_cc's
/// `_impl_delegate` must resolve exactly like passed ones).
pub(crate) fn resolve_label_attr<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    v: Value<'v>,
    single_label: bool,
    dep_labels: &mut Vec<String>,
    aspects: Value<'v>,
) -> starlark::Result<Value<'v>> {
    resolve_label_attr_inner(eval, v, single_label, dep_labels, aspects)
}


pub(crate) fn resolve_label_attr_inner<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    v: Value<'v>,
    single_label: bool,
    dep_labels: &mut Vec<String>,
    aspects: Value<'v>,
) -> starlark::Result<Value<'v>> {
    let one = |x: Value<'v>| -> Option<String> {
        x.unpack_str()
            .map(String::from)
            .or_else(|| x.downcast_ref::<LabelV>().map(|l| l.to_string()))
    };
    let labels: Vec<String> = if let Some(list) = ListRef::from_value(v) {
        list.iter().filter_map(one).collect()
    } else if let Some(s) = one(v) {
        vec![s]
    } else {
        Vec::new()
    };
    let mut structs: Vec<Value<'v>> = Vec::new();
    for label in &labels {
        // Canonical label — bare in single-package mode, //pkg:name in a workspace.
        let mut dep = canon_label(session(eval), label);
        // E0: a forward-referenced local declaration analyzes on demand, first.
        ensure_analyzed(eval, &dep)?;
        // Aliases forward to their `actual` (provider flow lives on the terminal target).
        for _ in 0..32 {
            let next = session(eval).aliases.borrow().get(&dep).cloned();
            match next {
                Some(actual) => {
                    dep = actual;
                    ensure_analyzed(eval, &dep)?;
                }
                None => break,
            }
        }
        let sess = session(eval);
        // Cross-package dep: load its package on demand (mirrors resolve_dep);
        // a failed load SURFACES (silence turns into bogus "not declared").
        let mut dep_load_err = None;
        if !sess.results.borrow().contains_key(&dep)
            && sess.workspace.is_some()
            && let Some(pkg) = crate::state::pkg_of(&dep)
        {
            dep_load_err = crate::rules::load_package(sess, &pkg).err();
        }
        if let Some(e) = dep_load_err
            && !sess.results.borrow().contains_key(&dep)
        {
            return Err(anyhow::anyhow!("loading dep `{dep}`'s package failed: {e}").into());
        }
        let resolved = {
            let results = sess.results.borrow();
            results.get(&dep).map(|t| t.default_info.clone())
        };
        let files = match resolved {
            Some(files) => files,
            None => {
                // A file label naming a GENERATED output: resolve via the output index
                // (the producer analyzes on demand; the dep's file is the output path).
                let produced = sess.output_index.borrow().get(&dep).cloned();
                if let Some((producer, out_path)) = produced {
                    ensure_analyzed(eval, &producer)?;
                    let heap = eval.heap();
                    let f = heap.alloc(crate::values::File { path: out_path });
                    structs.push(heap.alloc(DepTarget {
                        label: dep.clone(),
                        fields: vec![("files".to_string(), heap.alloc(vec![f]))],
                        providers: Vec::new(),
                    }));
                    dep_labels.push(producer);
                    continue;
                }
                // Bazel file-label semantics (L2): a label naming no declared target
                // resolves to a SOURCE FILE in the package when that file exists
                // (`srcs = ["lib.rs"]`). Source files are not target deps. External
                // file labels check the vendored repo; their path takes Bazel's
                // exec-root form (`external/<repo>/…`).
                let (on_disk, qualified) = if dep.starts_with('@') {
                    // Trim ALL leading `@` so the canonical `@@repo//` form (§11.3) resolves too
                    // (mirrors the deps-arm fix — a lone `strip_prefix('@')` left `@@…` repos a stray
                    // `@`, missing the vendored dir for a data/srcs file like blake3's `Cargo.lock`).
                    let rest = dep.trim_start_matches('@');
                    match rest
                        .split_once("//")
                        .and_then(|(r, pf)| pf.split_once(':').map(|(p, f)| (r, p, f)))
                    {
                        Some((repo, pkg, file)) => {
                            // Host-materialized repo files (`@cc_compatibility_proxy//:
                            // symbols.bzl`) exist by construction — razel compiled them in.
                            let exists = crate::host::host_bzl(&dep).is_some()
                                || sess.global.external_repo_dirs(repo).iter().any(|d| {
                                    crate::state::path_is_file(sess, &d.join(pkg).join(file))
                                });
                            (exists, format!("external/{repo}/{pkg}/{file}"))
                        }
                        None => (false, dep.clone()),
                    }
                } else if let Some(rest) = dep.strip_prefix("//") {
                    // A workspace file label from ANY package: `//pkg:file` → `pkg/file`.
                    let q = rest
                        .replacen(':', "/", 1)
                        .trim_start_matches('/')
                        .to_string();
                    let exists = sess
                        .workspace
                        .as_ref()
                        .is_some_and(|root| crate::state::path_is_file(sess, &root.join(&q)));
                    (exists, q)
                } else {
                    let q = qualify(sess, label.trim_start_matches(':'));
                    let exists = sess
                        .workspace
                        .as_ref()
                        .is_some_and(|root| crate::state::path_is_file(sess, &root.join(&q)));
                    (exists, q)
                };
                if on_disk {
                    let heap = eval.heap();
                    let f = heap.alloc(crate::values::File { path: qualified });
                    structs.push(heap.alloc(DepTarget {
                        label: dep.clone(),
                        fields: vec![("files".to_string(), heap.alloc(vec![f]))],
                        providers: Vec::new(),
                    }));
                    continue;
                }
                if let Some(e) = sess.native_errors.borrow().get(&dep) {
                    return Err(
                        anyhow::anyhow!("analysis of `{dep}` previously failed: {e}").into(),
                    );
                }
                return Err(anyhow::anyhow!(
                    "`{dep}` is neither a declared target nor a source file in \
                         this package"
                )
                .into());
            }
        };
        let tkey =
            crate::dds::target_key(InstanceId::SINGLE, &dep).map_err(|e| anyhow::anyhow!(e))?;
        // `files` is own-exposed (DefaultInfo); transitive fields fold via the ONE
        // registry-driven helper over the Session's LIVE store (E0d — no rebuild).
        let mut sfields: Vec<(String, Vec<String>)> = vec![("files".to_string(), files)];
        if let Some(hit) = sess.fold_cache.borrow().get(&dep) {
            sfields.extend(hit.iter().cloned());
        } else {
            let folded = {
                let dds = crate::dds::session_dds(sess);
                crate::dds::fold_dep_fields(&dds, &tkey)
            };
            sess.fold_cache
                .borrow_mut()
                .insert(dep.clone(), folded.clone());
            sfields.extend(folded);
        }
        // L2a: a dep is a DepTarget — plain projected fields by attr, plus the dep's
        // returned provider instances indexable by constructor (`dep[MyInfo]`).
        let heap = eval.heap();
        // `files` entries become FILE values (impls read .extension/.path on dep files);
        // folded provider fields (cflags/defines/…) stay plain strings.
        let dfields: Vec<(String, Value<'v>)> = sfields
            .into_iter()
            .map(|(k, xs)| {
                if k == "files" {
                    let files: Vec<Value<'v>> = xs
                        .into_iter()
                        .map(|p| heap.alloc(crate::values::File { path: p }))
                        .collect();
                    (k, heap.alloc(files))
                } else {
                    (k, heap.alloc(xs))
                }
            })
            .collect();
        let providers = decl_store(eval)?.captured.borrow().get(&dep).cloned();
        // Layer 0: cross-package instances come from the Session harvest.
        let providers = providers.or_else(|| cross_providers_for(eval, &dep));
        // Demand-analyzed-by-ANOTHER-consumer: the instances live in that consumer's
        // (unfrozen, mid-load) module — invisible here. Re-analyze the harvested decl in
        // THIS eval (idempotent: results overwrite by label) so instances are local.
        let providers = if providers.is_none()
            && !session(eval).analyzing_contains(&dep)
            && find_deferred(eval, &dep).is_some()
        {
            // P4a: a results row is visible the moment its producer RECORDS it, but the
            // producer's captured-instance harvest lands only when its whole package eval
            // completes. If that package is mid-flight on ANOTHER worker, wait for it
            // (load_package: no-op when Done, condvar wait when InFlight), then re-read
            // the harvest before falling back to a local re-analysis.
            if let Some(pkg) = crate::state::pkg_of(&dep) {
                let _ = crate::rules::load_package(session(eval), &pkg);
            }
            let waited = cross_providers_for(eval, &dep);
            if let Some(waited) = waited {
                waited
            } else {
                crate::state::load_event(session(eval), "provider-reanalyze", &dep);
                analyze_deferred(eval, &dep)?;
                decl_store(eval)?
                    .captured
                    .borrow()
                    .get(&dep)
                    .cloned()
                    .unwrap_or_default()
            }
        } else {
            providers.unwrap_or_default()
        };
        // L5: apply this attr's aspects — extra providers attach to the dep target.
        let mut providers = providers;
        if let Some(l) = ListRef::from_value(aspects) {
            let to_apply: Vec<Value<'v>> = l.iter().collect();
            for a in to_apply {
                apply_aspect(eval, a, &dep, &mut providers)?;
            }
        }
        let heap = eval.heap();
        structs.push(heap.alloc(DepTarget {
            label: dep.clone(),
            fields: dfields,
            providers,
        }));
        dep_labels.push(dep);
    }
    // D1c: a single `attr.label` yields ONE struct; a list yields the list of structs.
    Ok(if single_label {
        structs.into_iter().next().unwrap_or_else(Value::new_none)
    } else {
        eval.heap().alloc(structs)
    })
}


