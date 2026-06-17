# razel host-repo slice (P5.0, §13/R6): the `@platforms//cpu` constraint_values the `@crates`
# graph's `@rules_rust//rust/platform:<triple>` config_settings reference. Vendored (not Bazel's
# external/ tree) so q4 query is dogfood-clean. Trimmed to the referenced subset; add a row as the
# graph demands more (the real @platforms//cpu BUILD has ~40 values).
package(default_visibility = ["//visibility:public"])

constraint_setting(name = "cpu")

constraint_value(name = "aarch64", constraint_setting = ":cpu")
constraint_value(name = "wasm32", constraint_setting = ":cpu")
constraint_value(name = "x86_64", constraint_setting = ":cpu")

# Exposes this BUILD as compile_data for razel-loading's `include_str!` (host.rs bakes this slice
# in). A sub-package's files can't be reached by the parent glob, so it's pulled in explicitly via
# `:files` — like the other host-repos slices (P5.4 dogfood). Inert for query (additive target).
filegroup(name = "files", srcs = ["BUILD"], visibility = ["//visibility:public"])
