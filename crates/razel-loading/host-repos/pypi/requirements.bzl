# Host stub (fetch R1): the pip_parse-GENERATED @pypi hub's init surface. The real hub is
# the round-25 stub-hub posture (a vendor/generation decision), not extraction's concern.
def install_deps(**kwargs):
    pass

def requirement(name):
    return "@pypi//:" + name
