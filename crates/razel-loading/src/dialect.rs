//! The rule-authoring API: Ctx, the rule() object, rule_globals (rule/Label/select/providers). C0.

use crate::state::{AnalyzedTarget, canon_label, qualify, session, with_current};
use crate::values::{Actions, Depset, File, extract_files, file_path, unpack};
use crate::glob::do_glob;
use crate::deps::record_target;
use razel_dds::InstanceId;
use allocative::Allocative;
use starlark::any::ProvidesStaticType;
use starlark::coerce::Coerce;
use starlark::collections::SmallMap;
use starlark::environment::{GlobalsBuilder, Module};
use starlark::eval::{Arguments, Evaluator};
use starlark::{starlark_complex_value, starlark_simple_value};
use starlark::values::list::{ListRef, UnpackList};
use starlark::values::tuple::UnpackTuple;
use starlark::values::none::NoneType;
use starlark::values::structs::AllocStruct;
use starlark::values::{
    Freeze, Heap, NoSerialize, StarlarkValue, Trace, Value, ValueLifetimeless, ValueLike,
    starlark_value,
};
use std::cell::RefCell;
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
    /// The declared `attrs` schema (name → attr descriptor), frozen with the rule and consulted at
    /// instantiation for defaults / `mandatory` (D1). `None` when no schema was declared.
    attrs: V,
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
    /// `my_rule(name=…, …)` — RECORD the declaration (E0 phase split). Dep resolution and the
    /// impl run later, in the demand-driven analysis pass ([`drive_decls`]) — which is what makes
    /// forward references within a package resolve (Bazel loads a package before analyzing it).
    fn invoke(
        &self,
        me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let named = args.names_map()?;
        let sess = session(eval);
        let kwargs: Vec<(String, Value<'v>)> =
            named.iter().map(|(k, v)| (k.as_str().to_string(), *v)).collect();
        let name = kwargs
            .iter()
            .find(|(k, _)| k == "name")
            .and_then(|(_, v)| v.unpack_str())
            .unwrap_or_default()
            .to_string();
        let label = canon_label(sess, &name);
        let store = decl_store(eval)?;
        let idx = {
            let mut decls = store.decls.borrow_mut();
            decls.push(Some(Decl { label: label.clone(), body: DeclBody::Rule { rule: me, kwargs } }));
            decls.len() - 1
        };
        sess.pending.borrow_mut().insert(label, idx);
        Ok(Value::new_none())
    }
}


// ---- E0: the phase split — declaration store + demand-driven analysis -------------------------


/// The module variable holding the package's [`DeclStore`] — installed by the analysis entry
/// points before BUILD eval; not addressable from Starlark source.
pub(crate) const DECLS_VAR: &str = "__razel_decls";

/// One recorded rule instantiation, analyzed on demand.
#[derive(Debug, Allocative, Trace)]
struct Decl<'v> {
    label: String,
    body: DeclBody<'v>,
}

/// What analyzing a declaration means: run a Starlark rule (value + raw kwargs) or a deferred
/// native body (an index into `Session.native_decls` — the closure lives off-heap, E0c).
#[derive(Debug, Allocative, Trace)]
enum DeclBody<'v> {
    Rule { rule: Value<'v>, kwargs: Vec<(String, Value<'v>)> },
    Native(usize),
}

/// The package's recorded declarations — a heap value, so the `'v`-bound rule/kwarg values live on
/// the module heap across the eval→analyze boundary. Slots are `take()`n when analyzed.
#[derive(Debug, Default, ProvidesStaticType, NoSerialize, Allocative, Trace)]
pub(crate) struct DeclStore<'v> {
    decls: RefCell<Vec<Option<Decl<'v>>>>,
}

impl fmt::Display for DeclStore<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<decls>")
    }
}

#[starlark_value(type = "razel_decls")]
impl<'v> StarlarkValue<'v> for DeclStore<'v> {}

/// Install an empty declaration store on the module. Every BUILD-eval entry point MUST call this
/// before evaluation and [`drive_decls`] after — rule instantiation without it is an error.
pub(crate) fn install_decl_store<'v>(module: &Module<'v>) {
    module.set(DECLS_VAR, module.heap().alloc_complex_no_freeze(DeclStore::default()));
}

/// The store installed on the current module.
fn decl_store<'v>(eval: &Evaluator<'v, '_, '_>) -> anyhow::Result<&'v DeclStore<'v>> {
    let v = eval.module().get(DECLS_VAR).ok_or_else(|| {
        anyhow::anyhow!("rule instantiation outside a package analysis (no declaration store)")
    })?;
    v.downcast_ref::<DeclStore>()
        .ok_or_else(|| anyhow::anyhow!("declaration store has the wrong type"))
}

