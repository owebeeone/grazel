# Ported core of the macOS `local_config_cc` cc_toolchain_config — the *compile* feature subset,
# expressed as evaluable Starlark (the same constructor shape razel-cc-toolchain evaluates). The
# structure is a one-time port of the (stable) upstream template; the flag VALUES are the real
# cc_configure-detected lists (compile_flags / cxx_flags / unfiltered_compile_flags), verbatim.
#
# Reproduces the deterministic core of the captured CppCompile argv. The two host-specific
# features omitted here (random_seed `-frandom-seed=`, macos_minimum_os `-mmacosx-version-min=`)
# are parameterized separately (host params; BazelCcCommandLine.md §"externalize").

def flag_group(flags = [], iterate_over = None, expand_if_available = None):
    return struct(flags = flags, iterate_over = iterate_over, expand_if_available = expand_if_available)

def flag_set(actions = [], flag_groups = []):
    return struct(actions = actions, with_features = [], flag_groups = flag_groups)

def feature(name, enabled = False, flag_sets = []):
    return struct(name = name, enabled = enabled, flag_sets = flag_sets, implies = [], requires = [], provides = [])

def action_config(action_name, tools = []):
    return struct(action_name = action_name, tools = tools)

def tool(path):
    return struct(path = path, with_features = [])

CC = ["c++-compile"]

CONFIG = struct(
    features = [
        feature(name = "default_compile_flags", enabled = True, flag_sets = [
            flag_set(actions = CC, flag_groups = [flag_group(flags = ["-U_FORTIFY_SOURCE"])]),
            # cc_configure-detected `compile_flags`:
            flag_set(actions = CC, flag_groups = [flag_group(flags = [
                "-fstack-protector",
                "-Wall",
                "-Wthread-safety",
                "-Wself-assign",
                "-Wunused-but-set-parameter",
                "-Wno-free-nonheap-object",
                "-fcolor-diagnostics",
                "-fno-omit-frame-pointer",
            ])]),
            # cc_configure-detected `cxx_flags` (c++ actions):
            flag_set(actions = CC, flag_groups = [flag_group(flags = ["-std=c++17"])]),
        ]),
        feature(name = "dependency_file", enabled = True, flag_sets = [
            flag_set(actions = CC, flag_groups = [
                flag_group(flags = ["-MD", "-MF", "%{dependency_file}"], expand_if_available = "dependency_file"),
            ]),
        ]),
        feature(name = "include_paths", enabled = True, flag_sets = [
            flag_set(actions = CC, flag_groups = [
                flag_group(flags = ["-iquote", "%{quote_include_paths}"], iterate_over = "quote_include_paths"),
            ]),
        ]),
        feature(name = "compiler_input_flags", enabled = True, flag_sets = [
            flag_set(actions = CC, flag_groups = [flag_group(flags = ["-c", "%{source_file}"])]),
        ]),
        feature(name = "compiler_output_flags", enabled = True, flag_sets = [
            flag_set(actions = CC, flag_groups = [flag_group(flags = ["-o", "%{output_file}"])]),
        ]),
        # cc_configure-detected `unfiltered_compile_flags` (appended last; coptsFilter-exempt):
        feature(name = "unfiltered_compile_flags", enabled = True, flag_sets = [
            flag_set(actions = CC, flag_groups = [flag_group(flags = [
                "-no-canonical-prefixes",
                "-Wno-builtin-macro-redefined",
                '-D__DATE__="redacted"',
                '-D__TIMESTAMP__="redacted"',
                '-D__TIME__="redacted"',
            ])]),
        ]),
    ],
    action_configs = [
        action_config(action_name = "c++-compile", tools = [tool(path = "cc_wrapper.sh")]),
    ],
)
