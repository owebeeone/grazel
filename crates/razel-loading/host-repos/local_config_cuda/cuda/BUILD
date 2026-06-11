# Host materialization of @local_config_cuda//cuda's BUILD — only the bzl_library doc
# target the vendored tree deps (`:build_defs_bzl`); build_defs.bzl is host-served.
load("@bazel_skylib//:bzl_library.bzl", "bzl_library")

bzl_library(
    name = "build_defs_bzl",
    srcs = ["build_defs.bzl"],
    visibility = ["//visibility:public"],
)

# No-CUDA host posture (round 43): the header/runtime targets TF deps unconditionally —
# empty filegroups (loud at ACTION time if anything ever consumes them for real).
filegroup(
    name = "cuda_headers",
    srcs = [],
    visibility = ["//visibility:public"],
)

filegroup(
    name = "cudart_static",
    srcs = [],
    visibility = ["//visibility:public"],
)

filegroup(
    name = "cuda_driver",
    srcs = [],
    visibility = ["//visibility:public"],
)

filegroup(
    name = "cudart",
    srcs = [],
    visibility = ["//visibility:public"],
)

filegroup(
    name = "cub_headers",
    srcs = [],
    visibility = ["//visibility:public"],
)
