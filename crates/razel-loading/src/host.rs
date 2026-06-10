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
    ];
    HOST.iter().find(|(k, _)| *k == label).map(|(_, v)| *v)
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
