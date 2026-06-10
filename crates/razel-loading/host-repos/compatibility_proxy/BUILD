# Host materialization of @compatibility_proxy's root BUILD (rules_python's compatibility
# proxy — same shape as @cc_compatibility_proxy): `:proxy_bzl` is depped by bzl_libraries.
load("@bazel_skylib//:bzl_library.bzl", "bzl_library")

bzl_library(
    name = "proxy_bzl",
    srcs = ["proxy.bzl"],
    visibility = ["//visibility:public"],
)
