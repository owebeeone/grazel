//! A2a (RazelStarlarkBoundaryPlan §10): the provider-capture eval change. A `rule()` impl returns
//! `[CcInfo(headers=…), DefaultInfo(…)]`; razel captures the OWN headers (`hdrs`) and exposes each
//! dep's **transitive** exported headers as `dep.headers` (the `provides` fold). Proven across two
//! levels (util → base → core) so util sees `core.h` *through* base.

use razel_loading::analyze_starlark;

const SRC: &str = r#"
def _lib(ctx):
    flags = []
    for d in ctx.attr.deps:
        flags = flags + d.headers
    out = ctx.actions.declare_file(ctx.attr.name + ".o")
    ctx.actions.run(executable = "cc", outputs = [out], inputs = [], arguments = flags)
    return [CcInfo(headers = [ctx.attr.name + ".h"]), DefaultInfo(files = [out])]

lib = rule(implementation = _lib, attrs = {})
lib(name = "core", deps = [])  # explicit [] — razel has no attr-schema defaults yet (A2/D)
lib(name = "base", deps = [":core"])
lib(name = "util", deps = [":base"])
"#;

#[test]
fn cc_info_captures_own_headers_and_folds_transitive_to_dependents() {
    let targets = analyze_starlark("BUILD", SRC).unwrap();
    let find = |n: &str| targets.iter().find(|t| t.name.ends_with(n)).unwrap();

    // CcInfo captured each target's OWN exported headers.
    assert_eq!(find("core").hdrs, ["core.h"]);
    assert_eq!(find("base").hdrs, ["base.h"]);
    assert_eq!(find("util").hdrs, ["util.h"]);

    // dep.headers is the TRANSITIVE fold: util's dep (base) exposes {base.h, core.h} — util sees
    // core.h through base. Folded into the action arguments (sorted, deduped).
    let util_action = &find("util").actions[0];
    assert_eq!(util_action.argv, ["cc", "base.h", "core.h"]);
    // base's dep (core) exposes {core.h}.
    let base_action = &find("base").actions[0];
    assert_eq!(base_action.argv, ["cc", "core.h"]);
}
