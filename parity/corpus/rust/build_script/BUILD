load("@rules_rust//rust:defs.bzl", "rust_library")
load("@rules_rust//cargo:defs.bzl", "cargo_build_script")

# Corpus case: a rust_library with its cargo_build_script EDGE (RazelParityHarness §9, plan P3.11
# rescoped). Mimics blake3's `:build_script_build` shape locally — the build script emits a
# `rustc-cfg` the crate consumes — so razel's §4.3 build-script edge (P3.6→P3.10) is verified
# against Bazel's action graph WITHOUT the external `@crates` integration (which razel doesn't
# resolve yet). The crate's Rustc runs through a process-wrapper consuming the build script's
# flags-file + OUT_DIR; both razel and Bazel do this (normalized away by the wrapper-prefix rule).

cargo_build_script(
    name = "build_script_build",
    srcs = ["build.rs"],
    crate_name = "build_script_build",
    crate_root = "build.rs",
    edition = "2021",
)

rust_library(
    name = "withbs",
    srcs = ["lib.rs"],
    crate_root = "lib.rs",
    edition = "2021",
    deps = [":build_script_build"],
)
