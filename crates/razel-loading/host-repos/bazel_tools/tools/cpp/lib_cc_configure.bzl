# Host stub (fetch R1): Bazel cc-autoconf helpers consulted by configure-time .bzl.
def auto_configure_fail(msg):
    fail("auto configure: " + msg)

def auto_configure_warning(msg):
    pass

def get_cpu_value(repository_ctx):
    return "darwin_arm64"

def escape_string(s):
    return s

def which(repository_ctx, cmd, default = None):
    return default

def execute(repository_ctx, command, **kwargs):
    return ""

def resolve_labels(repository_ctx, labels):
    return {l: l for l in labels}
