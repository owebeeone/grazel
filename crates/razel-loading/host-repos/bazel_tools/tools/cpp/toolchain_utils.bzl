# razel host materialization of @bazel_tools//tools/cpp:toolchain_utils.bzl (host-shipped in
# Bazel). The cc toolchain accessor surface; values absorb until L3 toolchain resolution.
CPP_TOOLCHAIN_TYPE = "@bazel_tools//tools/cpp:toolchain_type"

def find_cpp_toolchain(ctx, *, mandatory = True):
    return razel_host_absorb

def use_cpp_toolchain(mandatory = False):
    return []
