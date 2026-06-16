//! `decls::rule_decl` — split from `decls.rs` (facade in `mod.rs`).

use crate::ctxv::{Ctx, toolchain_map};
use crate::deps::record_target;
use crate::labels::LabelV;
use crate::provider_values::instance_callable;
use crate::selects::resolve_attr_value;
use crate::state::{AnalyzedTarget, canon_label, qualify, qualify_output, session};
use crate::values::{Actions, File};
use starlark::eval::Evaluator;
use starlark::values::list::ListRef;
use starlark::values::structs::AllocStruct;
use starlark::values::{
    StarlarkValue, Value,
};
// ---- rule() + DefaultInfo + select ----------------------------------------------
use super::*;

/// The analysis of one Starlark-rule declaration — dep resolution (demand-driven), schema
/// defaults/`mandatory`, ctx construction, and the impl call. (Ran inside `invoke()` before E0.)
pub(crate) fn analyze_rule_decl<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    rule: Value<'v>,
    kwargs: &[(String, Value<'v>)],
) -> starlark::Result<()> {
    let (implementation, attrs, out_templates) = rule_parts(rule)?;
    // Deferred selects resolve HERE — attr consumption time (Bazel's model); conditions declared
    // anywhere in the loaded graph are visible by now. An EXPLICIT `None` kwarg means UNSET
    // (Bazel drops it; the schema default applies) — TF macros pass `copts = None` through.
    let kwargs: Vec<(String, Value<'v>)> = kwargs
        .iter()
        .filter(|(_, v)| !v.is_none())
        .map(|(k, v)| Ok((k.clone(), resolve_attr_value(eval, *v)?)))
        .collect::<anyhow::Result<_>>()?;
    let mut name = String::new();
    let mut dep_labels: Vec<String> = Vec::new();
    let mut fields: Vec<(String, Value<'v>)> = Vec::new();
    // Typed output attrs (kind output/output_list) surface as FILES on ctx.outputs.<attr>
    // (@xla cc_embed_data iterates ctx.outputs.outs — round 42).
    let mut typed_outputs: Vec<(String, Vec<String>)> = Vec::new();
    for (key, v) in &kwargs {
        let (key, v) = (key.clone(), *v);
        // D1b/c: the schema kind drives label resolution. Look it up once: `label`/`label_list`
        // resolve to provider struct(s); the legacy `deps` is an implicit `label_list`.
        let (attr_kind, attr_aspects): (Option<String>, Value<'v>) = {
            let mut k = None;
            let mut asp = Value::new_none();
            if let Some(d) = starlark::values::dict::DictRef::from_value(attrs) {
                for (kk, desc) in d.iter() {
                    if kk.unpack_str() == Some(key.as_str()) {
                        if let Ok(Some(kind)) = desc.get_attr("kind", eval.heap()) {
                            k = kind.unpack_str().map(String::from);
                        }
                        if let Ok(Some(a)) = desc.get_attr("aspects", eval.heap()) {
                            asp = a;
                        }
                        break;
                    }
                }
            }
            (k, asp)
        };
        let is_label =
            key == "deps" || matches!(attr_kind.as_deref(), Some("label") | Some("label_list"));
        let single_label = attr_kind.as_deref() == Some("label");
        match key.as_str() {
            "name" => {
                name = v.unpack_str().unwrap_or_default().to_string();
                fields.push((key, v));
            }
            // `label_keyed_string_dict` (TF's build_settings pattern): KEYS resolve to dep
            // targets; string values pass through.
            _ if attr_kind.as_deref() == Some("label_keyed_string_dict") => {
                let heap = eval.heap();
                let mut entries: Vec<(Value<'v>, Value<'v>)> = Vec::new();
                if let Some(d) = starlark::values::dict::DictRef::from_value(v) {
                    let pairs: Vec<(Value<'v>, Value<'v>)> = d.iter().collect();
                    drop(d);
                    for (kk, vv) in pairs {
                        let resolved =
                            resolve_label_attr(eval, kk, true, &mut dep_labels, Value::new_none())?;
                        entries.push((resolved, vv));
                    }
                }
                fields.push((key, heap.alloc(starlark::values::dict::AllocDict(entries))));
            }
            _ if matches!(attr_kind.as_deref(), Some("output") | Some("output_list")) => {
                let outs: Vec<String> = match v.unpack_str() {
                    Some(s) => vec![s.to_string()],
                    None => crate::values::unpack_strs_any(Some(v)),
                };
                typed_outputs.push((key.clone(), outs));
                fields.push((key, v));
            }
            // A label attr (legacy `deps` or any `attr.label_list`): resolve each label to its
            // analyzed providers as a `struct(files=…, <folded fields>…)`.
            _ if is_label => {
                let resolved =
                    resolve_label_attr(eval, v, single_label, &mut dep_labels, attr_aspects)?;
                fields.push((key, resolved));
            }
            _ => fields.push((key, v)),
        }
    }

    // D1: consult the declared attrs schema — fill omitted attrs from their `default`, error on a
    // missing `mandatory` one. (A2 discarded the schema; the real upstream rules require it.)
    if let Some(schema) = starlark::values::dict::DictRef::from_value(attrs) {
        let passed: std::collections::BTreeSet<&str> =
            kwargs.iter().map(|(k, _)| k.as_str()).collect();
        for (aname, descriptor) in schema.iter() {
            let Some(an) = aname.unpack_str() else {
                continue;
            };
            if an == "name" || passed.contains(an) {
                continue;
            }
            let default = descriptor
                .get_attr("default", eval.heap())?
                .unwrap_or_else(Value::new_none);
            let kind = descriptor
                .get_attr("kind", eval.heap())?
                .and_then(|k| k.unpack_str().map(String::from))
                .unwrap_or_default();
            if !default.is_none() {
                // Implicit label attrs (rules_cc `_impl_delegate` etc.) resolve like passed ones.
                let v = if kind == "label" || kind == "label_list" {
                    let default = resolve_attr_value(eval, default)?;
                    let asp = descriptor
                        .get_attr("aspects", eval.heap())?
                        .unwrap_or_else(Value::new_none);
                    resolve_label_attr(eval, default, kind == "label", &mut dep_labels, asp)?
                } else {
                    default
                };
                fields.push((an.to_string(), v));
            } else if descriptor
                .get_attr("mandatory", eval.heap())?
                .and_then(|m| m.unpack_bool())
                .unwrap_or(false)
            {
                return Err(anyhow::anyhow!("mandatory attribute `{an}` not provided").into());
            } else {
                // Bazel TYPE defaults: an attr always exists on ctx.attr (real rules iterate
                // `ctx.attr.deps` unconditionally). Lists → [], dicts → {}, string → "",
                // int → 0, bool → False, label/output → None.
                let heap = eval.heap();
                let v = match kind.as_str() {
                    "label_list" | "string_list" | "output_list" => {
                        heap.alloc(Vec::<Value<'v>>::new())
                    }
                    "string_dict" | "string_list_dict" | "label_keyed_string_dict" => {
                        heap.alloc(starlark::values::dict::AllocDict::EMPTY)
                    }
                    "string" => heap.alloc(""),
                    "int" => heap.alloc(0),
                    "bool" => Value::new_bool(false),
                    _ => Value::new_none(), // label / output / unknown kinds
                };
                fields.push((an.to_string(), v));
            }
        }
    }

    let sess = session(eval);
    let heap = eval.heap();
    sess.set_current_target(Some(AnalyzedTarget {
        name: canon_label(sess, &name),
        deps: dep_labels,
        ..Default::default()
    }));

    // ctx.outputs.<attr> — string-valued attrs are predeclared output filenames
    // (package-qualified). ctx.files.<attr> — list-valued attrs are source files
    // (qualified). ctx.executable is empty until an executable-label attr is wired.
    let mk_file = |s: &str| {
        heap.alloc(File {
            path: qualify(sess, s),
        })
    };
    // ctx.outputs entries are GENERATED files → bazel-out under compat (mk_file stays for the
    // ctx.files/ctx.file SOURCE entries).
    let mk_output = |s: &str| {
        heap.alloc(File {
            path: qualify_output(sess, s),
        })
    };
    let mut outputs_fields: Vec<(String, Value<'v>)> = Vec::new();
    // Implicit outputs: rule(outputs = {"attr": "%{name}.ext"}) — templates expand with the
    // target name into package-qualified Files on ctx.outputs.
    if let Some(d) = starlark::values::dict::DictRef::from_value(out_templates) {
        let name = kwargs
            .iter()
            .find(|(k, _)| k == "name")
            .and_then(|(_, v)| v.unpack_str())
            .unwrap_or_default()
            .to_string();
        for (k, tpl) in d.iter() {
            if let (Some(k), Some(tpl)) = (k.unpack_str(), tpl.unpack_str()) {
                let path = qualify_output(sess, &tpl.replace("%{name}", &name));
                outputs_fields.push((k.to_string(), heap.alloc(File { path })));
            }
        }
    }
    // Typed output attrs first (output → one File, output_list → a list of Files).
    let typed_keys: std::collections::BTreeSet<&str> =
        typed_outputs.iter().map(|(k, _)| k.as_str()).collect();
    for (k, outs) in &typed_outputs {
        if outs.len() == 1 {
            outputs_fields.push((k.clone(), mk_output(&outs[0])));
        } else {
            let files: Vec<Value<'v>> = outs.iter().map(|o| mk_output(o)).collect();
            outputs_fields.push((k.clone(), heap.alloc(files)));
        }
    }
    let kw_outputs: Vec<(String, Value<'v>)> = kwargs
        .iter()
        .filter(|(k, _)| !typed_keys.contains(k.as_str()))
        .filter_map(|(k, v)| v.unpack_str().map(|s| (k.clone(), mk_output(s))))
        .collect();
    outputs_fields.extend(kw_outputs);
    let files_fields: Vec<(String, Value<'v>)> = kwargs
        .iter()
        .filter_map(|(k, v)| {
            if let Some(list) = ListRef::from_value(*v) {
                let items: Vec<Value<'v>> = list
                    .iter()
                    .filter_map(|it| it.unpack_str().map(mk_file))
                    .collect();
                Some((k.clone(), heap.alloc(items)))
            } else {
                // Single-string label attrs feed ctx.file.<attr> too (gentbl's td_file).
                v.unpack_str().map(|s| {
                    let one: Vec<Value<'v>> = vec![mk_file(s)];
                    (k.clone(), heap.alloc(one))
                })
            }
        })
        .collect();
    let ctx = heap.alloc_complex_no_freeze(Ctx {
        attr: heap.alloc(AllocStruct(fields)),
        actions: heap.alloc_complex_no_freeze(Actions),
        // `ctx.label` is a real Label value (.package/.name/.workspace_root; canonical str()).
        label: {
            let cur = sess.current_pkg().unwrap_or_default();
            let (repo, pkg) = match cur.strip_prefix('@') {
                Some(rest) => match rest.split_once("//") {
                    Some((r, p)) => (Some(format!("@{r}")), p.to_string()),
                    None => (None, cur.clone()),
                },
                None => (None, cur.clone()),
            };
            heap.alloc(LabelV {
                repo,
                package: pkg,
                name: name.clone(),
            })
        },
        // Defaulting namespace: an OMITTED output_list attr reads as [] (Bazel: the
        // declared-outputs view; gentbl's additional_outputs).
        outputs: heap.alloc_complex_no_freeze(crate::ctxv::FilesNs {
            fields: outputs_fields,
        }),
        files: heap.alloc_complex_no_freeze(crate::ctxv::FilesNs {
            fields: files_fields.clone(),
        }),
        file: heap.alloc_complex_no_freeze(crate::ctxv::FileNs {
            fields: files_fields,
        }),
        var: heap.alloc(starlark::values::dict::AllocDict([
            (
                heap.alloc("COMPILATION_MODE"),
                heap.alloc(sess.global.mode()),
            ),
            (
                heap.alloc("TARGET_CPU"),
                heap.alloc(crate::state::host_cpu()),
            ),
            (heap.alloc("BINDIR"), heap.alloc("bazel-out/bin")),
            // Standard cc-toolchain Make vars (host-true: empty on this platform).
            (heap.alloc("STACK_FRAME_UNLIMITED"), heap.alloc("")),
        ])),
        executable: heap.alloc_complex_no_freeze(crate::ctxv::ExecNs { fields: Vec::new() }),
        toolchains: toolchain_map(heap, sess),
        build_setting_value: kwargs
            .iter()
            .find(|(k, _)| k == "build_setting_default")
            .map(|(_, v)| *v)
            .unwrap_or_else(Value::new_none),
    });
    let ret = eval.eval_function(implementation, &[ctx], &[])?;

    // L2a: capture the provider instances the impl RETURNED (keyed by canonical label) — what a
    // dependent's `dep[MyInfo]` reads. DefaultInfo/razel_build.info side-effect channels unchanged.
    {
        let mut captured: Vec<(Value<'v>, Value<'v>)> = Vec::new();
        let mut grab = |item: Value<'v>| {
            if let Some(callable) = instance_callable(item) {
                captured.push((callable, item));
            }
        };
        if let Some(list) = ListRef::from_value(ret) {
            for item in list.iter() {
                grab(item);
            }
        } else {
            grab(ret);
        }
        let label = canon_label(session(eval), &name);
        decl_store(eval)?
            .captured
            .borrow_mut()
            .insert(label, captured);
    }

    // Post-impl: commit the analyzed target via record_target (E0d: it also asserts into the
    // Session's live fact store). Take the in-flight target with a short borrow first.
    let committed = sess.take_current_target();
    if let Some(c) = committed {
        record_target(sess, c);
    }
    Ok(())
}


