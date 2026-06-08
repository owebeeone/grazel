//! `razel-cc-toolchain` — ingest a cc toolchain config (its Starlark `feature`/`flag_set`/…
//! definitions) into the `Constrain` feature-config types (`razel_rulepack::constrain`) by
//! **evaluating** it via Starlark. This is the faithful path (BazelCcCommandLine.md): the config
//! is *run*, not hand-transcribed, so razel's argv tracks Bazel's exactly.
//!
//! First slice: a config expressed with pure-Starlark constructor functions returning structs
//! (`feature()`/`flag_set()`/`flag_group()`/`with_feature_set()`/`action_config()`/`tool()` — the
//! shape `cc_toolchain_config_lib.bzl` defines) + a top-level `CONFIG`. Loading the *real*
//! `unix_cc_toolchain_config.bzl` rule impl (which needs a `cc_common` shim + running the rule)
//! is the next slice — the extraction below is the same.

use razel_rulepack::constrain::{
    ActionConfig, Feature, FeatureConfig, FlagGroup, FlagSet, Tool, WithFeatures,
};
use starlark::environment::{GlobalsBuilder, LibraryExtension, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};
use starlark::values::list::ListRef;
use starlark::values::{Heap, Value};

/// Evaluate a Starlark cc toolchain config into a [`FeatureConfig`]. The source must define a
/// top-level `CONFIG = struct(features = [...], action_configs = [...])`, with each feature /
/// flag_set / flag_group / with_feature_set / action_config / tool a `struct(...)` carrying the
/// fields below.
pub fn parse_feature_config(src: &str) -> Result<FeatureConfig, String> {
    let ast = AstModule::parse("cc_config.bzl", src.to_owned(), &Dialect::Extended)
        .map_err(|e| format!("{e}"))?;
    let globals = GlobalsBuilder::extended_by(&[LibraryExtension::StructType]).build();
    Module::with_temp_heap(|module| {
        let mut eval = Evaluator::new(&module);
        eval.eval_module(ast, &globals).map_err(|e| format!("{e}"))?;
        let heap = eval.heap();
        let config = module
            .get("CONFIG")
            .ok_or("cc toolchain config must define a top-level `CONFIG`")?;
        Ok(FeatureConfig {
            features: list(config, "features", heap).iter().map(|f| feature(*f, heap)).collect(),
            action_configs: list(config, "action_configs", heap)
                .iter()
                .map(|a| action_config(*a, heap))
                .collect(),
        })
    })
}

fn attr<'v>(v: Value<'v>, name: &str, heap: Heap<'v>) -> Option<Value<'v>> {
    v.get_attr(name, heap).ok().flatten()
}
fn str_field<'v>(v: Value<'v>, name: &str, heap: Heap<'v>) -> String {
    attr(v, name, heap).and_then(|x| x.unpack_str().map(String::from)).unwrap_or_default()
}
fn bool_field<'v>(v: Value<'v>, name: &str, heap: Heap<'v>) -> bool {
    attr(v, name, heap).and_then(|x| x.unpack_bool()).unwrap_or(false)
}
fn opt_str<'v>(v: Value<'v>, name: &str, heap: Heap<'v>) -> Option<String> {
    attr(v, name, heap).and_then(|x| x.unpack_str().map(String::from))
}
fn list<'v>(v: Value<'v>, name: &str, heap: Heap<'v>) -> Vec<Value<'v>> {
    match attr(v, name, heap).and_then(ListRef::from_value) {
        Some(l) => l.iter().collect(),
        None => Vec::new(),
    }
}
fn str_list<'v>(v: Value<'v>, name: &str, heap: Heap<'v>) -> Vec<String> {
    list(v, name, heap).iter().filter_map(|x| x.unpack_str().map(String::from)).collect()
}
fn with_features<'v>(w: Value<'v>, heap: Heap<'v>) -> WithFeatures {
    WithFeatures {
        features: str_list(w, "features", heap),
        not_features: str_list(w, "not_features", heap),
    }
}

