# Host stub (fetch R1): Bazel's Windows cc-autoconf helpers — non-Windows host, no-ops.
def find_msvc_tool(repository_ctx, vc_path, tool, **kwargs):
    return None

def find_vc_path(repository_ctx):
    return None

def setup_vc_env_vars(repository_ctx, vc_path, **kwargs):
    return {}
