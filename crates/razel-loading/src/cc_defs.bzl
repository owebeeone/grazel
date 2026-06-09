# razel's own cc:defs.bzl — `cc_library` over the razel_cc engine (RazelStarlarkBoundaryPlan §6/§10).
# Bundled into the binary (A3/4·ii) and served for `@rules_cc//cc:defs.bzl`; here (·i) it is evaluated
# as source via analyze_starlark. The per-source loop + path model live in Starlark (the rule logic);
# `razel_cc.command_line` is the tight engine seam (Constrain). Target-type logic stays legible.

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

    objects = []
    for src in getattr(ctx.attr, "srcs", []):
        stem = src.rsplit(".", 1)[0]
        obj = "%s/%s.o" % (objs, stem)
        cl = razel_cc.command_line("c++-compile", {
            "source_file": src_prefix + src,
            "output_file": obj,
            "dependency_file": "%s/%s.d" % (objs, stem),
            "minimum_os_version": _SDK,
            "quote_include_paths": [".", bin],
        })
        ctx.actions.run(
            executable = cl[0],
            arguments = cl[1:],
            inputs = [src_prefix + src] + headers,
            outputs = ["%s/%s.d" % (objs, stem), obj],
            mnemonic = "CppCompile",
        )
        objects.append(obj)

    lib = "%s/lib%s.a" % (prefix, name)
    al = razel_cc.command_line("c++-link-static-library", {
        "output_execpath": lib,
        "libraries_to_link": objects,
    })
    ctx.actions.run(
        executable = al[0],
        arguments = al[1:],
        inputs = objects,
        outputs = [lib],
        mnemonic = "CppArchive",
    )

    return [CcInfo(headers = own_headers), DefaultInfo(files = [lib])]

cc_library = rule(implementation = _cc_library_impl, attrs = {})