fn feature<'v>(v: Value<'v>, heap: Heap<'v>) -> Feature {
    Feature {
        name: str_field(v, "name", heap),
        enabled: bool_field(v, "enabled", heap),
        flag_sets: list(v, "flag_sets", heap).iter().map(|fs| flag_set(*fs, heap)).collect(),
        implies: str_list(v, "implies", heap),
        // `requires` is a list of `feature_set(features = [...])`.
        requires: list(v, "requires", heap).iter().map(|r| str_list(*r, "features", heap)).collect(),
        provides: str_list(v, "provides", heap),
    }
}
fn flag_set<'v>(v: Value<'v>, heap: Heap<'v>) -> FlagSet {
    FlagSet {
        actions: str_list(v, "actions", heap),
        with_features: list(v, "with_features", heap).iter().map(|w| with_features(*w, heap)).collect(),
        flag_groups: list(v, "flag_groups", heap).iter().map(|fg| flag_group(*fg, heap)).collect(),
    }
}
fn flag_group<'v>(v: Value<'v>, heap: Heap<'v>) -> FlagGroup {
    FlagGroup {
        flags: str_list(v, "flags", heap),
        flag_groups: list(v, "flag_groups", heap).iter().map(|g| flag_group(*g, heap)).collect(),
        iterate_over: opt_str(v, "iterate_over", heap),
        expand_if_available: opt_str(v, "expand_if_available", heap).into_iter().collect(),
        expand_if_not_available: opt_str(v, "expand_if_not_available", heap).into_iter().collect(),
        expand_if_true: opt_str(v, "expand_if_true", heap),
        expand_if_false: opt_str(v, "expand_if_false", heap),
        expand_if_equal: opt_equal(v, heap),
    }
}

