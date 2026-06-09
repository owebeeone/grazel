# razel's cc_toolchain_config_lib — the real Bazel cc-config constructor API
# (feature/flag_set/flag_group/action_config/tool) plus a `cc_common` shim whose
# `create_cc_toolchain_config_info` captures features + action_configs (the FeatureConfig razel's
# Constrain interpreter consumes). Prepended to every config razel evaluates so configs are written
# in the REAL API (RazelStarlarkBoundaryPlan §10 A5a). A5b (→ Phase D) evaluates the ACTUAL @rules_cc
# library over the host-generated config; this razel-provided lib is the Phase-A foundation.

def flag_group(flags = [], iterate_over = None, expand_if_available = None):
    return struct(flags = flags, iterate_over = iterate_over, expand_if_available = expand_if_available)

def flag_set(actions = [], with_features = [], flag_groups = []):
    return struct(actions = actions, with_features = with_features, flag_groups = flag_groups)

def feature(name, enabled = False, flag_sets = [], implies = [], requires = [], provides = []):
    return struct(
        name = name,
        enabled = enabled,
        flag_sets = flag_sets,
        implies = implies,
        requires = requires,
        provides = provides,
    )

def action_config(action_name, tools = []):
    return struct(action_name = action_name, tools = tools)

def tool(path, with_features = []):
    return struct(path = path, with_features = with_features)

def _create_cc_toolchain_config_info(ctx = None, features = [], action_configs = [], **kwargs):
    # razel ingests features + action_configs into its FeatureConfig; the host-attr kwargs
    # (cxx_builtin_include_directories, toolchain_identifier, …) are absorbed for now (A5b/Phase D
    # consume them when razel eats the actual generated config + its BUILD attrs).
    return struct(features = features, action_configs = action_configs)

cc_common = struct(create_cc_toolchain_config_info = _create_cc_toolchain_config_info)
