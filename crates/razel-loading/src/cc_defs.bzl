# razel's own cc:defs.bzl — `cc_library` over the razel_build engine (RazelStarlarkBoundaryPlan §6/§10).
# Bundled into the binary (A3/4·ii) and served for `@rules_cc//cc:defs.bzl`; here (·i) it is evaluated
# as source via analyze_starlark. The per-source loop + path model live in Starlark (the rule logic);
# `razel_build.command_line` is the tight engine seam (Constrain). Target-type logic stays legible.

_CFG = "darwin_arm64-fastbuild"  # output-tree segment; normalize() maps it to <cfg> (live cfg: A4 open).
_SDK = "<sdk>"                    # macOS SDK placeholder (host param).

def _cc_library_impl(ctx):
    pkg = ctx.label.package
    name = ctx.label.name
    bin = "bazel-out/%s/bin" % _CFG
    prefix = "%s/%s" % (bin, pkg) if pkg else bin
    objs = "%s/_objs/%s" % (prefix, name)
    src_prefix = pkg + "/" if pkg else ""

    # OWN exported headers (qualified) + deps' transitive (dep.headers — A2a's provides fold).
    # getattr defaults: razel has no attr-schema defaults yet (A2/D), and real BUILDs omit optional
    # attrs (a dep-less cc_library has no `deps`).
    own_headers = [src_prefix + h for h in getattr(ctx.attr, "hdrs", [])]
    headers = own_headers
    for d in getattr(ctx.attr, "deps", []):
        headers = headers + d.headers
    headers = dedup(headers)  # F1: cross-sibling dedup (a diamond must not list base.h twice)

    objects = []
    for src in getattr(ctx.attr, "srcs", []):
        stem = src.rsplit(".", 1)[0]
        obj = "%s/%s.o" % (objs, stem)
        cl = razel_build.command_line("cc", "c++-compile", {
            "source_file": src_prefix + src,
            "output_file": obj,
            "dependency_file": "%s/%s.d" % (objs, stem),
            "minimum_os_version": _SDK,
            "quote_include_paths": [".", bin],
        })
        razel_build.action(
            executable = cl[0],
            arguments = cl[1:],
            inputs = [src_prefix + src] + headers,
            outputs = ["%s/%s.d" % (objs, stem), obj],
            mnemonic = "CppCompile",
        )
        objects.append(obj)

    lib = "%s/lib%s.a" % (prefix, name)
    al = razel_build.command_line("cc", "c++-link-static-library", {
        "output_execpath": lib,
        "libraries_to_link": objects,
    })
    razel_build.action(
        executable = al[0],
        arguments = al[1:],
        inputs = objects,
        outputs = [lib],
        mnemonic = "CppArchive",
    )

    # C3: the generic provider constructor; `libs` feeds cc_binary's link (Phase E).
    razel_build.info("CcInfo", {"hdrs": own_headers, "libs": [lib]})
    return [DefaultInfo(files = [lib])]

cc_library = rule(implementation = _cc_library_impl, attrs = {})

# Phase E (S4): the FAITHFUL cc_binary — compiles into `_objs/<name>/` (CppCompile,
# dotd included) and links via the adopted `c++-link-executable` action_config
# (CppLink). Retires the F21 native fallback; argv fidelity grows against the
# examples graph goldens.
def _cc_binary_impl(ctx):
    pkg = ctx.label.package
    name = ctx.label.name
    bin = "bazel-out/%s/bin" % _CFG
    prefix = "%s/%s" % (bin, pkg) if pkg else bin
    objs = "%s/_objs/%s" % (prefix, name)
    src_prefix = pkg + "/" if pkg else ""

    headers = []
    dep_libs = []
    for d in getattr(ctx.attr, "deps", []):
        headers = headers + d.headers
        dep_libs = dep_libs + d.libs  # CcInfo.libs (folded — see registry)
    headers = dedup(headers)

    objects = []
    for src in getattr(ctx.attr, "srcs", []):
        stem = src.rsplit(".", 1)[0]
        obj = "%s/%s.o" % (objs, stem)
        cl = razel_build.command_line("cc", "c++-compile", {
            "source_file": src_prefix + src,
            "output_file": obj,
            "dependency_file": "%s/%s.d" % (objs, stem),
            "minimum_os_version": _SDK,
            "quote_include_paths": [".", bin],
        })
        razel_build.action(
            executable = cl[0],
            arguments = cl[1:],
            inputs = [src_prefix + src] + headers,
            outputs = ["%s/%s.d" % (objs, stem), obj],
            mnemonic = "CppCompile",
        )
        objects.append(obj)

    out = "%s/%s" % (prefix, name)
    ll = razel_build.command_line("cc", "c++-link-executable", {
        "output_execpath": out,
        "libraries_to_link": objects + dep_libs,
        "minimum_os_version": _SDK,
    })
    razel_build.action(
        executable = ll[0],
        arguments = ll[1:],
        inputs = objects + dep_libs,
        outputs = [out],
        mnemonic = "CppLink",
    )
    return [DefaultInfo(files = [out])]

cc_binary = rule(implementation = _cc_binary_impl, attrs = {})

# cc_test: analysis-wise a cc executable; the test protocol (exit codes, test.log,
# summary) is the `test` verb's job, not the rule's.
cc_test = rule(implementation = _cc_binary_impl, attrs = {})
