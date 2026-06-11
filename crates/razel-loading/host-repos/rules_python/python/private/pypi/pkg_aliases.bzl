# Host macro (fetch R5): rules_python's hub aliases — each package dir aliases into its
# spoke repo. Loading-grade razel semantics for the byte-faithful generated hub BUILDs.
def pkg_aliases(name, actual, **kwargs):
    native.alias(name = name, actual = "@" + actual + "//:pkg")
    native.alias(name = "pkg", actual = "@" + actual + "//:pkg")
    native.alias(name = "whl", actual = "@" + actual + "//:whl")
    native.alias(name = "data", actual = "@" + actual + "//:data")
    native.alias(name = "dist_info", actual = "@" + actual + "//:dist_info")
