load("@rules_rust//rust:defs.bzl", "rust_library")

# Corpus case: transitive rust (util → base). Mirrors cc/transitive — exercises the Rustc action
# and transitive crate-dep propagation (--extern / -Ldependency), the rust baseline
# (RazelParityHarness.md §9).

rust_library(
    name = "base",
    srcs = ["base.rs"],
    edition = "2021",
)

rust_library(
    name = "util",
    srcs = ["util.rs"],
    edition = "2021",
    deps = [":base"],
)
