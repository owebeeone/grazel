# razel host-repo slice (P5.0, §13/R6): the `@rules_rust//rust/platform` config_settings the `@crates`
# graph references — the 7 target triples that appear in generated `select()` arms. Vendored (not
# Bazel's external/ tree, and NOT the `declare_config_settings()` macro chain) so q4 query is
# dogfood-clean. constraint_values are captured verbatim from `bazel query --output=build` (rules_rust
# 0.70); trimmed to the referenced subset (the real package declares ~62). Add a config_setting as the
# graph demands more.
package(default_visibility = ["//visibility:public"])

# wasm32-wasip1 selects on this rules_rust-local WASI version constraint.
constraint_setting(name = "wasi_version", default_constraint_value = ":wasi_preview_1")

constraint_value(name = "wasi_preview_1", constraint_setting = ":wasi_version")

config_setting(
    name = "aarch64-apple-darwin",
    constraint_values = ["@platforms//cpu:aarch64", "@platforms//os:osx"],
)

config_setting(
    name = "aarch64-unknown-linux-gnu",
    constraint_values = ["@platforms//cpu:aarch64", "@platforms//os:linux"],
)

config_setting(
    name = "wasm32-unknown-unknown",
    constraint_values = ["@platforms//cpu:wasm32", "@platforms//os:none"],
)

config_setting(
    name = "wasm32-wasip1",
    constraint_values = ["@platforms//cpu:wasm32", "@platforms//os:wasi", ":wasi_preview_1"],
)

config_setting(
    name = "x86_64-pc-windows-msvc",
    constraint_values = ["@platforms//cpu:x86_64", "@platforms//os:windows"],
)

config_setting(
    name = "x86_64-unknown-linux-gnu",
    constraint_values = ["@platforms//cpu:x86_64", "@platforms//os:linux"],
)

config_setting(
    name = "x86_64-unknown-nixos-gnu",
    constraint_values = ["@platforms//cpu:x86_64", "@platforms//os:nixos"],
)

# Exposes this BUILD as compile_data for razel-loading's `include_str!` (P5.4 dogfood); inert for query.
filegroup(name = "files", srcs = ["BUILD"], visibility = ["//visibility:public"])
