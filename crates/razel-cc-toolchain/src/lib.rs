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
    ActionConfig, Feature, FeatureConfig, FlagGroup, FlagSet, Tool, VarValue, Vars, WithFeatures,
};
use starlark::environment::{GlobalsBuilder, LibraryExtension, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};
use starlark::values::list::ListRef;
use starlark::values::{Heap, Value};

pub mod derive;
pub mod rust;
pub use derive::{DeclaredAction, derive_cc_library_actions};
pub use rust::derive_rust_library_action;

/// Evaluate a Starlark cc toolchain config into a [`FeatureConfig`], in the REAL Bazel cc-config API
/// (A5a): razel's `cc_toolchain_config_lib` (feature/flag_set/flag_group/action_config/tool +
/// `cc_common.create_cc_toolchain_config_info`) is prepended, and the source must bind
/// `CONFIG = cc_common.create_cc_toolchain_config_info(features = [...], action_configs = [...])`.
/// (A5b → Phase D evaluates the ACTUAL @rules_cc lib over the host-generated config instead.)
pub fn parse_feature_config(src: &str) -> Result<FeatureConfig, String> {
    // Prepend razel's cc_toolchain_config_lib so the config is written in the REAL Bazel cc-config
    // API (feature/flag_set/… + cc_common.create_cc_toolchain_config_info) — A5a. The config must NOT
    // re-define those names (Starlark forbids reassigning a global).
    // F35: prepending shifts the config's error line numbers by the lib length (and the parse module
    // is named `cc_config.bzl`, not the source). Tolerable for embedded fixtures; for A5b/Phase D
    // (real ~2000-line generated configs) load the lib as a separate FileLoader module so the config
    // keeps its own filename/line numbers. (Tracked in RazelGaps.)
    let lib = include_str!("../fixtures/cc_toolchain_config_lib.bzl");
    let full = format!("{lib}\n{src}");
    let ast = AstModule::parse("cc_config.bzl", full, &Dialect::Extended)
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

/// The variables a cc **compile** action provides — fed by the §8b propagation queries (include
/// paths) + the target's attrs + host params (the SDK).
#[derive(Debug, Clone, Default)]
pub struct CompileInputs {
    pub source_file: String,
    pub output_file: String,
    pub dependency_file: String,
    pub quote_include_paths: Vec<String>,
    pub minimum_os_version: String,
}

/// Build the `CppCompile` argv via [`Constrain`](razel_rulepack::constrain) over the toolchain
/// `config` + the action's inputs — the §8c **actions UDF** for cc. The loader calls this with
/// an analyzed target's data; the result is the action's command line.
pub fn cc_compile_argv(config: &FeatureConfig, inputs: &CompileInputs) -> Vec<String> {
    let vars = Vars::from([
        ("source_file".to_string(), VarValue::Scalar(inputs.source_file.clone())),
        ("output_file".to_string(), VarValue::Scalar(inputs.output_file.clone())),
        ("dependency_file".to_string(), VarValue::Scalar(inputs.dependency_file.clone())),
        ("minimum_os_version".to_string(), VarValue::Scalar(inputs.minimum_os_version.clone())),
        (
            "quote_include_paths".to_string(),
            VarValue::Sequence(inputs.quote_include_paths.clone()),
        ),
    ]);
    config.full_command_line(&config.select(&[]), "c++-compile", &vars)
}

/// The variables a cc **static-archive** action provides.
#[derive(Debug, Clone, Default)]
pub struct ArchiveInputs {
    pub output_execpath: String,
    pub libraries_to_link: Vec<String>,
}

/// Build the `CppArchive` argv via `Constrain` — the §8c actions UDF for the static archive.
pub fn cc_archive_argv(config: &FeatureConfig, inputs: &ArchiveInputs) -> Vec<String> {
    let vars = Vars::from([
        ("output_execpath".to_string(), VarValue::Scalar(inputs.output_execpath.clone())),
        ("libraries_to_link".to_string(), VarValue::Sequence(inputs.libraries_to_link.clone())),
    ]);
    config.full_command_line(&config.select(&[]), "c++-link-static-library", &vars)
}

/// Strip a cc source extension to get the object stem (`util.cc` → `util`).
fn obj_stem(src: &str) -> &str {
    [".cc", ".cpp", ".cxx", ".c", ".C"]
        .iter()
        .find_map(|e| src.strip_suffix(e))
        .unwrap_or(src)
}

/// Bazel's compile-action variables for a cc source — the `bazel-out/<cfg>/bin/<pkg>/_objs/
/// <target>/<stem>.{o,d}` output layout + `-iquote .`/`-iquote bazel-out/<cfg>/bin`. This is the
/// path model razel must adopt to byte-match Bazel's argv *independently* (vs the runner feeding
/// the golden's paths). `cfg`/`sdk` are the (normalized) config segment + SDK host params.
pub fn bazel_compile_inputs(cfg: &str, pkg: &str, target: &str, src: &str, sdk: &str) -> CompileInputs {
    let base = format!("bazel-out/{cfg}/bin/{pkg}/_objs/{target}/{}", obj_stem(src));
    CompileInputs {
        source_file: format!("{pkg}/{src}"),
        output_file: format!("{base}.o"),
        dependency_file: format!("{base}.d"),
        minimum_os_version: sdk.to_string(),
        quote_include_paths: vec![".".to_string(), format!("bazel-out/{cfg}/bin")],
    }
}

/// Bazel's static-archive variables: `bazel-out/<cfg>/bin/<pkg>/lib<target>.a` + the per-source
/// object outputs (matching [`bazel_compile_inputs`]).
pub fn bazel_archive_inputs(cfg: &str, pkg: &str, target: &str, srcs: &[&str]) -> ArchiveInputs {
    ArchiveInputs {
        output_execpath: format!("bazel-out/{cfg}/bin/{pkg}/lib{target}.a"),
        libraries_to_link: srcs
            .iter()
            .map(|s| format!("bazel-out/{cfg}/bin/{pkg}/_objs/{target}/{}.o", obj_stem(s)))
            .collect(),
    }
}

/// The ported macOS cc toolchain config (core compile + archive features), parsed from the
/// embedded fixture. The accessor the loader + parity runner use to drive `Constrain`. (When
/// host-value ingestion lands, this takes the detected flag lists + SDK instead of the fixture's.)
pub fn macos_core_config() -> Result<FeatureConfig, String> {
    parse_feature_config(include_str!("../fixtures/cc_macos_core.bzl"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shim_round_trips_the_richer_cc_config_constructs() {
        // F34: the shim now emits the FULL surface its extractor reads — with_feature_set,
        // feature_set, variable_with_value, expand_if_*, nested flag_groups, env_set/env_entry. These
        // reads were dead before (the shim couldn't produce them); this proves shim ⟷ extractor agree.
        let cfg = parse_feature_config(
            r#"
CONFIG = cc_common.create_cc_toolchain_config_info(
    features = [
        feature(
            name = "opt",
            enabled = True,
            requires = [feature_set(features = ["a", "b"])],
            flag_sets = [
                flag_set(
                    actions = ["c++-compile"],
                    with_features = [with_feature_set(features = ["dbg"], not_features = ["fast"])],
                    flag_groups = [
                        flag_group(
                            flags = ["-O2"],
                            expand_if_not_available = "x",
                            expand_if_equal = variable_with_value(name = "mode", value = "opt"),
                            flag_groups = [flag_group(flags = ["-g"])],
                        ),
                    ],
                ),
            ],
        ),
    ],
    action_configs = [
        action_config(
            action_name = "c++-compile",
            tools = [tool(path = "cc")],
            env_sets = [env_set(actions = ["c++-compile"], env_entries = [env_entry(key = "K", value = "V")])],
        ),
    ],
)
"#,
        )
        .unwrap();

        let f = &cfg.features[0];
        assert_eq!(f.name, "opt");
        assert_eq!(f.requires, vec![vec!["a".to_string(), "b".to_string()]]); // feature_set
        let fs = &f.flag_sets[0];
        assert_eq!(fs.with_features[0].features, ["dbg"]); // with_feature_set
        assert_eq!(fs.with_features[0].not_features, ["fast"]);
        let fg = &fs.flag_groups[0];
        assert_eq!(fg.expand_if_not_available, ["x"]);
        assert_eq!(fg.expand_if_equal, Some(("mode".to_string(), "opt".to_string()))); // variable_with_value
        assert_eq!(fg.flag_groups[0].flags, ["-g"]); // nested flag_groups
        // env_set/env_entry/action_config-kwargs evaluate without error (accepted, absorbed by create).
        assert_eq!(cfg.action_configs[0].action_name, "c++-compile");
    }

    // Constructors + cc_common come from razel's prepended cc_toolchain_config_lib (A5a — the real
    // cc-config API); this fixture is just content written against it.
    const CFG: &str = r#"
CONFIG = cc_common.create_cc_toolchain_config_info(
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
        let cfg = macos_core_config().unwrap();
        let argv = cc_compile_argv(&cfg, &CompileInputs {
            source_file: "util.cc".into(),
            output_file: "util.o".into(),
            dependency_file: "util.d".into(),
            minimum_os_version: "26.4".into(),
            quote_include_paths: vec![".".into(), "bazel-out/<cfg>/bin".into()],
        });
        assert_eq!(
            argv,
            vec![
                "external/<repo>/cc_wrapper.sh",
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

    #[test]
    fn ported_macos_config_reproduces_the_archive_argv() {
        // The static-archive action: only `archiver_flags` fires (action filter), tool = libtool.
        let cfg = macos_core_config().unwrap();
        let argv = cc_archive_argv(&cfg, &ArchiveInputs {
            output_execpath: "libutil.a".into(),
            libraries_to_link: vec!["util.o".into()],
        });
        assert_eq!(
            argv,
            ["/usr/bin/libtool", "-D", "-no_warning_for_no_symbols", "-static", "-o", "libutil.a", "util.o"]
        );
    }

    /// Parse the `Command Line: (exec … )` of the golden action whose header contains `header`
    /// into argv tokens (drop `\` continuations; strip the single-quotes Bazel adds).
    fn golden_argv(golden: &str, header: &str) -> Vec<String> {
        let block = &golden[golden.find(&format!("action '{header}")).expect("action")..];
        let after = &block[block.find("Command Line: (exec ").expect("cmdline")
            + "Command Line: (exec ".len()..];
        let end = after.find(')').unwrap_or(after.len());
        after[..end]
            .split_whitespace()
            .filter(|t| *t != "\\")
            .map(|t| t.trim_matches('\'').to_string())
            .collect()
    }

    /// The `Outputs: [...]` of the golden action whose header contains `header`, sorted.
    fn golden_outputs(golden: &str, header: &str) -> Vec<String> {
        let block = &golden[golden.find(&format!("action '{header}")).expect("action")..];
        let start = block.find("Outputs: [").expect("outputs") + "Outputs: [".len();
        let end = block[start..].find(']').unwrap() + start;
        let mut v: Vec<String> = block[start..end].split(", ").map(String::from).collect();
        v.sort();
        v
    }

    #[test]
    fn parity_constrain_reproduces_the_real_golden_command_lines() {
        // THE parity check: §8c, fed the golden's variables, reproduces Bazel's ACTUAL captured
        // command lines (parsed from golden.txt) — both compile and archive, byte-for-byte.
        let golden = include_str!("../../../parity/corpus/cc/transitive/golden.txt");
        let cfg = macos_core_config().unwrap();

        // razel COMPUTES the paths (the path-model formula) — independent of the golden:
        let ci = bazel_compile_inputs("<cfg>", "corpus/cc/transitive", "util", "util.cc", "<sdk>");
        assert_eq!(
            cc_compile_argv(&cfg, &ci),
            golden_argv(golden, "Compiling corpus/cc/transitive/util.cc")
        );
        let mut compile_out = vec![ci.output_file.clone(), ci.dependency_file.clone()];
        compile_out.sort();
        assert_eq!(compile_out, golden_outputs(golden, "Compiling corpus/cc/transitive/util.cc"));

        let ai = bazel_archive_inputs("<cfg>", "corpus/cc/transitive", "util", &["util.cc"]);
        assert_eq!(
            cc_archive_argv(&cfg, &ai),
            golden_argv(golden, "Linking corpus/cc/transitive/libutil.a")
        );
        assert_eq!(
            vec![ai.output_execpath.clone()],
            golden_outputs(golden, "Linking corpus/cc/transitive/libutil.a")
        );
    }
}
