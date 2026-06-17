load("@rules_rust//rust:defs.bzl", "rust_library", "rust_test")

# Corpus case: a test-bearing package — gates `query tests()` against bazel (P6.Q5). The library is
# DROPPED by tests(); the two rust_test rules are KEPT; the test_suite is EXPANDED into its members.

rust_library(
    name = "lib",
    srcs = ["lib.rs"],
    edition = "2021",
)

rust_test(
    name = "lib_test",
    crate = ":lib",
    edition = "2021",
)

rust_test(
    name = "extra_test",
    srcs = ["extra_test.rs"],
    edition = "2021",
)

test_suite(
    name = "suite",
    tests = [
        ":lib_test",
        ":extra_test",
    ],
)
