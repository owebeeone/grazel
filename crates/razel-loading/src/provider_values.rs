//! provider()/instances/raw ctors + DepTarget (the dep view a rule impl sees).

use allocative::Allocative;
use starlark::any::ProvidesStaticType;
use starlark::coerce::Coerce;
use starlark::eval::{Arguments, Evaluator};
use starlark::starlark_complex_value;
use starlark::values::{
    Freeze, Heap, NoSerialize, StarlarkValue, Trace, Value, ValueLifetimeless, ValueLike,
    starlark_value,
};
use std::fmt;


/// A `provider()` value (D4.2/L2): a callable constructing provider instances. With `init=`
/// (Bazel's `CcInfo, _raw = provider(init = f)` shape) the kwargs route through `init` (which
/// returns the field dict) and `provider()` returns a 2-tuple `(Provider, raw_ctor)`. Generic
/// over `V` (the `RuleObjGen` pattern) so a `.bzl`-defined provider survives `module.freeze()`.
#[derive(Debug, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub(crate) struct ProviderCallableGen<V: ValueLifetimeless> {
    /// The `init` callback (Starlark `None` ⇒ kwargs are the fields directly).
    pub(crate) init: V,
}


starlark_complex_value!(pub(crate) ProviderCallable);


impl<V: ValueLifetimeless> fmt::Display for ProviderCallableGen<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<provider>")
    }
}


#[starlark_value(type = "provider")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for ProviderCallableGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    /// `MyInfo(field = value, …)` → a [`ProviderInstance`]. The instance remembers its
    /// constructor (`me`) — that identity is what `dep[MyInfo]` indexes by (L2a). With `init`,
    /// the kwargs go through it and its returned dict becomes the fields.
    fn invoke(
        &self,
        me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let named = args.names_map()?;
        let kwargs: Vec<(String, Value<'v>)> =
            named.iter().map(|(k, v)| (k.as_str().to_string(), *v)).collect();
        let pos: Vec<Value<'v>> = args.positions(eval.heap())?.collect();
        let init = self.init.to_value();
        let fields = if !init.is_none() {
            {
                // The call's args go to `init` VERBATIM (its signature is the contract —
                // positional args are legal; rules_cc calls `_ArtifactCategoryInfo("X", …)`).
                let named_refs: Vec<(&str, Value<'v>)> =
                    kwargs.iter().map(|(k, v)| (k.as_str(), *v)).collect();
                let dict = eval.eval_function(init, &pos, &named_refs)?;
                let Some(d) = starlark::values::dict::DictRef::from_value(dict) else {
                    return Err(anyhow::anyhow!(
                        "provider init callback must return a dict of fields"
                    )
                    .into());
                };
                d.iter()
                    .filter_map(|(k, v)| k.unpack_str().map(|k| (k.to_string(), v)))
                    .collect()
            }
        } else {
            if !pos.is_empty() {
                return Err(anyhow::anyhow!(
                    "providers take field values as keyword arguments (no init= declared)"
                )
                .into());
            }
            kwargs
        };
        Ok(eval.heap().alloc(ProviderInstance { callable: me, fields }))
    }
}


/// The raw constructor from `provider(init=…)` — builds instances of the SAME provider
/// (instances carry the CANONICAL provider's identity, so `dep[P]` finds raw-made ones),
/// bypassing `init`.
#[derive(Debug, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub(crate) struct RawCtorGen<V: ValueLifetimeless> {
    pub(crate) canonical: V,
}


starlark_complex_value!(pub(crate) RawCtor);


impl<V: ValueLifetimeless> fmt::Display for RawCtorGen<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<provider raw constructor>")
    }
}


#[starlark_value(type = "provider_raw_constructor")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for RawCtorGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let named = args.names_map()?;
        let fields: Vec<(String, Value<'v>)> =
            named.iter().map(|(k, v)| (k.as_str().to_string(), *v)).collect();
        Ok(eval
            .heap()
            .alloc(ProviderInstance { callable: self.canonical.to_value(), fields }))
    }
}


/// A constructed provider instance (L2a): the fields plus the constructor's identity. Field reads
/// (`info.msg`) go through `get_attr`; a rule impl returning instances gets them captured onto the
/// target, and a dependent retrieves them via `dep[MyInfo]` (identity = same constructor value).
/// Freeze-generic: real `.bzl` construct instances at MODULE level (rules_cc's artifact
/// categories), so instances must survive `module.freeze()`.
#[derive(Debug, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub(crate) struct ProviderInstanceGen<V: ValueLifetimeless> {
    callable: V,
    fields: Vec<(String, V)>,
}


starlark_complex_value!(pub(crate) ProviderInstance);


impl<V: ValueLifetimeless> fmt::Display for ProviderInstanceGen<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<provider instance>")
    }
}


#[starlark_value(type = "provider_instance")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for ProviderInstanceGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn get_attr(&self, attribute: &str, _heap: Heap<'v>) -> Option<Value<'v>> {
        // Unset fields read as None (razel doesn't track the DECLARED field list, so
        // partially-constructed instances — `CcInfo().linking_context` — default rather than
        // error; undeclared-field misuse is not caught. Registered debt.)
        Some(
            self.fields
                .iter()
                .find(|(k, _)| k == attribute)
                .map(|(_, v)| v.to_value())
                .unwrap_or_else(Value::new_none),
        )
    }
}


