# razel materialization of @bazel_tools (the HOST's built-in repo — Bazel ships it; razel, as the
# host, provides it). action_names: a verbatim re-export of rules_cc's copy (the upstream single
# source these constants mirror).
load("@rules_cc//cc:action_names.bzl", _names = "ACTION_NAMES")

ACTION_NAMES = _names
C_COMPILE_ACTION_NAME = _names.c_compile
CPP_COMPILE_ACTION_NAME = _names.cpp_compile
CPP_LINK_DYNAMIC_LIBRARY_ACTION_NAME = _names.cpp_link_dynamic_library
CPP_LINK_EXECUTABLE_ACTION_NAME = _names.cpp_link_executable
CPP_LINK_NODEPS_DYNAMIC_LIBRARY_ACTION_NAME = _names.cpp_link_nodeps_dynamic_library
CPP_LINK_STATIC_LIBRARY_ACTION_NAME = _names.cpp_link_static_library
