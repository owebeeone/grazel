//! @rules_rust → razel native rules: rust_library / rust_binary (rustc).
//!
//! STUB. Registered in `rules::ruleset_modules` under the `@rules_rust//` prefix; a
//! `load("@rules_rust//...", ...)` resolves here. The native rule logic + tests are
//! implemented in the language fan-out. Shared helpers live in `crate::rules`
//! (`record_target`, `canon_label`, `qualify`, `resolve_dep`, `unpack`,
//! `AnalyzedTarget`/`AnalyzedAction`); model the implementation on `rules::cc_rules`.

use starlark::environment::{FrozenModule, GlobalsBuilder, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};

/// The synthetic `@rules_rust` module re-exporting razel's native rules. Currently
/// empty (no rules bound) — a `load` of a rule name from it will fail until the
/// fan-out implements them.
pub(crate) fn module() -> Result<FrozenModule, String> {
    let globals = GlobalsBuilder::standard().build();
    Module::with_temp_heap(|module| {
        let ast = AstModule::parse("@rules_rust", String::new(), &Dialect::Extended)
            .map_err(|e| format!("{e}"))?;
        {
            let mut eval = Evaluator::new(&module);
            eval.eval_module(ast, &globals)
                .map_err(|e| format!("{e}"))?;
        }
        module.freeze().map_err(|e| format!("{e:?}"))
    })
}
