//! `decls::aspect` — split from `decls.rs` (facade in `mod.rs`).

use crate::provider_values::{AspectObj, DepTarget, FrozenAspectObj, instance_callable};
use crate::selects::resolve_attr_value;
use crate::state::session;
use starlark::eval::Evaluator;
use starlark::values::list::ListRef;
use starlark::values::{
    StarlarkValue, Value, ValueLike,
};
// ---- rule() + DefaultInfo + select ----------------------------------------------
use super::*;

/// Apply an aspect to an analyzed dep (L5 MVP): run its impl in THIS eval with
/// `target` = the dep's current providers and `ctx.rule.attr` = the dep's ORIGINAL attrs.
/// Label attrs resolve to dep targets; attrs named in `attr_aspects` resolve recursively WITH
/// this aspect. Results memoize in the consumer store's captured map under the aspect id plus
/// label; returned pairs extend the DepTarget.
pub(crate) fn apply_aspect<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    aspectv: Value<'v>,
    label: &str,
    providers: &mut Vec<(Value<'v>, Value<'v>)>,
) -> starlark::Result<()> {
    let (aspect_id, implementation, asp_attrs, attr_aspects) = {
        if let Some(a) = aspectv.downcast_ref::<AspectObj<'v>>() {
            (a.id, a.implementation, a.attrs, a.attr_aspects.clone())
        } else if let Some(a) = aspectv.downcast_ref::<FrozenAspectObj>() {
            (
                a.id,
                a.implementation.to_value(),
                a.attrs.to_value(),
                a.attr_aspects.clone(),
            )
        } else {
            return Ok(()); // not an aspect value (absorbed/None) — nothing to apply
        }
    };
    let memo_key = format!("aspect::{aspect_id}::{label}");
    if let Some(pairs) = decl_store(eval)?.captured.borrow().get(&memo_key) {
        providers.extend(pairs.iter().copied());
        return Ok(());
    }
    let sess = session(eval);
    if !sess.analyzing_insert(&memo_key) {
        return Ok(()); // cycle: the aspect is being computed up-stack
    }
    let res = apply_aspect_uncached(
        eval,
        implementation,
        asp_attrs,
        &attr_aspects,
        aspectv,
        label,
        &memo_key,
        providers,
    );
    session(eval).analyzing_remove(&memo_key);
    res
}