/// A rule value's (implementation, attrs-schema), frozen or not.
fn rule_parts<'v>(rule: Value<'v>) -> anyhow::Result<(Value<'v>, Value<'v>)> {
    if let Some(r) = rule.downcast_ref::<RuleObj<'v>>() {
        Ok((r.implementation, r.attrs))
    } else if let Some(r) = rule.downcast_ref::<FrozenRuleObj>() {
        Ok((r.implementation.to_value(), r.attrs.to_value()))
    } else {
        Err(anyhow::anyhow!("declaration's rule is not a rule value"))
    }
}

/// Phase 2: analyze every recorded declaration, in declaration order (demand-recursion may pull a
/// forward-referenced one earlier; its slot is then empty when the loop reaches it).
pub(crate) fn drive_decls<'v>(eval: &mut Evaluator<'v, '_, '_>) -> starlark::Result<()> {
    let mut i = 0;
    loop {
        let n = decl_store(eval)?.decls.borrow().len();
        if i >= n {
            return Ok(());
        }
        analyze_decl(eval, i)?;
        i += 1;
    }
}

/// Ensure `label` (canonical) is analyzed: no-op if already in results; demand-analyze if it is a
/// pending local declaration; otherwise leave it to the caller's existing resolution/error path.
pub(crate) fn ensure_analyzed<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    label: &str,
) -> starlark::Result<()> {
    let idx = {
        let sess = session(eval);
        if sess.results.borrow().contains_key(label) {
            return Ok(());
        }
        if sess.analyzing.borrow().contains(label) {
            return Err(anyhow::anyhow!("dependency cycle detected at `{label}`").into());
        }
        sess.pending.borrow().get(label).copied()
    };
    match idx {
        Some(i) => analyze_decl(eval, i),
        None => Ok(()),
    }
}

/// Analyze the declaration at `idx` (no-op if its slot was already taken). Cycle-guarded via
/// `Session.analyzing`.
fn analyze_decl<'v>(eval: &mut Evaluator<'v, '_, '_>, idx: usize) -> starlark::Result<()> {
    let decl = { decl_store(eval)?.decls.borrow_mut()[idx].take() };
    let Some(decl) = decl else { return Ok(()) };
    {
        let sess = session(eval);
        if !sess.analyzing.borrow_mut().insert(decl.label.clone()) {
            return Err(
                anyhow::anyhow!("dependency cycle detected at `{}`", decl.label).into()
            );
        }
        sess.pending.borrow_mut().remove(&decl.label);
    }
    let res = match &decl.body {
        DeclBody::Rule { rule, kwargs } => analyze_rule_decl(eval, *rule, kwargs),
        DeclBody::Native(nidx) => {
            let f = { session(eval).native_decls.borrow_mut()[*nidx].take() };
            match f {
                Some(f) => f(eval).map_err(Into::into),
                None => Ok(()),
            }
        }
    };
    session(eval).analyzing.borrow_mut().remove(&decl.label);
    res
}

/// Record a deferred native-rule analysis (E0c): the rule fn extracts its plain attrs at eval time
/// and hands the body here; the demand-driven pass runs it — so native targets forward-reference
/// (and interleave with Starlark-rule targets) like everything else.
pub(crate) fn record_native<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    label: String,
    f: crate::state::NativeAnalyzeFn,
) -> anyhow::Result<()> {
    let sess = session(eval);
    let nidx = {
        let mut v = sess.native_decls.borrow_mut();
        v.push(Some(f));
        v.len() - 1
    };
    let store = decl_store(eval)?;
    let idx = {
        let mut decls = store.decls.borrow_mut();
        decls.push(Some(Decl { label: label.clone(), body: DeclBody::Native(nidx) }));
        decls.len() - 1
    };
    sess.pending.borrow_mut().insert(label, idx);
    Ok(())
}

