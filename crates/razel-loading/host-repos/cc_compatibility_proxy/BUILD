# Host materialization of @cc_compatibility_proxy's root BUILD (rules_cc's
# compatibility-proxy repo rule — cc/extensions.bzl writes the real one). Declares the
# bzl_library targets that rules_cc/TF doc targets dep (`:symbols_bzl`, `:proxy_bzl`).
# srcs name the proxy's own host-materialized .bzl files (the real repo writes them too);
# deps are omitted — consumers only need the targets to resolve.
load("@bazel_skylib//:bzl_library.bzl", "bzl_library")

bzl_library(
    name = "proxy_bzl",
    srcs = ["proxy.bzl"],
    visibility = ["//visibility:public"],
)

bzl_library(
    name = "symbols_bzl",
    srcs = ["symbols.bzl"],
    visibility = ["//visibility:public"],
)
