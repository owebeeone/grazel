# Host macro (fetch R5): rules_python's spoke targets over an unpacked wheel — the
# loading-grade razel semantics for the byte-faithful generated spoke BUILDs.
def whl_library_targets(
        name,
        dependencies = [],
        entry_points = {},
        **kwargs):
    py_library(
        name = "pkg",
        srcs = native.glob(["site-packages/**/*.py"], allow_empty = True),
        deps = dependencies,
    )
    native.filegroup(name = "whl", srcs = [name])
    native.filegroup(
        name = "data",
        srcs = native.glob(["site-packages/**/*"], allow_empty = True),
    )
    native.filegroup(
        name = "dist_info",
        srcs = native.glob(["site-packages/*.dist-info/**"], allow_empty = True),
    )
