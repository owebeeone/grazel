# razel host-repo slice (P5.0, §13/R6): the `@platforms//` root — `:incompatible`, the
# `target_compatible_with` default-arm marker every generated `@crates` crate references (§5.4).
# Vendored faithfully (matches @platforms' real `incompatible_setting`/`incompatible` pair).
package(default_visibility = ["//visibility:public"])

constraint_setting(name = "incompatible_setting")

constraint_value(name = "incompatible", constraint_setting = ":incompatible_setting")
