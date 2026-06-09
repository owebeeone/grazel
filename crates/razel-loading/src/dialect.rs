//! The rule-authoring API: Ctx, the rule() object, rule_globals (rule/Label/select/providers). C0.

use crate::state::{AnalyzedTarget, canon_label, qualify, session, with_current};
use crate::values::{Actions, Depset, File, extract_files, file_path, unpack};
use crate::glob::do_glob;
use crate::deps::record_target;
use razel_dds::{DdsRead, FieldKind, FieldValue, InstanceId, Scalar};
use allocative::Allocative;
use starlark::any::ProvidesStaticType;
use starlark::coerce::Coerce;
use starlark::collections::SmallMap;
use starlark::environment::GlobalsBuilder;
use starlark::eval::{Arguments, Evaluator};
use starlark::starlark_complex_value;
use starlark::values::list::{ListRef, UnpackList};
use starlark::values::none::NoneType;
use starlark::values::structs::AllocStruct;
use starlark::values::{
    Freeze, Heap, NoSerialize, StarlarkValue, Trace, Value, ValueLifetimeless, ValueLike,
    starlark_value,
};
use std::fmt;



// ---- ctx ------------------------------------------------------------------------


/// The analysis `ctx`. All fields are heap `Value`s so it traces cleanly, no freezing.
#[derive(Debug, NoSerialize, ProvidesStaticType, Allocative, Trace)]
pub(crate) struct Ctx<'v> {
    attr: Value<'v>,
    actions: Value<'v>,
    label: Value<'v>,
    /// `ctx.outputs.<name>` — predeclared output filenames (package-qualified).
    outputs: Value<'v>,
    /// `ctx.files.<name>` — the source files of label/label_list attrs (qualified).
    files: Value<'v>,
    /// `ctx.executable.<name>` — the runnable output of an executable-label attr.
    executable: Value<'v>,
}


impl fmt::Display for Ctx<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<ctx>")
    }
}


#[starlark_value(type = "ctx")]
impl<'v> StarlarkValue<'v> for Ctx<'v> {
    fn get_attr(&self, attribute: &str, _heap: Heap<'v>) -> Option<Value<'v>> {
        match attribute {
            "attr" => Some(self.attr),
            "actions" => Some(self.actions),
            "label" => Some(self.label),
            "outputs" => Some(self.outputs),
            "files" => Some(self.files),
            "executable" => Some(self.executable),
            _ => None,
        }
    }
}


// ---- rule() + DefaultInfo + select ----------------------------------------------


/// A `rule()` value. Generic over `V` so it has both an unfrozen form (`RuleObj<'v>`,
/// holding a live `Value`) and a frozen form (`FrozenRuleObj`, holding a `FrozenValue`)
/// — which is what lets a rule **survive `module.freeze()`** and therefore be defined
/// in a `.bzl` and `load()`ed, not just inline. The impl function freezes with it.
#[derive(Debug, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub(crate) struct RuleObjGen<V: ValueLifetimeless> {
    implementation: V,
}
starlark_complex_value!(RuleObj);

impl<V: ValueLifetimeless> fmt::Display for RuleObjGen<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<rule>")
    }
}


