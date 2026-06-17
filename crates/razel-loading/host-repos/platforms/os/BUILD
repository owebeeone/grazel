# razel host-repo slice (P5.0, §13/R6): the `@platforms//os` constraint_values the `@crates` graph's
# `@rules_rust//rust/platform:<triple>` config_settings reference. Vendored for dogfood-clean q4
# query; trimmed to the referenced subset (the real @platforms//os BUILD has ~30 values).
package(default_visibility = ["//visibility:public"])

constraint_setting(name = "os")

constraint_value(name = "osx", constraint_setting = ":os")
constraint_value(name = "linux", constraint_setting = ":os")
constraint_value(name = "none", constraint_setting = ":os")
constraint_value(name = "wasi", constraint_setting = ":os")
constraint_value(name = "windows", constraint_setting = ":os")
constraint_value(name = "nixos", constraint_setting = ":os")

# Exposes this BUILD as compile_data for razel-loading's `include_str!` (P5.4 dogfood); inert for query.
filegroup(name = "files", srcs = ["BUILD"], visibility = ["//visibility:public"])
