# Host materialization of @bazel_features's root BUILD: `:features` is depped by
# rules_cc's bzl_libraries (cc/toolchains/BUILD) and through them TF's doc targets.
load("@bazel_skylib//:bzl_library.bzl", "bzl_library")

bzl_library(
    name = "features",
    srcs = ["features.bzl"],
    visibility = ["//visibility:public"],
)
