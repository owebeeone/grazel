//! razel's HOST repos — the `.bzl` content razel itself provides, compiled into the engine
//! (razelV3). Bazel ships `@bazel_tools` and *generates* dispatch repos like
//! `@cc_compatibility_proxy`; razel, as the host build tool, provides its materializations of
//! them here (the Bazel<9 shape: host-native bindings). Checked BEFORE vendored externals —
//! these names are host-reserved, exactly as `@bazel_tools` is in Bazel.
//!
//! Adding a host file is adding a row + an `include_str!` — registration, not engine-core code.

/// The host `.bzl` for a canonical label (`@repo//pkg:file`), if razel provides one.
pub(crate) fn host_bzl(label: &str) -> Option<&'static str> {
    const HOST: &[(&str, &str)] = &[
        (
            "@bazel_tools//tools/build_defs/cc:action_names.bzl",
            include_str!("../host-repos/bazel_tools/tools/build_defs/cc/action_names.bzl"),
        ),
        (
            "@bazel_tools//tools/cpp:toolchain_utils.bzl",
            include_str!("../host-repos/bazel_tools/tools/cpp/toolchain_utils.bzl"),
        ),
        (
            "@local_config_cuda//cuda:build_defs.bzl",
            include_str!("../host-repos/local_config_cuda/cuda/build_defs.bzl"),
        ),
        (
            "@tf_wheel_version_suffix//:wheel_version_suffix.bzl",
            include_str!("../host-repos/tf_wheel_version_suffix/wheel_version_suffix.bzl"),
        ),
        (
            "@local_config_remote_execution//:remote_execution.bzl",
            include_str!("../host-repos/local_config_remote_execution/remote_execution.bzl"),
        ),
        (
            "@local_config_rocm//rocm:build_defs.bzl",
            include_str!("../host-repos/local_config_rocm/rocm/build_defs.bzl"),
        ),
        (
            "@local_config_sycl//sycl:build_defs.bzl",
            include_str!("../host-repos/local_config_sycl/sycl/build_defs.bzl"),
        ),
        (
            "@local_config_tensorrt//:build_defs.bzl",
            include_str!("../host-repos/local_config_tensorrt/build_defs.bzl"),
        ),
        (
            "@proto_bazel_features//:features.bzl",
            include_str!("../host-repos/proto_bazel_features/features.bzl"),
        ),
        (
            "@bazel_skylib//lib:selects.bzl",
            include_str!("../host-repos/bazel_skylib/lib/selects.bzl"),
        ),
        (
            "@compatibility_proxy//:proxy.bzl",
            include_str!("../host-repos/compatibility_proxy/proxy.bzl"),
        ),
        (
            "@cc_compatibility_proxy//:symbols.bzl",
            include_str!("../host-repos/cc_compatibility_proxy/symbols.bzl"),
        ),
        (
            "@cc_compatibility_proxy//:proxy.bzl",
            include_str!("../host-repos/cc_compatibility_proxy/proxy.bzl"),
        ),
        (
            "@python_version_repo//:py_version.bzl",
            include_str!("../host-repos/python_version_repo/py_version.bzl"),
        ),
        (
            "@bazel_features//:features.bzl",
            include_str!("../host-repos/bazel_features/features.bzl"),
        ),
        // Fetch R1 (RazelFetchPlan §3): WORKSPACE dep-init stubs — repo declarations made
        // by these initializers are either recorded via the recorder (when they call
        // tf_http_archive-shaped rules) or are host/vendored concerns; the loads must
        // resolve so the chain evaluates. Host rows beat ruleset-prefix shims.
        (
            "@bazel_features//:deps.bzl",
            include_str!("../host-repos/bazel_features/deps.bzl"),
        ),
        (
            "@rules_shell//shell:repositories.bzl",
            include_str!("../host-repos/rules_shell/shell/repositories.bzl"),
        ),
        (
            "@bazel_tools//tools/build_defs/repo:http.bzl",
            include_str!("../host-repos/bazel_tools/tools/build_defs/repo/http.bzl"),
        ),
        (
            "@bazel_tools//tools/build_defs/repo:utils.bzl",
            include_str!("../host-repos/bazel_tools/tools/build_defs/repo/utils.bzl"),
        ),
        (
            "@bazel_tools//tools/build_defs/repo:local.bzl",
            include_str!("../host-repos/bazel_tools/tools/build_defs/repo/local.bzl"),
        ),
        (
            "@bazel_tools//tools/build_defs/repo:java.bzl",
            include_str!("../host-repos/bazel_tools/tools/build_defs/repo/java.bzl"),
        ),
        (
            "@bazel_tools//tools/build_defs/repo:git.bzl",
            include_str!("../host-repos/bazel_tools/tools/build_defs/repo/git.bzl"),
        ),
        (
            "@rules_python//python:repositories.bzl",
            include_str!("../host-repos/rules_python/python/repositories.bzl"),
        ),
        (
            "@rules_python//python:versions.bzl",
            include_str!("../host-repos/rules_python/python/versions.bzl"),
        ),
        (
            "@rules_python//python:pip.bzl",
            include_str!("../host-repos/rules_python/python/pip.bzl"),
        ),
        (
            "@pypi//:requirements.bzl",
            include_str!("../host-repos/pypi/requirements.bzl"),
        ),
        (
            "@rules_jvm_external//:defs.bzl",
            include_str!("../host-repos/rules_jvm_external/defs.bzl"),
        ),
        (
            "@io_bazel_rules_closure//closure:defs.bzl",
            include_str!("../host-repos/io_bazel_rules_closure/closure/defs.bzl"),
        ),
        (
            "@rules_pkg//:deps.bzl",
            include_str!("../host-repos/rules_pkg/deps.bzl"),
        ),
        (
            "@llvm-raw//utils/bazel:configure.bzl",
            include_str!("../host-repos/llvm-raw/utils/bazel/configure.bzl"),
        ),
        (
            "@bazel_toolchains//repositories:repositories.bzl",
            include_str!("../host-repos/bazel_toolchains/repositories/repositories.bzl"),
        ),
        (
            "@build_bazel_apple_support//lib:repositories.bzl",
            include_str!("../host-repos/build_bazel_apple_support/lib/repositories.bzl"),
        ),
        (
            "@build_bazel_rules_apple//apple:repositories.bzl",
            include_str!("../host-repos/build_bazel_rules_apple/apple/repositories.bzl"),
        ),
        (
            "@build_bazel_rules_swift//swift:repositories.bzl",
            include_str!("../host-repos/build_bazel_rules_swift/swift/repositories.bzl"),
        ),
        (
            "@com_github_grpc_grpc//bazel:grpc_extra_deps.bzl",
            include_str!("../host-repos/com_github_grpc_grpc/bazel/grpc_extra_deps.bzl"),
        ),
        (
            "@local_config_android//:android.bzl",
            include_str!("../host-repos/local_config_android/android.bzl"),
        ),
        (
            "@rules_foreign_cc//foreign_cc:repositories.bzl",
            include_str!("../host-repos/rules_foreign_cc/foreign_cc/repositories.bzl"),
        ),
        (
            "@com_google_googleapis//:repository_rules.bzl",
            include_str!("../host-repos/com_google_googleapis/repository_rules.bzl"),
        ),
        (
            "@cuda_redist_json//:distributions.bzl",
            include_str!("../host-repos/cuda_redist_json/distributions.bzl"),
        ),
        (
            "@nvshmem_redist_json//:distributions.bzl",
            include_str!("../host-repos/nvshmem_redist_json/distributions.bzl"),
        ),
        (
            "@bazel_tools//tools/cpp:cc_configure.bzl",
            include_str!("../host-repos/bazel_tools/tools/cpp/cc_configure.bzl"),
        ),
        (
            "@bazel_tools//tools/cpp:lib_cc_configure.bzl",
            include_str!("../host-repos/bazel_tools/tools/cpp/lib_cc_configure.bzl"),
        ),
        (
            "@bazel_tools//tools/cpp:windows_cc_configure.bzl",
            include_str!("../host-repos/bazel_tools/tools/cpp/windows_cc_configure.bzl"),
        ),
    ];
    // The generated CUDA/NCCL/NVSHMEM/hermetic-LLVM redist repos all expose
    // `//:version.bzl` with a VERSION constant — one shared no-CUDA stub serves every
    // such repo (fetch R1).
    if (label.starts_with("@cuda_")
        || label.starts_with("@nccl")
        || label.starts_with("@nvshmem")
        || label.starts_with("@llvm_"))
        && label.ends_with("//:version.bzl")
    {
        return Some(include_str!("../host-repos/cuda_redist/version.bzl"));
    }
    HOST.iter().find(|(k, _)| *k == label).map(|(_, v)| *v)
}

