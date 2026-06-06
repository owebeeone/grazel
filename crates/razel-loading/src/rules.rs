//! Starlark-defined rules (Phase 3): `rule(implementation, attrs)` returns a **callable
//! custom value**; instantiating it in a `BUILD`/`.bzl` (`my_rule(name=…)`) records a
//! target. This is the keystone the `no_prelude` gate needs (user rules, not built-ins).
//!
//! The rule object holds the impl `Value` and is allocated with `alloc_complex_no_freeze`
//! (traced, never frozen) — so analysis can invoke the impl in the **same eval scope**,
//! sidestepping module-freezing and cross-phase value escape. (ctx/actions wiring is the
//! next increment; this proves the callable + target collection.)

use allocative::Allocative;
use starlark::any::ProvidesStaticType;
use starlark::environment::GlobalsBuilder;
use starlark::eval::{Arguments, Evaluator};
use starlark::values::{NoSerialize, StarlarkValue, Trace, Value, starlark_value};
use std::cell::RefCell;
use std::fmt;

/// A target instantiated from a Starlark-defined `rule()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StarlarkTargetDecl {
    pub name: String,
    /// The named-attribute keys passed at the call site (e.g. `name`, `srcs`, `deps`).
    pub attrs: Vec<String>,
}

thread_local! {
    static STARLARK_TARGETS: RefCell<Vec<StarlarkTargetDecl>> = const { RefCell::new(Vec::new()) };
}

/// A callable rule object (the result of `rule(...)`). Holds the impl function so analysis
/// can run it in the same eval scope.
#[derive(Debug, Trace, NoSerialize, ProvidesStaticType, Allocative)]
struct RuleObj<'v> {
    implementation: Value<'v>,
}

impl fmt::Display for RuleObj<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<rule>")
    }
}

#[starlark_value(type = "rule")]
impl<'v> StarlarkValue<'v> for RuleObj<'v> {
    /// `my_rule(name=…, …)` — record a target. (Impl invocation / ctx is the next step.)
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        _eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let named = args.names_map()?;
        let mut name = String::new();
        let mut attrs = Vec::new();
        for (k, v) in &named {
            let key = k.as_str().to_string();
            if key == "name" {
                name = v.unpack_str().unwrap_or_default().to_string();
            }
            attrs.push(key);
        }
        STARLARK_TARGETS.with_borrow_mut(|t| t.push(StarlarkTargetDecl { name, attrs }));
        Ok(Value::new_none())
    }
}

#[starlark::starlark_module]
fn rule_globals(b: &mut GlobalsBuilder) {
    /// `rule(implementation, attrs={})` → a callable rule object.
    fn rule<'v>(
        #[starlark(require = named)] implementation: Value<'v>,
        #[starlark(require = named)] attrs: Option<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let _ = attrs; // attr schema unused until ctx wiring
        Ok(eval
            .heap()
            .alloc_complex_no_freeze(RuleObj { implementation }))
    }
}

/// Evaluate a `BUILD`/`.bzl` source that defines and instantiates Starlark rules,
/// returning the recorded targets.
pub fn load_starlark_rules(name: &str, src: &str) -> Result<Vec<StarlarkTargetDecl>, String> {
    use starlark::environment::Module;
    use starlark::syntax::{AstModule, Dialect};

    STARLARK_TARGETS.with_borrow_mut(|t| t.clear());
    let ast =
        AstModule::parse(name, src.to_owned(), &Dialect::Extended).map_err(|e| format!("{e}"))?;
    let globals = GlobalsBuilder::standard().with(rule_globals).build();
    let res: Result<(), String> = Module::with_temp_heap(|module| {
        let mut eval = Evaluator::new(&module);
        eval.eval_module(ast, &globals)
            .map_err(|e| format!("{e}"))?;
        Ok(())
    });
    res?;
    Ok(STARLARK_TARGETS.with_borrow_mut(std::mem::take))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_is_callable_and_records_targets() {
        let src = r#"
def _impl(ctx):
    return []

my_rule = rule(implementation = _impl, attrs = {"srcs": 1, "deps": 2})
my_rule(name = "foo", srcs = ["a.cc"])
my_rule(name = "bar")
"#;
        let targets = load_starlark_rules("BUILD", src).unwrap();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].name, "foo");
        assert!(targets[0].attrs.contains(&"srcs".to_string()));
        assert!(targets[0].attrs.contains(&"name".to_string()));
        assert_eq!(targets[1].name, "bar");
    }
}
