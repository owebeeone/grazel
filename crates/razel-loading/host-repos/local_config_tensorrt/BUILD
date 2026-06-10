# Host materialization of @local_config_tensorrt's root BUILD — only the bzl_library doc
# target the vendored tree deps (`:build_defs_bzl`); build_defs.bzl is host-served.
load("@bazel_skylib//:bzl_library.bzl", "bzl_library")

bzl_library(
    name = "build_defs_bzl",
    srcs = ["build_defs.bzl"],
    visibility = ["//visibility:public"],
)
