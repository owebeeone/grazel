# razel host materialization of @bazel_skylib//lib:selects.bzl. Upstream implements
# config_setting_group via native alias/ConfigMatchingProvider chains (Bazel-native semantics
# razel doesn't model); the host provides the same CONTRACT natively
# (razel_config_setting_group). with_or is upstream's pure-Starlark shape.
def _with_or_dict(input_dict):
    output_dict = {}
    for key, value in input_dict.items():
        if type(key) == type(()):
            for k in key:
                output_dict[k] = value
        else:
            output_dict[key] = value
    return output_dict

def _with_or(input_dict, no_match_error = ""):
    return select(_with_or_dict(input_dict), no_match_error = no_match_error)

def _config_setting_group(name, match_any = [], match_all = [], visibility = None):
    razel_config_setting_group(name = name, match_all = match_all, match_any = match_any)

selects = struct(
    with_or = _with_or,
    with_or_dict = _with_or_dict,
    config_setting_group = _config_setting_group,
)
