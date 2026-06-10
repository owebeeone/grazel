# Host materialization of @rules_python//python's BUILD — ONLY the bzl_library doc targets
# the vendored tree deps (@xla//xla/tsl's bzl_library graph: `:defs_bzl`, `:py_library_bzl`).
# rules_python itself is a razel ruleset shim (no vendored files), so these are filegroups:
# skylib's real bzl_library fail()s on empty srcs+deps, and consumers only read `.files`.
filegroup(
    name = "defs_bzl",
    srcs = [],
    visibility = ["//visibility:public"],
)

filegroup(
    name = "py_library_bzl",
    srcs = [],
    visibility = ["//visibility:public"],
)
