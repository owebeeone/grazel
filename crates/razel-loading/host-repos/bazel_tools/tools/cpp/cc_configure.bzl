# Host stub (fetch R1): Bazel's cc autoconf — razel's cc posture is the host toolchain
# (CcToolchainMode); the WORKSPACE-time autoconf is a no-op here.
def cc_configure(**kwargs):
    pass

def cc_autoconf_impl(repository_ctx, overriden_tools = {}):
    pass

MSVC_ENVVARS = []
