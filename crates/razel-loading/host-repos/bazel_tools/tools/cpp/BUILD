# razel materialization of @bazel_tools//tools/cpp (Bazel's built-in cc package):
# the well-known stand-in targets real rules depend on. Declare-only.
cc_library(name = "malloc")
cc_library(name = "empty_lib")
cc_library(name = "link_extra_lib")
filegroup(name = "empty", srcs = [])
filegroup(name = "current_cc_toolchain", srcs = [])
filegroup(name = "grep-includes", srcs = [])
toolchain_type(name = "toolchain_type")
config_setting(name = "compiler", values = {})