pub(crate) fn apply_aspect_uncached<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    implementation: Value<'v>,
    asp_attrs: Value<'v>,
    attr_aspects: &[String],
    aspectv: Value<'v>,
    label: &str,
    memo_key: &str,
    providers: &mut Vec<(Value<'v>, Value<'v>)>,
) -> starlark::Result<()> {
    let heap = eval.heap();
    // The dep's ORIGINAL attrs (the harvested decl): rule.attr for the aspect impl.
    let deferred = find_original_decl(eval, label);
    // Aspect work runs in the TARGET's package context (declare_file/labels qualify there).
    let origin = crate::state::pkg_of(label);
    let prev = match origin {
        Some(p) => session(eval).set_current_pkg(Some(p)),
        None => session(eval).current_pkg(),
    };
    let result = (|| -> starlark::Result<()> {
        let mut rule_fields: Vec<(String, Value<'v>)> = Vec::new();
        let attr_aspects: std::collections::BTreeSet<&str> =
            attr_aspects.iter().map(String::as_str).collect();
        if let Some((_pkg, rule, kwargs)) = &deferred {
            let schema = rule_parts(*rule).ok().map(|(_, attrs, _)| attrs);
            for (k, v) in kwargs {
                let attr_kind = schema.and_then(|attrs| {
                    starlark::values::dict::DictRef::from_value(attrs).and_then(|d| {
                        d.iter().find_map(|(kk, desc)| {
                            if kk.unpack_str() == Some(k.as_str()) {
                                desc.get_attr("kind", heap)
                                    .ok()
                                    .flatten()
                                    .and_then(|kind| kind.unpack_str().map(String::from))
                            } else {
                                None
                            }
                        })
                    })
                });
                let is_label_attr = k == "deps"
                    || attr_aspects.contains(k.as_str())
                    || matches!(attr_kind.as_deref(), Some("label") | Some("label_list"));
                if is_label_attr {
                    let mut sub = Vec::new();
                    let vv = resolve_attr_value(eval, *v)?;
                    let aspects = if attr_aspects.contains(k.as_str()) {
                        heap.alloc(vec![aspectv])
                    } else {
                        Value::new_none()
                    };
                    let resolved = resolve_label_attr_inner(
                        eval,
                        vv,
                        attr_kind.as_deref() == Some("label"),
                        &mut sub,
                        aspects,
                    )?;
                    rule_fields.push((k.clone(), resolved));
                } else {
                    rule_fields.push((k.clone(), *v));
                }
            }
        }
        if !rule_fields.iter().any(|(k, _)| k == "deps") {
            rule_fields.push(("deps".to_string(), heap.alloc(Vec::<Value<'v>>::new())));
        }
        // The aspect's OWN attrs (implicit label defaults resolve like rule-schema defaults).
        let mut own_fields: Vec<(String, Value<'v>)> = Vec::new();
        if let Some(d) = starlark::values::dict::DictRef::from_value(asp_attrs) {
            let entries: Vec<(String, Value<'v>)> = d
                .iter()
                .filter_map(|(k, desc)| k.unpack_str().map(|k| (k.to_string(), desc)))
                .collect();
            drop(d);
            for (an, desc) in entries {
                let default = desc
                    .get_attr("default", heap)?
                    .unwrap_or_else(Value::new_none);
                let kind = desc
                    .get_attr("kind", heap)?
                    .and_then(|k| k.unpack_str().map(String::from))
                    .unwrap_or_default();
                let v = if !default.is_none() && (kind == "label" || kind == "label_list") {
                    let default = resolve_attr_value(eval, default)?;
                    let mut sub = Vec::new();
                    resolve_label_attr_inner(
                        eval,
                        default,
                        kind == "label",
                        &mut sub,
                        Value::new_none(),
                    )?
                } else {
                    default
                };
                own_fields.push((an, v));
            }
        }
        // target: the dep as seen so far (files + providers accumulated pre-aspect).
        let target = heap.alloc(DepTarget {
            label: label.to_string(),
            fields: vec![("files".to_string(), heap.alloc(Vec::<Value<'v>>::new()))],
            providers: providers.clone(),
        });
        let (repo, rest) = match label.split_once("//") {
            Some((r, rest)) if r.starts_with('@') => (Some(r.to_string()), rest),
            Some((_, rest)) => (None, rest),
            None => (None, label),
        };
        let (lpkg, lname) = rest.split_once(':').unwrap_or(("", rest));
        let ctx = heap.alloc(crate::engine::AbsorbWith {
            overrides: vec![
                (
                    "rule".to_string(),
                    heap.alloc(crate::engine::AbsorbWith {
                        overrides: vec![(
                            "attr".to_string(),
                            heap.alloc(starlark::values::structs::AllocStruct(rule_fields)),
                        )],
                    }),
                ),
                (
                    "attr".to_string(),
                    heap.alloc(starlark::values::structs::AllocStruct(own_fields)),
                ),
                (
                    "actions".to_string(),
                    heap.alloc_complex_no_freeze(crate::values::Actions),
                ),
                (
                    "label".to_string(),
                    heap.alloc(crate::labels::LabelV {
                        repo,
                        package: lpkg.to_string(),
                        name: lname.to_string(),
                    }),
                ),
                (
                    "toolchains".to_string(),
                    crate::ctxv::toolchain_map(heap, session(eval)),
                ),
                (
                    "bin_dir".to_string(),
                    heap.alloc(crate::engine::AbsorbWith {
                        overrides: vec![("path".to_string(), heap.alloc("bazel-out/bin"))],
                    }),
                ),
                (
                    "genfiles_dir".to_string(),
                    heap.alloc(crate::engine::AbsorbWith {
                        overrides: vec![("path".to_string(), heap.alloc("bazel-out/bin"))],
                    }),
                ),
            ],
        });
        let ret = eval.eval_function(implementation, &[target, ctx], &[])?;
        let mut pairs: Vec<(Value<'v>, Value<'v>)> = Vec::new();
        if let Some(list) = ListRef::from_value(ret) {
            for item in list.iter() {
                if let Some(c) = instance_callable(item) {
                    pairs.push((c, item));
                }
            }
        } else if let Some(c) = instance_callable(ret) {
            pairs.push((c, ret));
        }
        decl_store(eval)?
            .captured
            .borrow_mut()
            .insert(memo_key.to_string(), pairs.clone());
        // Projectability measurement (gated on the diag hook): classify each captured provider's
        // fields against the DDS-projectable set — the signal for whether a cross-thread consumer
        // could be served from DDS facts (option 1) instead of re-analyzing.
        let sess = session(eval);
        if sess.global.sched_hook.is_some() {
            for (_, inst) in &pairs {
                for (proj, ty) in crate::provider_values::classify_provider_fields(*inst) {
                    crate::state::load_event(sess, "provider-field", &format!("{}:{ty}", proj as u8));
                }
            }
        }
        providers.extend(pairs);
        Ok(())
    })();
    session(eval).set_current_pkg(prev);
    result
}


