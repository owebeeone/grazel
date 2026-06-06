//! Starlark-defined rules + analysis (Phase 3): `rule(implementation, attrs)` returns a
//! callable custom value; instantiating it runs the rule **implementation** with a `ctx`
//! (Bazel dialect: `ctx.attr.*`, `ctx.label`, `ctx.actions.declare_file/run/write`) and
//! captures the declared outputs, registered actions, and `DefaultInfo` — i.e. the target
//! **analyzes**.
//!
//! Analysis runs in the **same eval scope** as instantiation (the impl `Value` never
//! escapes the heap) — sidestepping module freezing. This is the Tier-2.5 simplification;
//! a two-phase freeze model comes when caching / cross-target dep-providers demand it.

use allocative::Allocative;
use starlark::any::ProvidesStaticType;
use starlark::collections::SmallMap;
use starlark::environment::{GlobalsBuilder, Methods, MethodsBuilder, Module};
use starlark::eval::{Arguments, Evaluator};
use starlark::syntax::{AstModule, Dialect};
use starlark::values::list::UnpackList;
use starlark::values::none::NoneType;
use starlark::values::structs::AllocStruct;
use starlark::values::{Heap, NoSerialize, StarlarkValue, Trace, Value, starlark_value};
use std::cell::RefCell;
use std::fmt;

/// The captured analysis of one target: a rule impl ran and produced these.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnalyzedTarget {
    pub name: String,
    pub declared_outputs: Vec<String>,
    /// Action mnemonics (the `executable` of each `ctx.actions.run`, or `write`).
    pub actions: Vec<String>,
    /// `DefaultInfo(files=…)`.
    pub default_info: Vec<String>,
}

#[derive(Default)]
struct AnalysisState {
    targets: Vec<AnalyzedTarget>,
    current: Option<AnalyzedTarget>,
}

thread_local! {
    static STATE: RefCell<AnalysisState> = RefCell::new(AnalysisState::default());
}

fn with_current<F: FnOnce(&mut AnalyzedTarget)>(f: F) {
    STATE.with_borrow_mut(|s| {
        if let Some(c) = s.current.as_mut() {
            f(c);
        }
    });
}

// ---- ctx.actions ----------------------------------------------------------------

#[derive(Debug, NoSerialize, ProvidesStaticType, Allocative, Trace)]
struct Actions;

impl fmt::Display for Actions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<ctx.actions>")
    }
}

#[starlark_value(type = "actions")]
impl<'v> StarlarkValue<'v> for Actions {
    fn get_methods() -> Option<&'static Methods> {
        Some(ACTIONS_METHODS.methods())
    }
}

starlark::methods_static!(ACTIONS_METHODS = actions_methods);

#[starlark::starlark_module]
fn actions_methods(b: &mut MethodsBuilder) {
    fn declare_file<'v>(
        #[starlark(this)] _this: Value<'v>,
        filename: String,
    ) -> anyhow::Result<String> {
        with_current(|c| c.declared_outputs.push(filename.clone()));
        Ok(filename)
    }
    fn run<'v>(
        #[starlark(this)] _this: Value<'v>,
        #[starlark(require = named)] executable: Option<String>,
        #[starlark(require = named)] outputs: Option<UnpackList<String>>,
        #[starlark(require = named)] arguments: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        let _ = (outputs, arguments);
        with_current(|c| c.actions.push(executable.unwrap_or_else(|| "run".into())));
        Ok(NoneType)
    }
    fn write<'v>(
        #[starlark(this)] _this: Value<'v>,
        #[starlark(require = named)] output: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        with_current(|c| {
            c.actions.push("write".into());
            c.declared_outputs.push(output);
        });
        Ok(NoneType)
    }
}

// ---- ctx ------------------------------------------------------------------------

