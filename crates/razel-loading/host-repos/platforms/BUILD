# razel host-repo slice (P5.0, §13/R6): the `@platforms//` root — `:incompatible`, the
# `target_compatible_with` default-arm marker every generated `@crates` crate references (§5.4).
# Vendored faithfully (matches @platforms' real `incompatible_setting`/`incompatible` pair).
package(default_visibility = ["//visibility:public"])

constraint_setting(name = "incompatible_setting")

constraint_value(name = "incompatible", constraint_setting = ":incompatible_setting")

# Exposes this BUILD as compile_data for razel-loading's `include_str!` (host.rs bakes this slice in);
# a sub-package's files need an explicit `:files` (the parent glob stops here). Inert for query (P5.4).
filegroup(name = "files", srcs = ["BUILD"], visibility = ["//visibility:public"])
