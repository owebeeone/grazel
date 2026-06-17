# Corpus case: cross-package visibility — gates `query visible()` (P6). filegroups (no sources) so
# the gate is pure visibility: public / //b:__pkg__ / //b:__subpackages__ / default-private.

filegroup(name = "a_public", srcs = [], visibility = ["//visibility:public"])

filegroup(name = "a_to_b", srcs = [], visibility = ["//corpus/rust/visibility/b:__pkg__"])

filegroup(name = "a_subpkg", srcs = [], visibility = ["//corpus/rust/visibility/b:__subpackages__"])

filegroup(name = "a_private", srcs = [])