/// The analysis of one Starlark-rule declaration — dep resolution (demand-driven), schema
/// defaults/`mandatory`, ctx construction, and the impl call. (Ran inside `invoke()` before E0.)
fn analyze_rule_decl<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    rule: Value<'v>,
    kwargs: &[(String, Value<'v>)],
) -> starlark::Result<()> {
    let (implementation, attrs) = rule_parts(rule)?;
    let mut name = String::new();
    let mut dep_labels: Vec<String> = Vec::new();
    let mut fields: Vec<(String, Value<'v>)> = Vec::new();
    for (key, v) in kwargs {
        let (key, v) = (key.clone(), *v);
        // D1b/c: the schema kind drives label resolution. Look it up once: `label`/`label_list`
        // resolve to provider struct(s); the legacy `deps` is an implicit `label_list`.
        let attr_kind: Option<String> = {
            let mut k = None;
            if let Some(d) = starlark::values::dict::DictRef::from_value(attrs) {
                for (kk, desc) in d.iter() {
                    if kk.unpack_str() == Some(key.as_str()) {
                        if let Ok(Some(kind)) = desc.get_attr("kind", eval.heap()) {
                            k = kind.unpack_str().map(String::from);
                        }
                        break;
                    }
                }
            }
            k
        };
        let is_label =
            key == "deps" || matches!(attr_kind.as_deref(), Some("label") | Some("label_list"));
        let single_label = attr_kind.as_deref() == Some("label");
        match key.as_str() {
            "name" => {
                name = v.unpack_str().unwrap_or_default().to_string();
                fields.push((key, v));
            }
            // A label attr (legacy `deps` or any `attr.label_list`): resolve each label to its
            // analyzed providers as a `struct(files=…, <folded fields>…)`.
            _ if is_label => {
                // Collect the label string(s): a list (`label_list`/`deps`) or a single (`label`).
                let labels: Vec<String> = if let Some(list) = ListRef::from_value(v) {
                    list.iter().filter_map(|x| x.unpack_str().map(String::from)).collect()
                } else if let Some(s) = v.unpack_str() {
                    vec![s.to_string()]
                } else {
                    Vec::new()
                };
                let mut structs: Vec<Value<'v>> = Vec::new();
                for label in &labels {
                    // Canonical label — bare in single-package mode, //pkg:name in a workspace.
                    let dep = canon_label(session(eval), label);
                    // E0: a forward-referenced local declaration analyzes on demand, first.
                    ensure_analyzed(eval, &dep)?;
                    let sess = session(eval);
                    let results = sess.results.borrow();
                    let Some(dep_target) = results.get(&dep) else {
                        return Err(anyhow::anyhow!(
                            "dep `{dep}` is not declared in this package \
                             (or its package failed to load)"
                        )
                        .into());
                    };
                    // C2c: transitive provider closures come from the ONE razel-dds fold. (E0b
                    // transitional: per-dep snapshot rebuild — E0d makes the Session own the Dds.)
                    let dds = crate::dds::to_dds(
                        &results.values().cloned().collect::<Vec<_>>(),
                        InstanceId::SINGLE,
                    )
                    .map_err(|e| anyhow::anyhow!(e))?;
                    let tkey = crate::dds::target_key(InstanceId::SINGLE, &dep)
                        .map_err(|e| anyhow::anyhow!(e))?;
                    // `files` is own-exposed (DefaultInfo); transitive fields fold via the ONE
                    // registry-driven helper (shared with the native path) — no hardcoded cc/java.
                    let mut sfields: Vec<(String, Vec<String>)> =
                        vec![("files".to_string(), dep_target.default_info.clone())];
                    sfields.extend(crate::dds::fold_dep_fields(&dds, &tkey));
                    dep_labels.push(dep);
                    structs.push(eval.heap().alloc(AllocStruct(sfields)));
                }
                // D1c: a single `attr.label` yields ONE struct; a list yields the list of structs.
                if single_label {
                    fields.push((key, structs.into_iter().next().unwrap_or_else(Value::new_none)));
                } else {
                    fields.push((key, eval.heap().alloc(structs)));
                }
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
            let Some(an) = aname.unpack_str() else { continue };
            if an == "name" || passed.contains(an) {
                continue;
            }
            let default =
                descriptor.get_attr("default", eval.heap())?.unwrap_or_else(Value::new_none);
            if !default.is_none() {
                fields.push((an.to_string(), default));
            } else if descriptor
                .get_attr("mandatory", eval.heap())?
                .and_then(|m| m.unpack_bool())
                .unwrap_or(false)
            {
                return Err(anyhow::anyhow!("mandatory attribute `{an}` not provided").into());
            }
        }
    }

    let sess = session(eval);
    let heap = eval.heap();
    sess.state.borrow_mut().current = Some(AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: dep_labels,
            ..Default::default()
        });

    // ctx.outputs.<attr> — string-valued attrs are predeclared output filenames
    // (package-qualified). ctx.files.<attr> — list-valued attrs are source files
    // (qualified). ctx.executable is empty until an executable-label attr is wired.
    let mk_file = |s: &str| heap.alloc_complex_no_freeze(File { path: qualify(sess, s) });
    let outputs_fields: Vec<(String, Value<'v>)> = kwargs
        .iter()
        .filter_map(|(k, v)| v.unpack_str().map(|s| (k.clone(), mk_file(s))))
        .collect();
    let files_fields: Vec<(String, Value<'v>)> = kwargs
        .iter()
        .filter_map(|(k, v)| {
            ListRef::from_value(*v).map(|list| {
                let items: Vec<Value<'v>> = list
                    .iter()
                    .filter_map(|it| it.unpack_str().map(mk_file))
                    .collect();
                (k.clone(), heap.alloc(items))
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
    eval.eval_function(implementation, &[ctx], &[])?;

    // Post-impl: commit the analyzed target. `sess` is still valid — it borrows the extra
    // target, not `eval`. Short, non-overlapping borrows.
    {
        let mut st = sess.state.borrow_mut();
        if let Some(c) = st.current.take() {
            sess.results.borrow_mut().insert(c.name.clone(), c.clone());
            st.targets.push(c);
        }
    }
    Ok(())
}


/// A `provider()` value (D4.2): a callable that constructs a provider instance. Calling it with
/// kwargs (`MyInfo(msg = "hi")`) yields a `struct` of those fields — the instance, read locally
/// (`info.msg`) or, later, captured from a rule's return + read off a dep (`dep[MyInfo]`, D4.3+).
/// Holds only field names (Bazel `provider(fields=…)`), so it freezes trivially and survives a `.bzl`
/// `load()` (`BuildSettingInfo = provider(...)`).
#[derive(Debug, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct ProviderCallable {
    fields: Vec<String>,
}
starlark_simple_value!(ProviderCallable);

impl fmt::Display for ProviderCallable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<provider>")
    }
}

#[starlark_value(type = "provider")]
impl<'v> StarlarkValue<'v> for ProviderCallable {
    /// `MyInfo(field = value, …)` → a `struct` carrying those fields (the provider instance).
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let _ = &self.fields; // declared field names (validation not yet enforced)
        let named = args.names_map()?;
        let fields: Vec<(String, Value<'v>)> =
            named.iter().map(|(k, v)| (k.as_str().to_string(), *v)).collect();
        Ok(eval.heap().alloc(AllocStruct(fields)))
    }
}


#[allow(non_snake_case)]
#[starlark::starlark_module]
pub(crate) fn rule_globals(b: &mut GlobalsBuilder) {
    /// `rule(implementation, attrs={})` → a callable rule object.
    fn rule<'v>(
        #[starlark(require = named)] implementation: Value<'v>,
        #[starlark(require = named)] attrs: Option<Value<'v>>,
        // D4: absorb the other rule() kwargs upstream rules pass (build_setting, doc, cfg, toolchains,
        // provides, executable, test, …). Not yet honored — enough to define the rule.
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // D1: keep the declared schema (was discarded) — instantiation consults it for defaults +
        // mandatory. alloc (freezable) so the rule survives module.freeze() (defined in a .bzl + load()ed).
        let attrs = attrs.unwrap_or_else(Value::new_none);
        Ok(eval.heap().alloc(RuleObjGen { implementation, attrs }))
    }

    /// `provider(doc=?, fields=?, ...)` → a callable provider constructor (D4.2). Args are absorbed
    /// (fields validation not yet enforced); calling the result builds the instance struct.
    fn provider<'v>(
        #[starlark(args)] _args: UnpackTuple<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(eval.heap().alloc(ProviderCallable { fields: Vec::new() }))
    }

    /// Bazel builtin global providers `RunEnvironmentInfo(...)` / `OutputGroupInfo(...)` (D4 compat):
    /// construct an instance `struct` from the kwargs. Stubs so real upstream rules resolve + run; the
    /// instances aren't yet captured/consumed (D4.3+).
    fn RunEnvironmentInfo<'v>(
        #[starlark(kwargs)] kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let fields: Vec<(String, Value<'v>)> =
            kw.iter().map(|(k, v)| (k.clone(), *v)).collect();
        Ok(eval.heap().alloc(AllocStruct(fields)))
    }
    fn OutputGroupInfo<'v>(
        #[starlark(kwargs)] kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let fields: Vec<(String, Value<'v>)> =
            kw.iter().map(|(k, v)| (k.clone(), *v)).collect();
        Ok(eval.heap().alloc(AllocStruct(fields)))
    }

    /// `transition(implementation, inputs, outputs)` (D4 compat stub): real rules define config
    /// transitions + pass them as `rule(cfg=…)`. razel doesn't apply transitions yet — absorb the args
    /// so the rule defines; `rule()` already absorbs the `cfg` kwarg.
    fn transition<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// `configuration_field(fragment, name)` (D4 compat stub): a late-bound default razel doesn't model
    /// — absorb the args; the attr's default becomes `None`.
    fn configuration_field<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
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
        // C3a.5: filegroup provides DefaultInfo only — it no longer fakes CcInfo to push files into
        // the cc header channel (that hack was untested; a real cc-on-filegroup case = cc reading dep
        // DefaultInfo files as inputs, Phase D).
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            default_info: files,
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

    // C3 (`razel_build.info`): the cc/java provider-capture builtins are gone — razel's `.bzl` rules
    // capture providers through the ONE generic `razel_build.info(provider, fields)` constructor
    // (engine.rs), schema-driven by the registry. No language-named capture builtin in the rule API.

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

