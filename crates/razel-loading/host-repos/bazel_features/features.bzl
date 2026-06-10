# razel host materialization of @bazel_features (generated — bazel-contrib/bazel_features
# probes the host Bazel's version). razel-as-host = modern posture (>= Bazel 8): every
# version-gate rules_cc consults answers True. Members NOT listed error loudly when a new
# consumer probes them — extend per use, don't pre-absorb.
bazel_features = struct(
    cc = struct(
        cc_common_is_in_rules_cc = True,
        supports_path_variable_patterns = True,
        supports_starlarkified_toolchains = True,
    ),
    external_deps = struct(
        extension_metadata_has_reproducible = True,
    ),
    toolchains = struct(
        has_use_target_platform_constraints = True,
    ),
)