/// The analysis `ctx`. All fields are heap `Value`s (struct of attrs, the actions object,
/// the label string) so it traces cleanly and needs no freezing.
#[derive(Debug, NoSerialize, ProvidesStaticType, Allocative, Trace)]
struct Ctx<'v> {
    attr: Value<'v>,
    actions: Value<'v>,
    label: Value<'v>,
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
            _ => None,
        }
    }
}

// ---- rule() + DefaultInfo -------------------------------------------------------

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
    /// `my_rule(name=…, …)` — build a `ctx` and run the impl (same-scope analysis).
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let named = args.names_map()?;
        let mut name = String::new();
        let mut fields: Vec<(String, Value<'v>)> = Vec::new();
        for (k, v) in &named {
            let key = k.as_str().to_string();
            if key == "name" {
                name = v.unpack_str().unwrap_or_default().to_string();
            }
            fields.push((key, *v));
        }

        STATE.with_borrow_mut(|s| {
            s.current = Some(AnalyzedTarget {
                name: name.clone(),
                ..Default::default()
            })
        });

        let heap = eval.heap();
        let ctx = heap.alloc_complex_no_freeze(Ctx {
            attr: heap.alloc(AllocStruct(fields)),
            actions: heap.alloc_complex_no_freeze(Actions),
            label: heap.alloc(format!("//:{name}")),
        });
        eval.eval_function(self.implementation, &[ctx], &[])?;

        STATE.with_borrow_mut(|s| {
            if let Some(c) = s.current.take() {
                s.targets.push(c);
            }
        });
        Ok(Value::new_none())
    }
}

#[allow(non_snake_case)]
#[starlark::starlark_module]
fn rule_globals(b: &mut GlobalsBuilder) {
    /// `rule(implementation, attrs={})` → a callable rule object.
    fn rule<'v>(
        #[starlark(require = named)] implementation: Value<'v>,
        #[starlark(require = named)] attrs: Option<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let _ = attrs; // attr schema unused until typed coercion
        Ok(eval
            .heap()
            .alloc_complex_no_freeze(RuleObj { implementation }))
    }

    /// `DefaultInfo(files=[…])` — the standard output provider (other kwargs absorbed).
    fn DefaultInfo<'v>(
        #[starlark(require = named)] files: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        if let Some(f) = files {
            with_current(|c| c.default_info = f.items);
        }
        Ok(NoneType)
    }
}

/// Evaluate a `BUILD`/`.bzl` that defines and instantiates Starlark rules, running each
/// rule impl (same-scope analysis); returns the analyzed targets.
pub fn analyze_starlark(name: &str, src: &str) -> Result<Vec<AnalyzedTarget>, String> {
    STATE.with_borrow_mut(|s| {
        s.targets.clear();
        s.current = None;
    });
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
    Ok(STATE.with_borrow_mut(|s| std::mem::take(&mut s.targets)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starlark_rule_analyzes_by_running_its_impl() {
        // A Bazel-dialect rule (the form a translated no_prelude rule takes): the impl
        // reads ctx.attr, declares an output, registers an action, returns DefaultInfo.
        let src = r#"
def _impl(ctx):
    out = ctx.actions.declare_file(ctx.attr.name + ".o")
    ctx.actions.run(executable = "cc", outputs = [out], arguments = ["-c", ctx.attr.src])
    return [DefaultInfo(files = [out])]

cc_thing = rule(implementation = _impl, attrs = {"src": 1})
cc_thing(name = "widget", src = "widget.c")
cc_thing(name = "gadget", src = "gadget.c")
"#;
        let targets = analyze_starlark("BUILD", src).unwrap();
        assert_eq!(targets.len(), 2);
        let w = &targets[0];
        assert_eq!(w.name, "widget");
        assert_eq!(w.declared_outputs, vec!["widget.o"]);
        assert_eq!(w.actions, vec!["cc"]);
        assert_eq!(w.default_info, vec!["widget.o"]);
        assert_eq!(targets[1].name, "gadget");
        assert_eq!(targets[1].declared_outputs, vec!["gadget.o"]);
    }
}
