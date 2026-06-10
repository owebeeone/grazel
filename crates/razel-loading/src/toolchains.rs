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
    // target_triple is rules_rust's triple STRUCT (.arch/.vendor/.system/.abi/.str);
    // version_semver is a semver struct (host rustc).
    let mut tp = triple.splitn(4, '-');
    let (arch, vendor, system) = (
        tp.next().unwrap_or(""),
        tp.next().unwrap_or(""),
        tp.next().unwrap_or(""),
    );
    let abi = tp.next();
    let triple_v = heap.alloc(AllocStruct([
        ("arch".to_string(), heap.alloc(arch)),
        ("vendor".to_string(), heap.alloc(vendor)),
        ("system".to_string(), heap.alloc(system)),
        (
            "abi".to_string(),
            abi.map(|a| heap.alloc(a)).unwrap_or_else(starlark::values::Value::new_none),
        ),
        ("str".to_string(), heap.alloc(triple)),
    ]));
    let semver_v = heap.alloc(AllocStruct([
        ("major".to_string(), heap.alloc(1)),
        ("minor".to_string(), heap.alloc(95)),
        ("patch".to_string(), heap.alloc(0)),
    ]));
    let rust = heap.alloc(AllocStruct([
        ("target_triple".to_string(), triple_v),
        ("version_semver".to_string(), semver_v),
        ("version".to_string(), heap.alloc("1.95.0")),
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
        // The full field surface rustc.bzl touches (host-true values; preempted in one pass).
        ("lto".to_string(), heap.alloc(AllocStruct([
            ("mode".to_string(), heap.alloc("off")),
        ]))),
        ("rustc".to_string(), {
            let path = std::env::var("PATH").unwrap_or_default();
            let found = path
                .split(':')
                .map(|d| std::path::Path::new(d).join("rustc"))
                .find(|p| p.is_file())
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "rustc".to_string());
            heap.alloc(crate::values::File { path: found })
        }),
        ("rust_std".to_string(), heap.alloc(Vec::<starlark::values::Value<'v>>::new())),
        ("all_files".to_string(), heap.alloc(Vec::<starlark::values::Value<'v>>::new())),
        ("env".to_string(), heap.alloc(starlark::values::dict::AllocDict::EMPTY)),
        ("extra_rustc_flags".to_string(), heap.alloc(Vec::<starlark::values::Value<'v>>::new())),
        ("extra_exec_rustc_flags".to_string(), heap.alloc(Vec::<starlark::values::Value<'v>>::new())),
        ("extra_rustc_flags_for_crate_types".to_string(),
            heap.alloc(starlark::values::dict::AllocDict::EMPTY)),
        ("compilation_mode_opts".to_string(), {
            let mode = |o: &str, d: &str| {
                heap.alloc(AllocStruct([
                    ("opt_level".to_string(), heap.alloc(o)),
                    ("debug_info".to_string(), heap.alloc(d)),
                    ("strip".to_string(), heap.alloc("none")),
                    ("strip_level".to_string(), heap.alloc("none")),
                ]))
            };
            heap.alloc(starlark::values::dict::AllocDict([
                (heap.alloc("fastbuild"), mode("0", "0")),
                (heap.alloc("dbg"), mode("0", "2")),
                (heap.alloc("opt"), mode("3", "0")),
            ]))
        }),
        ("libstd_and_allocator_ccinfo".to_string(), starlark::values::Value::new_none()),
        ("libstd_and_global_allocator_ccinfo".to_string(), starlark::values::Value::new_none()),
        ("nostd_and_global_allocator_ccinfo".to_string(), starlark::values::Value::new_none()),
        // A real host linker File (cc): cc_common.get_tool_for_action absorbs (falsy), so
        // the rust-linker fallback path reads this.
        ("linker".to_string(), {
            let path = std::env::var("PATH").unwrap_or_default();
            let found = path
                .split(':')
                .map(|d| std::path::Path::new(d).join("cc"))
                .find(|p| p.is_file())
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "cc".to_string());
            heap.alloc(crate::values::File { path: found })
        }),
        ("linker_type".to_string(), heap.alloc("default")),
        ("linker_preference".to_string(), heap.alloc("default")),
        ("llvm_cov".to_string(), starlark::values::Value::new_none()),
        ("llvm_profdata".to_string(), starlark::values::Value::new_none()),
        ("llvm_lib".to_string(), starlark::values::Value::new_none()),
        ("sysroot_anchor".to_string(), starlark::values::Value::new_none()),
        ("stdlib_linkflags".to_string(), heap.alloc(Vec::<starlark::values::Value<'v>>::new())),
        ("target_flag_value".to_string(), heap.alloc(triple)),
        ("target_json".to_string(), heap.alloc("")),
        ("target_abi".to_string(), heap.alloc("")),
        ("coverage_supported".to_string(), starlark::values::Value::new_bool(false)),
        ("require_explicit_unstable_features".to_string(), starlark::values::Value::new_bool(false)),
        ("_codegen_units".to_string(), heap.alloc(-1)),
        ("_experimental_link_std_dylib".to_string(), starlark::values::Value::new_bool(false)),
        ("_experimental_use_allocator_libraries_with_mangled_symbols".to_string(), heap.alloc(0)),
        ("_experimental_use_allocator_libraries_with_mangled_symbols_setting".to_string(), heap.alloc(0)),
        ("_experimental_use_cc_common_link".to_string(), heap.alloc(0)),
        ("_experimental_use_coverage_metadata_files".to_string(), starlark::values::Value::new_bool(false)),
        ("_experimental_use_global_allocator".to_string(), starlark::values::Value::new_bool(false)),
        ("_incompatible_do_not_include_data_in_compile_data".to_string(), starlark::values::Value::new_bool(true)),
        ("_incompatible_do_not_include_transitive_data_in_compile_inputs".to_string(), starlark::values::Value::new_bool(true)),
        ("_linker_files".to_string(), heap.alloc(Vec::<starlark::values::Value<'v>>::new())),
        ("_no_std".to_string(), heap.alloc("off")),
        ("_pipelined_compilation".to_string(), starlark::values::Value::new_bool(false)),
        ("_toolchain_generated_sysroot".to_string(), starlark::values::Value::new_bool(false)),
    ]));
    vec![
        ("@rules_rust//rust:toolchain_type".to_string(), rust),
        ("@bazel_tools//tools/cpp:toolchain_type".to_string(), empty(heap)),
        ("@com_google_protobuf//bazel/private:proto_toolchain_type".to_string(), empty(heap)),
        ("@bazel_tools//tools/sh:toolchain_type".to_string(), empty(heap)),
    ]
}
