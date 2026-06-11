# Host materialization (round 39): Bazel's built-in @bazel_tools//src/conditions —
# host-true config_settings (adapted from the real bazel_tools; constraint forms razel
# models). ruy's config_setting_group members resolve here.
package(default_visibility = ["//visibility:public"])

config_setting(name = "windows", constraint_values = ["@platforms//os:windows"])

config_setting(name = "windows_msvc", constraint_values = ["@platforms//os:windows"])

config_setting(name = "windows_msys", constraint_values = ["@platforms//os:windows"])

config_setting(name = "darwin", constraint_values = ["@platforms//os:macos"])

config_setting(
    name = "darwin_x86_64",
    constraint_values = ["@platforms//os:macos", "@platforms//cpu:x86_64"],
)

config_setting(
    name = "darwin_arm64",
    constraint_values = ["@platforms//os:macos", "@platforms//cpu:arm64"],
)

config_setting(name = "linux", constraint_values = ["@platforms//os:linux"])

config_setting(
    name = "linux_x86_64",
    constraint_values = ["@platforms//os:linux", "@platforms//cpu:x86_64"],
)

config_setting(
    name = "linux_aarch64",
    constraint_values = ["@platforms//os:linux", "@platforms//cpu:aarch64"],
)

config_setting(name = "freebsd", constraint_values = ["@platforms//os:freebsd"])

config_setting(name = "openbsd", constraint_values = ["@platforms//os:openbsd"])
