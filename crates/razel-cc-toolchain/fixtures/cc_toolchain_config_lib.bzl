# razel's cc_toolchain_config_lib — a FAITHFUL SUBSET of Bazel's real cc-config constructor API,
# prepended to every config razel evaluates so configs are written in the real API (RazelStarlark-
# BoundaryPlan §10 A5a). The shim's constructors emit EXACTLY the fields razel's extractor
# (`parse_feature_config`) reads — they round-trip (F34). Constructors razel does not yet ingest
# (env_set/make_variable/tool_path/artifact_name_pattern) are still accepted so a realistic config
# evaluates without error; `create_cc_toolchain_config_info` absorbs them. A5b (→ Phase D) evaluates
# the ACTUAL @rules_cc library over the host-generated config instead of this shim.

def flag_group(
        flags = [],
        iterate_over = None,
        expand_if_available = None,
        expand_if_not_available = None,
        expand_if_true = None,
        expand_if_false = None,
        expand_if_equal = None,
        flag_groups = []):
    return struct(
        flags = flags,
        iterate_over = iterate_over,
        expand_if_available = expand_if_available,
        expand_if_not_available = expand_if_not_available,
        expand_if_true = expand_if_true,
        expand_if_false = expand_if_false,
        expand_if_equal = expand_if_equal,
        flag_groups = flag_groups,
    )

def flag_set(actions = [], with_features = [], flag_groups = []):
    return struct(actions = actions, with_features = with_features, flag_groups = flag_groups)

def with_feature_set(features = [], not_features = []):
    return struct(features = features, not_features = not_features)

def feature(name, enabled = False, flag_sets = [], implies = [], requires = [], provides = [], env_sets = []):
    return struct(
        name = name,
        enabled = enabled,
        flag_sets = flag_sets,
        implies = implies,
        requires = requires,
        provides = provides,
        env_sets = env_sets,
    )

def feature_set(features = []):
    return struct(features = features)

# Bazel's `variable_with_value(name, value)` — razel's `expand_if_equal` reads `.variable`/`.value`.
def variable_with_value(name, value):
    return struct(variable = name, value = value)

def action_config(action_name, enabled = False, tools = [], flag_sets = [], implies = [], env_sets = []):
    return struct(
        action_name = action_name,
        enabled = enabled,
        tools = tools,
        flag_sets = flag_sets,
        implies = implies,
        env_sets = env_sets,
    )

def tool(path = "", with_features = [], execution_requirements = []):
    return struct(path = path, with_features = with_features, execution_requirements = execution_requirements)

# --- accepted but not yet ingested (so a realistic config evaluates; create absorbs them) ---
def env_set(actions = [], env_entries = [], with_features = []):
    return struct(actions = actions, env_entries = env_entries, with_features = with_features)

def env_entry(key, value):
    return struct(key = key, value = value)

def make_variable(name, value):
    return struct(name = name, value = value)

def tool_path(name, path):
    return struct(name = name, path = path)

def artifact_name_pattern(category_name, prefix, extension):
    return struct(category_name = category_name, prefix = prefix, extension = extension)

def _create_cc_toolchain_config_info(ctx = None, features = [], action_configs = [], **kwargs):
    # razel ingests features + action_configs into its FeatureConfig; the host-attr kwargs
    # (cxx_builtin_include_directories, toolchain_identifier, env_set, make_variables, …) are
    # absorbed for now (A5b/Phase D consume them when razel eats the actual generated config).
    return struct(features = features, action_configs = action_configs)

cc_common = struct(create_cc_toolchain_config_info = _create_cc_toolchain_config_info)