#[starlark_value(type = "rule")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for RuleObjGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    /// `my_rule(name=…, …)` — build a `ctx` and run the impl (same-scope analysis).
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let named = args.names_map()?;
        let heap = eval.heap();
        let sess = session(eval);
        let mut name = String::new();
        let mut dep_labels: Vec<String> = Vec::new();
        let mut fields: Vec<(String, Value<'v>)> = Vec::new();
        for (k, v) in &named {
            let key = k.as_str().to_string();
            match key.as_str() {
                "name" => {
                    name = v.unpack_str().unwrap_or_default().to_string();
                    fields.push((key, *v));
                }
                // Two-phase provider flow: resolve each dep label to its analyzed
                // DefaultInfo (from the results registry) as a `struct(files = [...])`.
                "deps" => {
                    let mut providers: Vec<Value<'v>> = Vec::new();
                    if let Some(list) = ListRef::from_value(*v) {
                        let results = sess.results.borrow();
                        // C2c: transitive provider closures come from the ONE razel-dds fold
                        // (`DdsRead` over a fact store built from the analyzed-so-far targets) — the
                        // parallel loader traversal is gone. cc `.headers` = `Set` fold; java
                        // `.compile_jars` = `OrderedDepset`; `.runtime_jars` = the same, neverlink-
                        // subtree-pruned (the prune the `neverlink` Scalar drives).
                        let dds = crate::dds::to_dds(
                            &results.values().cloned().collect::<Vec<_>>(),
                            InstanceId::SINGLE,
                        )
                        .map_err(|e| anyhow::anyhow!(e))?;
                        let to_strs = |xs: Vec<Scalar>| -> Vec<String> {
                            xs.into_iter().filter_map(|s| match s {
                                Scalar::Str(x) => Some(x),
                                _ => None,
                            }).collect()
                        };
                        let registry = crate::registry::builtin_registry();
                        for item in list.iter() {
                            let label = item.unpack_str().unwrap_or_default();
                            // Key by canonical label — bare in single-package mode,
                            // //pkg:name in a workspace (matches the native rules).
                            let dep = canon_label(sess, label);
                            let Some(dep_target) = results.get(&dep) else {
                                return Err(anyhow::anyhow!(
                                    "dep `{dep}` not analyzed yet — declare it before its users \
                                     (forward references not yet supported)"
                                )
                                .into());
                            };
                            let key = crate::dds::target_key(InstanceId::SINGLE, &dep)
                                .map_err(|e| anyhow::anyhow!(e))?;
                            // C3a.3: `files` is own-exposed (DefaultInfo); every transitive field folds
                            // via the registry — no hardcoded cc/java provider or field names here. The
                            // fold (Set / OrderedDepset / pruned) + the dep-struct name come from the spec.
                            let mut sfields: Vec<(String, Vec<String>)> =
                                vec![("files".to_string(), dep_target.default_info.clone())];
                            for ty in registry.provider_types() {
                                for (field, kind, depfold) in registry.dep_folds(ty) {
                                    use crate::registry::FoldPolicy;
                                    let folded = match (kind, &depfold.policy) {
                                        (FieldKind::Set, _) => {
                                            to_strs(dds.fold_set(&key, ty, field).into_iter().collect())
                                        }
                                        (FieldKind::OrderedDepset, FoldPolicy::Plain) => {
                                            to_strs(dds.fold_depset(&key, ty, field))
                                        }
                                        (FieldKind::OrderedDepset, FoldPolicy::PrunedBy(p)) => {
                                            to_strs(dds.fold_depset_pruned(&key, ty, field, p))
                                        }
                                        (FieldKind::Scalar, _) => continue,
                                    };
                                    sfields.push((depfold.projection.to_string(), folded));
                                }
                            }
                            dep_labels.push(dep);
                            providers.push(heap.alloc(AllocStruct(sfields)));
                        }
                    }
                    fields.push((key, heap.alloc(providers)));
                }
                _ => fields.push((key, *v)),
            }
        }

        sess.state.borrow_mut().current = Some(AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: dep_labels,
            ..Default::default()
        });

        // ctx.outputs.<attr> — string-valued attrs are predeclared output filenames
        // (package-qualified). ctx.files.<attr> — list-valued attrs are source files
        // (qualified). ctx.executable is empty until an executable-label attr is wired.
        // (razel resolves attribute values directly; the schema is not consulted.)
        let mk_file = |s: &str| heap.alloc_complex_no_freeze(File { path: qualify(sess, s) });
        let outputs_fields: Vec<(String, Value<'v>)> = named
            .iter()
            .filter_map(|(k, v)| v.unpack_str().map(|s| (k.as_str().to_string(), mk_file(s))))
            .collect();
        let files_fields: Vec<(String, Value<'v>)> = named
            .iter()
            .filter_map(|(k, v)| {
                ListRef::from_value(*v).map(|list| {
                    let items: Vec<Value<'v>> = list
                        .iter()
                        .filter_map(|it| it.unpack_str().map(mk_file))
                        .collect();
                    (k.as_str().to_string(), heap.alloc(items))
                })
            })
            .collect();
        let ctx = heap.alloc_complex_no_freeze(Ctx {
            attr: heap.alloc(AllocStruct(fields)),
            actions: heap.alloc_complex_no_freeze(Actions),
            // `ctx.label` is a Label struct (`.package`/`.name`) — razel's cc:defs.bzl reads them
            // for the path model. package is the current pkg (empty in single-package mode).
            label: {
                let pkg = sess.current_pkg.borrow().clone().unwrap_or_default();
                heap.alloc(AllocStruct([
                    ("package".to_string(), heap.alloc(pkg)),
                    ("name".to_string(), heap.alloc(name.clone())),
                ]))
            },
            outputs: heap.alloc(AllocStruct(outputs_fields)),
            files: heap.alloc(AllocStruct(files_fields)),
            executable: heap.alloc(AllocStruct(Vec::<(String, Value<'v>)>::new())),
        });
        eval.eval_function(self.implementation.to_value(), &[ctx], &[])?;

        // Post-eval (after the nested rule impl ran): commit the analyzed target. `sess` is
        // still valid — it borrows the extra target, not `eval`. Short, non-overlapping borrows.
        {
            let mut st = sess.state.borrow_mut();
            if let Some(c) = st.current.take() {
                sess.results.borrow_mut().insert(c.name.clone(), c.clone());
                st.targets.push(c);
            }
        }
        Ok(Value::new_none())
    }
}


#[allow(non_snake_case)]
#[starlark::starlark_module]
pub(crate) fn rule_globals(b: &mut GlobalsBuilder) {
    /// `rule(implementation, attrs={})` → a callable rule object.
    fn rule<'v>(
        #[starlark(require = named)] implementation: Value<'v>,
        #[starlark(require = named)] attrs: Option<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let _ = attrs;
        // alloc (freezable) — the rule survives module.freeze(), so it can be
        // defined in a .bzl and load()ed, not just used inline.
        Ok(eval.heap().alloc(RuleObjGen { implementation }))
    }

    /// `Label("//pkg:name")` — a minimal Label exposing `.package`/`.name`/
    /// `.workspace_root`/`.workspace_name`. razel treats everything as the main repo,
    /// so workspace_root/workspace_name are empty (matching Bazel on the main repo).
    fn Label<'v>(
        #[starlark(require = pos)] s: String,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let after = s.rsplit_once("//").map(|(_, a)| a).unwrap_or(s.as_str());
        let (pkg, name) = match after.split_once(':') {
            Some((p, n)) => (p.to_string(), n.to_string()),
            None => (
                after.to_string(),
                after.rsplit('/').next().unwrap_or(after).to_string(),
            ),
        };
        let heap = eval.heap();
        Ok(heap.alloc(AllocStruct([
            ("package".to_string(), heap.alloc(pkg)),
            ("name".to_string(), heap.alloc(name)),
            ("workspace_root".to_string(), heap.alloc(String::new())),
            ("workspace_name".to_string(), heap.alloc(String::new())),
        ])))
    }

    /// BUILD package-declaration builtins. razel doesn't enforce visibility/licenses
    /// and tracks no separate file-export set, so these are no-op declarations —
    /// recognized so real BUILD files evaluate. (`package`, `package_group`,
    /// `licenses`, `exports_files`.)
    fn package<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn package_group<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn licenses<'v>(
        #[starlark(require = pos)] _licenses: UnpackList<String>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn exports_files<'v>(
        #[starlark(require = pos)] _files: UnpackList<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// Native build-graph builtins razel recognizes so real BUILD files evaluate.
    /// `config_setting`/`test_suite`/`alias` carry no buildable output here, so they
    /// record an empty target under their label (analysis-visible, builds to nothing);
    /// `filegroup` forwards its `srcs` as its outputs so dependents resolve them.
    fn config_setting<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }
    fn test_suite<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }
    fn alias<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }
    fn filegroup<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let files: Vec<String> = unpack(srcs).iter().map(|s| qualify(sess, s)).collect();
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            default_info: files.clone(),
            providers: crate::state::cc_provider_map(files, Vec::new()),
            ..Default::default()
        });
        Ok(NoneType)
    }

    /// `DefaultInfo(files=…)` — the standard output provider. `files` may be a list
    /// (of Files/strings) or a `depset`; other kwargs absorbed.
    fn DefaultInfo<'v>(
        #[starlark(require = named)] files: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        if let Some(f) = files {
            let paths = extract_files(f);
            with_current(session(eval), |c| c.default_info = paths);
        }
        Ok(NoneType)
    }

    /// `CcInfo(headers=…)` — the cc provider. razel captures the target's OWN exported headers into
    /// `hdrs`; the transitive set a dependent sees is recovered by folding over deps
    /// ([`fold_headers`]). Other kwargs absorbed. (A2a: the rule()-return providers are captured by
    /// side-effect, mirroring `DefaultInfo`.)
    fn CcInfo<'v>(
        #[starlark(require = named)] headers: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        if let Some(h) = headers {
            let paths = extract_files(h);
            with_current(session(eval), |c| {
                c.set_provider(
                    "CcInfo",
                    "hdrs",
                    FieldValue::Set(paths.into_iter().map(Scalar::Str).collect()),
                );
            });
        }
        Ok(NoneType)
    }

    /// `JavaInfo(compile_jars=…)` — the java provider (B3 spike). razel captures the target's OWN
    /// exported compile jar(s) (the header/ijar) into `compile_jars`, **ordered**; a dependent's
    /// classpath is the preorder fold over deps ([`fold_compile_jars`] — the OrderedDepset analog).
    /// Other kwargs (runtime_jars, …) absorbed. Mirrors `CcInfo`, the SECOND hardcoded provider — the
    /// B5 ledger signal that Phase C should generalize provider-capture to a schema-driven map.
    fn JavaInfo<'v>(
        #[starlark(require = named)] compile_jars: Option<Value<'v>>,
        #[starlark(require = named)] runtime_jars: Option<Value<'v>>,
        #[starlark(require = named)] neverlink: Option<bool>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        // Two SEPARATE ordered depsets (compile vs runtime) + the neverlink conditional (B4).
        let compile = compile_jars.map(extract_files);
        let runtime = runtime_jars.map(extract_files);
        let nl = neverlink.unwrap_or(false);
        let odep = |j: Vec<String>| FieldValue::OrderedDepset(j.into_iter().map(Scalar::Str).collect());
        with_current(session(eval), |c| {
            if let Some(j) = compile {
                c.set_provider("JavaInfo", "compile_jars", odep(j));
            }
            if let Some(j) = runtime {
                c.set_provider("JavaInfo", "runtime_jars", odep(j));
            }
            c.set_provider("JavaInfo", "neverlink", FieldValue::Scalar(Scalar::Bool(nl)));
        });
        Ok(NoneType)
    }

    /// `dedup(list)` — the list with duplicate strings removed, **preserving first occurrence**. The
    /// cross-sibling dedup the rule()-path classpath/header assembly needs: a target's transitive
    /// closure folds *per-dep*, so a diamond (`app -> [x,y] -> base`) would otherwise list base's
    /// jar/header twice. `dedup()` makes the merge the engine's job, not silent duplication (F1).
    fn dedup(#[starlark(require = pos)] list: UnpackList<String>) -> anyhow::Result<Vec<String>> {
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for s in list.items {
            if seen.insert(s.clone()) {
                out.push(s);
            }
        }
        Ok(out)
    }

    /// `depset(direct=[], transitive=[depsets], order=…)` — Bazel's transitive set.
    /// razel folds member paths from `direct` + each `transitive` depset, deduped.
    fn depset<'v>(
        #[starlark(require = pos)] direct: Option<Value<'v>>,
        #[starlark(require = named)] transitive: Option<Value<'v>>,
        #[starlark(require = named)] order: Option<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // razel does not model depset traversal order yet (the 4-order family is reserved). WARN
        // loudly on a non-default order rather than silently produce a possibly-wrong sequence (F36).
        if let Some(o) = &order
            && o != "default"
        {
            eprintln!(
                "razel: warning: depset(order={o:?}) — traversal order not yet modeled, treating as \
                 default (F36; RazelGaps)"
            );
        }
        let mut items: Vec<String> = Vec::new();
        let push = |s: String, items: &mut Vec<String>| {
            if !items.contains(&s) {
                items.push(s);
            }
        };
        if let Some(d) = direct
            && let Some(list) = ListRef::from_value(d)
        {
            for it in list.iter() {
                push(file_path(it), &mut items);
            }
        }
        if let Some(t) = transitive
            && let Some(list) = ListRef::from_value(t)
        {
            for dep in list.iter() {
                if let Some(ds) = dep.downcast_ref::<Depset>() {
                    for s in &ds.items {
                        push(s.clone(), &mut items);
                    }
                }
            }
        }
        Ok(eval.heap().alloc_complex_no_freeze(Depset { items }))
    }

    /// `select({cond: value, …})` — host-config-lite: pick `//conditions:default`, else
    /// the first branch. (Real config_setting matching is Phase 8.)
    fn select<'v>(branches: SmallMap<String, Value<'v>>) -> anyhow::Result<Value<'v>> {
        if let Some(v) = branches.get("//conditions:default") {
            return Ok(*v);
        }
        branches
            .values()
            .next()
            .copied()
            .ok_or_else(|| anyhow::anyhow!("select() with no branches"))
    }

    /// `define_config(name, compile, archive=None, link=None)` — declare + register a
    /// toolchain transform (D7). Returns a struct of the transform fns (so a rule can call
    /// `cfg.compile(req)`); also records the name engine-side for host-config selection.
    fn define_config<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] compile: Value<'v>,
        #[starlark(require = named)] archive: Option<Value<'v>>,
        #[starlark(require = named)] link: Option<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        session(eval).configs.borrow_mut().push(name);
        let mut fields: Vec<(String, Value<'v>)> = vec![("compile".to_string(), compile)];
        if let Some(a) = archive {
            fields.push(("archive".to_string(), a));
        }
        if let Some(l) = link {
            fields.push(("link".to_string(), l));
        }
        Ok(eval.heap().alloc(AllocStruct(fields)))
    }

    /// `glob(include, exclude=[])` — match the current package's files (workspace
    /// mode) against the patterns, returning package-relative paths. Requires a
    /// package on disk; errors in single-package (no-dir) mode.
    fn glob<'v>(
        #[starlark(require = pos)] include: UnpackList<String>,
        #[starlark(require = named)] exclude: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Vec<String>> {
        do_glob(session(eval), include.items, exclude.map(|l| l.items).unwrap_or_default())
    }
}

