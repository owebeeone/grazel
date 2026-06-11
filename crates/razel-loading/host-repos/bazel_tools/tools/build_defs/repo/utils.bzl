# Host materialization (fetch R1): repo utils — `maybe` declares iff not already declared;
# razel's recorder dedups by name downstream, so it always forwards.
def maybe(repo_rule, name, **kwargs):
    repo_rule(name = name, **kwargs)

# repository_ctx operations — extraction-time no-ops (fetch R1 records, never executes).
def patch(ctx, patches = None, patch_cmds = None, patch_tool = None, patch_args = None, **kwargs):
    pass

def workspace_and_buildfile(ctx):
    pass

def update_attrs(orig, keys, override):
    return orig

def read_netrc(ctx, filename):
    return {}

def read_user_netrc(ctx):
    return {}

def use_netrc(netrc, urls, patterns):
    return {}

def get_auth(ctx, urls):
    return {}