/// The constructor identity of a provider-instance value (frozen or live), for `dep[P]` capture.
pub(crate) fn instance_callable<'v>(item: Value<'v>) -> Option<Value<'v>> {
    if let Some(pi) = item.downcast_ref::<ProviderInstance<'v>>() {
        Some(pi.callable)
    } else if let Some(pi) = item.downcast_ref::<FrozenProviderInstance>() {
        Some(pi.callable.to_value())
    } else {
        None
    }
}


/// A resolved dependency as seen by a rule impl (L2a): the plain projected fields (`files`,
/// `headers`, …) via `get_attr`, plus `dep[MyInfo]` indexing into the dep's captured provider
/// instances (constructor identity — `Value::ptr_eq`).
#[derive(Debug, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub(crate) struct DepTargetGen<V: ValueLifetimeless> {
    /// The dep's canonical label string (`.label` synthesizes a Label value from it).
    pub(crate) label: String,
    pub(crate) fields: Vec<(String, V)>,
    pub(crate) providers: Vec<(V, V)>,
}


starlark_complex_value!(pub(crate) DepTarget);


impl<V: ValueLifetimeless> fmt::Display for DepTargetGen<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<dep>")
    }
}


#[starlark_value(type = "dep_target")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for DepTargetGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn get_attr(&self, attribute: &str, _heap: Heap<'v>) -> Option<Value<'v>> {
        if attribute == "label" {
            let (repo, rest) = match self.label.split_once("//") {
                Some((r, rest)) if r.starts_with('@') => (Some(r.to_string()), rest),
                Some((_, rest)) => (None, rest),
                None => (None, self.label.as_str()),
            };
            let (pkg, name) = rest.split_once(':').unwrap_or(("", rest));
            return Some(_heap.alloc(crate::labels::LabelV {
                repo,
                package: pkg.to_string(),
                name: name.to_string(),
            }));
        }
        // FilesToRunProvider shape: executable = the dep's first file.
        if attribute == "files_to_run" {
            let exe = self
                .fields
                .iter()
                .find(|(k, _)| k == "files")
                .and_then(|(_, v)| {
                    starlark::values::list::ListRef::from_value(v.to_value())
                        .and_then(|l| l.iter().next())
                })
                .unwrap_or_else(Value::new_none);
            return Some(_heap.alloc(crate::engine::AbsorbWith {
                overrides: vec![("executable".to_string(), exe)],
            }));
        }
        self.fields.iter().find(|(k, _)| k == attribute).map(|(_, v)| v.to_value())
    }
    /// `dep[MyInfo]` — the instance this dep's rule returned for that provider.
    /// `Provider in dep` — true for registered providers (and the implicit DefaultInfo).
    fn is_in(&self, other: Value<'v>) -> starlark::Result<bool> {
        Ok(self.providers.iter().any(|(c, _)| c.to_value().ptr_eq(other))
            || other.to_string() == "DefaultInfo")
    }
    fn at(&self, index: Value<'v>, _heap: Heap<'v>) -> starlark::Result<Value<'v>> {
        if let Some(v) = self
            .providers
            .iter()
            .find(|(c, _)| c.to_value().ptr_eq(index))
            .map(|(_, inst)| inst.to_value())
        {
            return Ok(v);
        }
        // Every target implicitly provides DefaultInfo (the builtin is a native fn — match by
        // name): files = the dep's files; runfiles/files_to_run absorb until run-goldens.
        if index.to_string() == "DefaultInfo" {
            // DefaultInfo.files is a DEPSET (real impls call .to_list()).
            let files = self
                .fields
                .iter()
                .find(|(k, _)| k == "files")
                .map(|(_, v)| v.to_value())
                .map(|v| match starlark::values::list::ListRef::from_value(v) {
                    Some(l) => _heap.alloc(crate::values::Depset {
                        items: l
                            .iter()
                            .map(|it| match it.unpack_str() {
                                // Stray path STRINGS coerce to File (impls read .extension).
                                Some(p) => {
                                    _heap.alloc(crate::values::File { path: p.to_string() })
                                }
                                None => it,
                            })
                            .collect(),
                    }),
                    None => v,
                })
                .unwrap_or_else(|| _heap.alloc(crate::values::Depset { items: Vec::new() }));
            use starlark::values::structs::AllocStruct;
            return Ok(_heap.alloc(AllocStruct([
                ("files".to_string(), files),
                ("default_runfiles".to_string(), _heap.alloc(crate::engine::Absorb)),
                ("data_runfiles".to_string(), _heap.alloc(crate::engine::Absorb)),
                ("files_to_run".to_string(), _heap.alloc(crate::engine::Absorb)),
            ])));
        }
        // OutputGroupInfo: the builtin stub returns a plain struct (not capturable, no stable
        // ctor identity across modules) — synthesize an absorbing instance. Registered debt:
        // output groups don't flow.
        if index.to_string() == "OutputGroupInfo" {
            return Ok(_heap.alloc(crate::engine::AbsorbWith { overrides: Vec::new() }));
        }
        Err(starlark::Error::new_other(anyhow::anyhow!(
            "target {} does not provide the requested provider {} (have {} pairs: {})",
            self.label,
            index.to_string(),
            self.providers.len(),
            self.providers
                .iter()
                .map(|(c, _)| c.to_value().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )))
    }
}