/// `expand_if_equal = variable_with_value(variable = …, value = …)` → `(variable, value)`.
fn opt_equal<'v>(v: Value<'v>, heap: Heap<'v>) -> Option<(String, String)> {
    let e = attr(v, "expand_if_equal", heap)?;
    if e.is_none() {
        return None;
    }
    Some((str_field(e, "variable", heap), str_field(e, "value", heap)))
}
fn action_config<'v>(v: Value<'v>, heap: Heap<'v>) -> ActionConfig {
    ActionConfig {
        action_name: str_field(v, "action_name", heap),
        tools: list(v, "tools", heap)
            .iter()
            .map(|t| Tool {
                path: str_field(*t, "path", heap),
                with_features: list(*t, "with_features", heap)
                    .iter()
                    .map(|w| with_features(*w, heap))
                    .collect(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use razel_rulepack::constrain::{VarValue, Vars};

    const CFG: &str = r#"
def flag_group(flags = [], iterate_over = None, expand_if_available = None):
    return struct(flags = flags, iterate_over = iterate_over, expand_if_available = expand_if_available)
def flag_set(actions = [], with_features = [], flag_groups = []):
    return struct(actions = actions, with_features = with_features, flag_groups = flag_groups)
def feature(name, enabled = False, flag_sets = [], implies = [], requires = [], provides = []):
    return struct(name = name, enabled = enabled, flag_sets = flag_sets, implies = implies, requires = requires, provides = provides)
def action_config(action_name, tools = []):
    return struct(action_name = action_name, tools = tools)
def tool(path, with_features = []):
    return struct(path = path, with_features = with_features)

CONFIG = struct(
    features = [
        feature(name = "default_compile_flags", enabled = True, flag_sets = [
            flag_set(actions = ["c++-compile"], flag_groups = [flag_group(flags = ["-U_FORTIFY_SOURCE", "-Wall"])]),
        ]),
        feature(name = "dependency_file", enabled = True, flag_sets = [
            flag_set(actions = ["c++-compile"], flag_groups = [
                flag_group(flags = ["-MD", "-MF", "%{dependency_file}"], expand_if_available = "dependency_file"),
            ]),
        ]),
        feature(name = "opt", flag_sets = [flag_set(actions = ["c++-compile"], flag_groups = [flag_group(flags = ["-O2"])])]),
    ],
    action_configs = [
        action_config(action_name = "c++-compile", tools = [tool(path = "cc_wrapper.sh")]),
    ],
)
"#;

    #[test]
    fn evals_starlark_config_into_constrain_and_runs() {
        let cfg = parse_feature_config(CFG).unwrap();
        assert_eq!(cfg.features.len(), 3);
        assert_eq!(cfg.action_configs.len(), 1);

        let vars = Vars::from([("dependency_file".into(), VarValue::Scalar("util.d".into()))]);
        // Defaults: default_compile_flags + dependency_file enabled; opt is not.
        let argv = cfg.full_command_line(&cfg.select(&[]), "c++-compile", &vars);
        assert_eq!(argv, ["cc_wrapper.sh", "-U_FORTIFY_SOURCE", "-Wall", "-MD", "-MF", "util.d"]);
        // The evaluated `opt` feature fires only when requested.
        let with_opt = cfg.full_command_line(&cfg.select(&["opt".into()]), "c++-compile", &vars);
        assert!(with_opt.contains(&"-O2".to_string()));
    }

    #[test]
    fn expand_if_available_gate_survives_the_round_trip() {
        let cfg = parse_feature_config(CFG).unwrap();
        // No dependency_file variable → the -MD/-MF group is gated out (the gate was extracted).
        let argv = cfg.full_command_line(&cfg.select(&[]), "c++-compile", &Vars::new());
        assert!(!argv.contains(&"-MD".to_string()));
    }

    #[test]
    fn ported_macos_config_reproduces_the_full_golden_compile_argv() {
        // The ported real-structure config + real cc_configure flags, evaluated + run through
        // Constrain, reproduces the ENTIRE captured CppCompile argv — including the host-specific
        // -frandom-seed / -mmacosx-version-min (here fed by the output_file / minimum_os_version
        // variables). (-mmacosx-version-min=26.4 normalizes to <sdk> at parity-diff time.)
        let cfg = parse_feature_config(include_str!("../fixtures/cc_macos_core.bzl")).unwrap();
        let vars = Vars::from([
            ("source_file".into(), VarValue::Scalar("util.cc".into())),
            ("output_file".into(), VarValue::Scalar("util.o".into())),
            ("dependency_file".into(), VarValue::Scalar("util.d".into())),
            ("minimum_os_version".into(), VarValue::Scalar("26.4".into())),
            (
                "quote_include_paths".into(),
                VarValue::Sequence(vec![".".into(), "bazel-out/<cfg>/bin".into()]),
            ),
        ]);
        let argv = cfg.full_command_line(&cfg.select(&[]), "c++-compile", &vars);
        assert_eq!(
            argv,
            vec![
                "cc_wrapper.sh",
                "-U_FORTIFY_SOURCE",
                "-fstack-protector",
                "-Wall",
                "-Wthread-safety",
                "-Wself-assign",
                "-Wunused-but-set-parameter",
                "-Wno-free-nonheap-object",
                "-fcolor-diagnostics",
                "-fno-omit-frame-pointer",
                "-std=c++17",
                "-frandom-seed=util.o",
                "-mmacosx-version-min=26.4",
                "-MD",
                "-MF",
                "util.d",
                "-iquote",
                ".",
                "-iquote",
                "bazel-out/<cfg>/bin",
                "-c",
                "util.cc",
                "-o",
                "util.o",
                "-no-canonical-prefixes",
                "-Wno-builtin-macro-redefined",
                "-D__DATE__=\"redacted\"",
                "-D__TIMESTAMP__=\"redacted\"",
                "-D__TIME__=\"redacted\"",
            ]
        );
    }
}
