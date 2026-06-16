//! `decls::store` — split from `decls.rs` (facade in `mod.rs`).

use crate::state::{canon_label, qualify, session};
use allocative::Allocative;
use starlark::any::ProvidesStaticType;
use starlark::coerce::Coerce;
use starlark::collections::SmallMap;
use starlark::environment::Module;
use starlark::eval::{Arguments, Evaluator};
use starlark::starlark_complex_value;
use starlark::values::{
    Freeze, NoSerialize, StarlarkValue, Trace, Value, ValueLifetimeless, ValueLike, starlark_value,
};
use std::cell::RefCell;
use std::fmt;
// ---- rule() + DefaultInfo + select ----------------------------------------------
use super::*;

/// A `rule()` value. Generic over `V` so it has both an unfrozen form (`RuleObj<'v>`,
/// holding a live `Value`) and a frozen form (`FrozenRuleObj`, holding a `FrozenValue`)
/// — which is what lets a rule **survive `module.freeze()`** and therefore be defined
/// in a `.bzl` and `load()`ed, not just inline. The impl function freezes with it.
#[derive(Debug, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub(crate) struct RuleObjGen<V: ValueLifetimeless> {
    pub(crate) implementation: V,
    /// The declared `attrs` schema (name → attr descriptor), frozen with the rule and consulted at
    /// instantiation for defaults / `mandatory` (D1). `None` when no schema was declared.
    pub(crate) attrs: V,
    /// `rule(outputs = {"attr": "%{name}.ext"})` — implicit-output templates; expanded into
    /// `ctx.outputs.<attr>` Files at instantiation. `None` when undeclared.
    pub(crate) outputs: V,
}

starlark_complex_value!(pub(crate) RuleObj);


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
        let kwargs: Vec<(String, Value<'v>)> = named
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), *v))
            .collect();
        let name = kwargs
            .iter()
            .find(|(k, _)| k == "name")
            .and_then(|(_, v)| v.unpack_str())
            .unwrap_or_default()
            .to_string();
        let label = canon_label(sess, &name);
        // Output-file labels resolve statically (Bazel): register `attr.output`/`output_list`
        // values in the output index at DECLARE time, like genrule outs.
        if let Ok((_, attrs, _)) = rule_parts(me)
            && let Some(schema) = starlark::values::dict::DictRef::from_value(attrs)
        {
            for (k, v) in &kwargs {
                let kind = schema
                    .iter()
                    .find(|(kk, _)| kk.unpack_str() == Some(k.as_str()))
                    .and_then(|(_, d)| d.get_attr("kind", eval.heap()).ok().flatten())
                    .and_then(|x| x.unpack_str().map(String::from))
                    .unwrap_or_default();
                if kind == "output" || kind == "output_list" {
                    let outs: Vec<String> = match v.unpack_str() {
                        Some(s) => vec![s.to_string()],
                        None => crate::values::unpack_strs_any(Some(*v)),
                    };
                    let mut idx = sess.output_index.borrow_mut();
                    for o in outs {
                        idx.insert(canon_label(sess, &o), (label.clone(), qualify(sess, &o)));
                    }
                }
            }
        }
        let store = decl_store(eval)?;
        let idx = {
            store
                .originals
                .borrow_mut()
                .insert(label.clone(), (me, kwargs.clone()));
            let mut decls = store.decls.borrow_mut();
            decls.push(Some(Decl {
                label: label.clone(),
                body: DeclBody::Rule { rule: me, kwargs },
            }));
            decls.len() - 1
        };
        sess.pending.borrow_mut().insert(label, idx);
        Ok(Value::new_none())
    }
}

// ---- E0: the phase split — declaration store + demand-driven analysis -------------------------


/// The module variable carrying the harvested provider captures across `module.freeze()` —
/// a plain dict {canonical label: [(constructor, instance)]} (builtin containers freeze natively;
/// the instances are freeze-generic).
pub(crate) const CAPTURED_VAR: &str = "__razel_captured";


/// Harvested UNDRIVEN Starlark declarations of a completed package:
/// {canonical label: (pkg, rule, [(kwarg, value)…])} — analyzed on demand cross-package.
pub(crate) const DEFERRED_VAR: &str = "__razel_deferred_decls";


/// The module variable holding the package's [`DeclStore`] — installed by the analysis entry
/// points before BUILD eval; not addressable from Starlark source.
pub(crate) const DECLS_VAR: &str = "__razel_decls";


/// One recorded rule instantiation, analyzed on demand.
#[derive(Debug, Allocative, Trace)]
pub(crate) struct Decl<'v> {
    pub(crate) label: String,
    pub(crate) body: DeclBody<'v>,
}


/// What analyzing a declaration means: run a Starlark rule (value + raw kwargs) or a deferred
/// native body (an index into `Session.native_decls` — the closure lives off-heap, E0c).
#[derive(Debug, Allocative, Trace)]
pub(crate) enum DeclBody<'v> {
    Rule {
        rule: Value<'v>,
        kwargs: Vec<(String, Value<'v>)>,
    },
    Native(usize),
}


/// The package's recorded declarations — a heap value, so the `'v`-bound rule/kwarg values live on
/// the module heap across the eval→analyze boundary. Slots are `take()`n when analyzed.
#[derive(Debug, Default, ProvidesStaticType, NoSerialize, Allocative, Trace)]
pub(crate) struct DeclStore<'v> {
    pub(crate) decls: RefCell<Vec<Option<Decl<'v>>>>,
    /// Original Starlark rule declarations by label. Analysis takes `decls` slots, but aspects
    /// still need the target's original attrs later to build `ctx.rule.attr`.
    pub(crate) originals: RefCell<SmallMap<String, (Value<'v>, Vec<(String, Value<'v>)>)>>,
    /// L2a: the provider instances each analyzed Starlark target RETURNED (canonical label →
    /// `[(constructor, instance)]`) — what `dep[MyInfo]` indexes. An empty vector is a
    /// known-empty marker, distinct from a missing map entry whose provider capture has not
    /// been seen in this eval.
    pub(crate) captured: RefCell<SmallMap<String, Vec<(Value<'v>, Value<'v>)>>>,
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
    module.set(
        DECLS_VAR,
        module.heap().alloc_complex_no_freeze(DeclStore::default()),
    );
}


/// The store installed on the current module.
pub(crate) fn decl_store<'v>(eval: &Evaluator<'v, '_, '_>) -> anyhow::Result<&'v DeclStore<'v>> {
    let v = eval.module().get(DECLS_VAR).ok_or_else(|| {
        anyhow::anyhow!("rule instantiation outside a package analysis (no declaration store)")
    })?;
    v.downcast_ref::<DeclStore>()
        .ok_or_else(|| anyhow::anyhow!("declaration store has the wrong type"))
}


