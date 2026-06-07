//! @rules_python → razel native rules: py_library / py_binary / py_test (no compile; launcher + PYTHONPATH).
//!
//! STUB. Registered in `rules::ruleset_modules` under the `@rules_python//` prefix; a
//! `load("@rules_python//...", ...)` resolves here. The native rule logic + tests are
//! implemented in the language fan-out. Shared helpers live in `crate::rules`
//! (`record_target`, `canon_label`, `qualify`, `resolve_dep`, `unpack`,
//! `AnalyzedTarget`/`AnalyzedAction`); model the implementation on `rules::cc_rules`.

use starlark::environment::{FrozenModule, GlobalsBuilder, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};

/// The synthetic `@rules_python` module re-exporting razel's native rules. Currently
/// empty (no rules bound) — a `load` of a rule name from it will fail until the
/// fan-out implements them.
pub(crate) fn module() -> Result<FrozenModule, String> {
    let globals = GlobalsBuilder::standard().build();
    Module::with_temp_heap(|module| {
        let ast = AstModule::parse("@rules_python", String::new(), &Dialect::Extended)
            .map_err(|e| format!("{e}"))?;
        {
            let mut eval = Evaluator::new(&module);
            eval.eval_module(ast, &globals)
                .map_err(|e| format!("{e}"))?;
        }
        module.freeze().map_err(|e| format!("{e:?}"))
    })
}
