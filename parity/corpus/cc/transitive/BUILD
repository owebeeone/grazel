load("@rules_cc//cc:defs.bzl", "cc_library")

# Corpus case: transitive cc (util → base). Exercises the three cc actions
# (CppModuleMap, CppCompile, CppArchive), transitive-header propagation (util's compile
# reads base.h), and ordered link inputs — the cc baseline (RazelParityHarness.md §9).

cc_library(
    name = "base",
    srcs = ["base.cc"],
    hdrs = ["base.h"],
)

cc_library(
    name = "util",
    srcs = ["util.cc"],
    hdrs = ["util.h"],
    deps = [":base"],
)
