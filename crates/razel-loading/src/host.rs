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
            "@cc_compatibility_proxy//:symbols.bzl",
            include_str!("../host-repos/cc_compatibility_proxy/symbols.bzl"),
        ),
        (
            "@cc_compatibility_proxy//:proxy.bzl",
            include_str!("../host-repos/cc_compatibility_proxy/proxy.bzl"),
        ),
    ];
    HOST.iter().find(|(k, _)| *k == label).map(|(_, v)| *v)
}
