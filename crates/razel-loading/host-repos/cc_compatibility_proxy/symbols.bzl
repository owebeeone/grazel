# razel materialization of @cc_compatibility_proxy (the HOST-dispatch seam).
# Bazel <9 materializes these symbols as the host's NATIVE CcInfo etc.; razel is the host here,
# so CcInfo is the host-provided provider, defined in pure Starlark. rules_rust (the ruleset
# under test) runs unmodified. Full rules_cc-Starlark CcInfo internals = L4 (debt register).
CcInfo = provider(
    doc = "C++ provider (razel host materialization).",
    fields = ["compilation_context", "linking_context"],
)

def merge_cc_infos(direct_cc_infos = [], cc_infos = []):
    # Minimal host merge: collect the inputs; consumers traverse. Fidelity grows with L4.
    return CcInfo(
        compilation_context = None,
        linking_context = None,
    )

cc_common = razel_host_absorb  # the host absorber: any member resolves (analysis-time surfacing)
CcToolchainConfigInfo = provider(doc = "razel host materialization.", fields = [])
DebugPackageInfo = provider(doc = "razel host materialization.", fields = [])
ObjcInfo = provider(doc = "razel host materialization.", fields = [])
new_objc_provider = ObjcInfo
CcSharedLibraryInfo = provider(doc = "razel host materialization.", fields = [])
