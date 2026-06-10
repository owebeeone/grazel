//! The toolchain resolver (Phase C3b) — maps a toolchain name to its config producer, so the generic
//! engine (`razel_build.command_line`) doesn't name a language. Adding a command-line-shaped toolchain
//! is a row here, not an edit to the engine — the same "registration, not a core edit" seam as the
//! provider registry.
//!
//! Interim: returns `FeatureConfig` (cc's Constrain model), so it's cc-shaped today. The generic
//! `Toolchain` type (cc `FeatureConfig` vs java's Starlark template) needs a second, command-line-
//! shaped toolchain to be honest — that arrives with real upstream toolchains in Phase D
//! (RazelHookSeam.md §4). AD2: a compile-time-const resolver, not a runtime-derived global.

use razel_rulepack::constrain::FeatureConfig;

/// Resolve a toolchain name to its feature config. This is a **registration site** — naming a
/// toolchain here is the point (allowlisted by the no-language-in-core gate, like the provider
/// registry); the generic engine reads it.
pub(crate) fn resolve_toolchain(name: &str) -> Result<FeatureConfig, String> {
    match name {
        "cc" => razel_cc_toolchain::macos_core_config(),
        other => Err(format!(
            "unknown toolchain {other:?} — only \"cc\" resolves today (java is template-shaped and \
             uses ctx.actions.run; more toolchains register here, the generic Toolchain type is Phase D)"
        )),
    }
}

/// The `ctx.toolchains` host rows (Layer 1): type label → a stand-in struct whose fields grow
/// probe-step by probe-step as real impls touch them. Registration site — naming toolchain types
/// here is the point (allowlisted). Real `rule(toolchains=)` resolution is L3.
pub(crate) fn toolchain_rows<'v>(
    heap: starlark::values::Heap<'v>,
) -> Vec<(String, starlark::values::Value<'v>)> {
    // Absorbing stand-ins: any field/method resolves (loading/analysis-shape); REAL fields
    // (rustc path, cc tool paths) land with the Layer-3 action goldens. Registered debt.
    // The rust row carries REAL scalar fields (typed builtins reject absorbed values) — grown
    // probe-step by probe-step; callables stay absorbed.
    let empty = |heap: starlark::values::Heap<'v>| heap.alloc(crate::engine::Absorb);
    use starlark::values::structs::AllocStruct;
    let (triple, tos, dylib) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => ("aarch64-apple-darwin", "darwin", ".dylib"),
        ("macos", _) => ("x86_64-apple-darwin", "darwin", ".dylib"),
        ("linux", "aarch64") => ("aarch64-unknown-linux-gnu", "linux", ".so"),
        _ => ("x86_64-unknown-linux-gnu", "linux", ".so"),
    };
    let rust = heap.alloc(AllocStruct([
        ("target_triple".to_string(), heap.alloc(triple)),
        ("target_os".to_string(), heap.alloc(tos)),
        ("target_arch".to_string(), heap.alloc(std::env::consts::ARCH)),
        ("binary_ext".to_string(), heap.alloc("")),
        ("staticlib_ext".to_string(), heap.alloc(".a")),
        ("dylib_ext".to_string(), heap.alloc(dylib)),
        ("default_edition".to_string(), heap.alloc("2021")),
        ("_rename_first_party_crates".to_string(), starlark::values::Value::new_bool(false)),
        ("_third_party_dir".to_string(), heap.alloc("third_party/rust")),
        ("_incompatible_change_rust_test_compilation_output_directory".to_string(),
            starlark::values::Value::new_bool(false)),
        ("make_libstd_and_allocator_ccinfo".to_string(), heap.alloc(crate::engine::Absorb)),
    ]));
    vec![
        ("@rules_rust//rust:toolchain_type".to_string(), rust),
        ("@bazel_tools//tools/cpp:toolchain_type".to_string(), empty(heap)),
        ("@com_google_protobuf//bazel/private:proto_toolchain_type".to_string(), empty(heap)),
        ("@bazel_tools//tools/sh:toolchain_type".to_string(), empty(heap)),
    ]
}
