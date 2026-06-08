# Ported core of the macOS `local_config_cc` cc_toolchain_config — the *compile* feature subset,
# expressed as evaluable Starlark (the same constructor shape razel-cc-toolchain evaluates). The
# structure is a one-time port of the (stable) upstream template; the flag VALUES are the real
# cc_configure-detected lists (compile_flags / cxx_flags / unfiltered_compile_flags), verbatim.
#
# Reproduces the FULL captured CppCompile argv. The two host-specific features (random_seed,
# macos_minimum_os) are included but parameterized — their values flow from variables
# (`%{output_file}`, `%{minimum_os_version}`) supplied per host, not detected here
# (BazelCcCommandLine.md §"externalize").

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
ARCHIVE = ["c++-link-static-library"]

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
        # Host-specific (parameterized via variables — values supplied per host, not detected here):
        feature(name = "random_seed", enabled = True, flag_sets = [
            flag_set(actions = CC, flag_groups = [
                flag_group(flags = ["-frandom-seed=%{output_file}"], expand_if_available = "output_file"),
            ]),
        ]),
        feature(name = "macos_minimum_os", enabled = True, flag_sets = [
            flag_set(actions = CC, flag_groups = [
                flag_group(flags = ["-mmacosx-version-min=%{minimum_os_version}"], expand_if_available = "minimum_os_version"),
            ]),
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
        # The static-archive action (macOS libtool): -D -no_warning_for_no_symbols -static -o <a> <objs>.
        feature(name = "archiver_flags", enabled = True, flag_sets = [
            flag_set(actions = ARCHIVE, flag_groups = [
                flag_group(flags = ["-D", "-no_warning_for_no_symbols", "-static"]),
                flag_group(flags = ["-o", "%{output_execpath}"], expand_if_available = "output_execpath"),
                flag_group(flags = ["%{libraries_to_link}"], iterate_over = "libraries_to_link"),
            ]),
        ]),
    ],
    action_configs = [
        action_config(action_name = "c++-compile", tools = [tool(path = "cc_wrapper.sh")]),
        action_config(action_name = "c++-link-static-library", tools = [tool(path = "/usr/bin/libtool")]),
    ],
)