/// The host BUILD for a canonical package (`@repo//pkg`), if razel provides one — the
/// package-level twin of [`host_bzl`] (Bazel built-in packages like @bazel_tools//tools/cpp).
pub(crate) fn host_build(pkg: &str) -> Option<&'static str> {
    const HOST: &[(&str, &str)] = &[
        (
            "@bazel_tools//tools/cpp",
            include_str!("../host-repos/bazel_tools/tools/cpp/BUILD"),
        ),
        (
            "@bazel_tools//tools/proto",
            include_str!("../host-repos/bazel_tools/tools/proto/BUILD"),
        ),
        // The proxy repo's root package: `@cc_compatibility_proxy//:symbols_bzl` is depped by
        // rules_cc's bzl_libraries (and through them TF's doc targets).
        (
            "@cc_compatibility_proxy//",
            include_str!("../host-repos/cc_compatibility_proxy/BUILD"),
        ),
        (
            "@bazel_features//",
            include_str!("../host-repos/bazel_features/BUILD"),
        ),
        (
            "@compatibility_proxy//",
            include_str!("../host-repos/compatibility_proxy/BUILD"),
        ),
        (
            "@proto_bazel_features//",
            include_str!("../host-repos/proto_bazel_features/BUILD"),
        ),
        (
            "@rules_python//python",
            include_str!("../host-repos/rules_python/python/BUILD"),
        ),
        (
            "@local_config_cuda//cuda",
            include_str!("../host-repos/local_config_cuda/cuda/BUILD"),
        ),
        (
            "@local_config_tensorrt//",
            include_str!("../host-repos/local_config_tensorrt/BUILD"),
        ),
        (
            "@local_config_rocm//rocm",
            include_str!("../host-repos/local_config_rocm/rocm/BUILD"),
        ),
        (
            "@local_config_sycl//sycl",
            include_str!("../host-repos/local_config_sycl/sycl/BUILD"),
        ),
    ];
    HOST.iter().find(|(k, _)| *k == pkg).map(|(_, v)| *v)
}

/// Conditions in razel's host-materialized generated repos that are FALSE by construction on the
/// CPU-only host (`@local_config_cuda//:is_cuda_enabled`, …). `select()` treats them as declared
/// non-matching config_settings — the same answer the generated repo's BUILD would give.
pub(crate) fn host_false_condition(canon: &str) -> bool {
    const FALSE_REPOS: &[&str] = &[
        "@local_config_cuda//",
        "@local_config_rocm//",
        "@local_config_sycl//",
        "@local_config_tensorrt//",
    ];
    FALSE_REPOS.iter().any(|p| canon.starts_with(p))
}
