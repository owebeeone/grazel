# razel materialization of @compatibility_proxy (rules_java's host-dispatch seam — the java
# twin of @cc_compatibility_proxy). razel-as-host: providers are host Starlark; rule callables
# and java_common absorb until the java lane is driven (registered debt).
JavaInfo = provider(
    doc = "Java provider (razel host materialization).",
    fields = ["transitive_runtime_jars", "transitive_compile_time_jars", "java_outputs"],
)
JavaPluginInfo = provider(
    doc = "Java plugin provider (razel host materialization).",
    fields = ["plugins", "api_generating_plugins"],
)
java_common = razel_host_absorb
java_common_internal_compile = razel_host_absorb
java_info_internal_merge = razel_host_absorb
java_info_to_implicit_exportable = razel_host_absorb
java_binary = razel_host_absorb
java_import = razel_host_absorb
java_library = razel_host_absorb
java_package_configuration = razel_host_absorb
java_plugin = razel_host_absorb
java_runtime = razel_host_absorb
java_test = razel_host_absorb
java_toolchain = razel_host_absorb
http_jar = razel_host_absorb
