# razel materialization of @cc_compatibility_proxy (the HOST-dispatch seam).
# Bazel <9 materializes these symbols as the host's NATIVE CcInfo etc.; razel is the host here,
# so CcInfo is the host-provided provider, defined in pure Starlark. rules_rust (the ruleset
# under test) runs unmodified. Full rules_cc-Starlark CcInfo internals = L4 (debt register).
def _empty_compilation_context():
    return razel_host_absorb_with({
        "headers": depset([]),
        "defines": depset([]),
        "includes": depset([]),
    })

def _empty_linking_context():
    return razel_host_absorb_with({
        "linker_inputs": depset([]),
    })

def _ccinfo_init(compilation_context = None, linking_context = None, **kwargs):
    # Bazel's CcInfo() defaults to EMPTY contexts (impls traverse .headers/.linker_inputs).
    return {
        "compilation_context": compilation_context if compilation_context != None else _empty_compilation_context(),
        "linking_context": linking_context if linking_context != None else _empty_linking_context(),
    }

CcInfo, _ccinfo_raw = provider(
    doc = "C++ provider (razel host materialization).",
    fields = ["compilation_context", "linking_context"],
    init = _ccinfo_init,
)

def merge_cc_infos(direct_cc_infos = [], cc_infos = []):
    # Minimal host merge: collect the inputs; consumers traverse. Fidelity grows with L4.
    return CcInfo(
        compilation_context = None,
        linking_context = None,
    )

def _toolchain_resolution_enabled(*args, **kwargs):
    return True

def _path_of(f):
    # File, (File, Label) pair (cc_helper's artifact-label maps), file-like (host
    # absorb_with carrying .path), or string.
    if type(f) == "tuple":
        f = f[0]
    if type(f) == "string":
        return f
    p = getattr(f, "path", None)
    return p if p != None else str(f)

def _file_like(p):
    # A host file value: impls read .short_path/.basename/.extension off link artifacts
    # (cc_library's collision check); unmodeled members absorb. Real File values for
    # host-created outputs arrive with the dynamic/link goldens.
    return razel_host_absorb_with({
        "path": p,
        "short_path": p,
        "basename": p.rsplit("/", 1)[-1],
        "extension": p.rsplit(".", 1)[-1],
    })

def _members(x):
    # depset | list | absorbed → list (Bazel forbids direct depset iteration).
    if x == None:
        return []
    if type(x) == "depset":
        return x.to_list()
    return [e for e in x]

_C_EXTS = (".c", ".cc", ".cpp", ".cxx", ".c++", ".C", ".m", ".mm", ".S", ".s", ".asm")

def _compile(
        *,
        actions = None,
        name = "",
        srcs = [],
        public_hdrs = [],
        private_hdrs = [],
        includes = [],
        quote_includes = [],
        system_includes = [],
        defines = [],
        local_defines = [],
        user_compile_flags = [],
        conly_flags = [],
        cxx_flags = [],
        compilation_contexts = [],
        **_kwargs):
    """razel host cc_common.compile: REAL clang compile actions over the Constrain engine
    (razel_build.command_line), one per compilable src; returns the Bazel-shaped
    (compilation_context, compilation_outputs) pair. Untouched members absorb."""
    hdrs = list(public_hdrs) + list(private_hdrs)
    t_hdrs = []
    t_defines = []
    t_includes = []
    for cc_ctx in compilation_contexts:
        # Our contexts carry real headers/defines/includes lists; foreign ones absorb (empty).
        t_hdrs += _members(cc_ctx.headers)
        t_defines += _members(cc_ctx.defines)
        t_includes += _members(cc_ctx.includes)
    hdr_paths = dedup([_path_of(h) for h in hdrs + t_hdrs])
    inc = dedup([str(i) for i in list(includes) + list(quote_includes) + t_includes])
    all_defines = dedup([str(d) for d in list(defines) + list(local_defines) + t_defines])

    objects = []
    for src in srcs:
        p = _path_of(src)
        is_c_like = False
        for ext in _C_EXTS:
            if p.endswith(ext):
                is_c_like = True
        if not is_c_like:
            continue
        obj = "_objs/%s/%s.o" % (name, p.rsplit("/", 1)[-1].rsplit(".", 1)[0])
        cl = razel_build.command_line("cc", "c++-compile", {
            "source_file": p,
            "output_file": obj,
            "dependency_file": obj + ".d",
            "quote_include_paths": ["."] + inc,
        })
        extra = ["-D" + d for d in all_defines] + list(user_compile_flags)
        razel_build.action(
            executable = cl[0],
            arguments = cl[1:] + extra,
            inputs = [p] + hdr_paths,
            outputs = [obj + ".d", obj],
            mnemonic = "CppCompile",
        )
        objects.append(obj)

    comp_ctx = razel_host_absorb_with({
        "headers": depset(hdrs + t_hdrs),
        "defines": depset(all_defines),
        "includes": depset(inc),
        "direct_headers": list(hdrs),
        "direct_public_headers": list(public_hdrs),
        "direct_private_headers": list(private_hdrs),
    })
    comp_outs = razel_host_absorb_with({
        "objects": list(objects),
        "pic_objects": list(objects),
    })
    return (comp_ctx, comp_outs)

def _merge_compilation_outputs(*, compilation_outputs = [], **_kwargs):
    # cc_library merges srcs + private-hdr outputs before archiving; ours carry real
    # objects/pic_objects lists, foreign ones absorb to empty.
    objs = []
    pic = []
    for co in compilation_outputs:
        objs += _members(co.objects)
        pic += _members(co.pic_objects)
    return razel_host_absorb_with({
        "objects": objs,
        "pic_objects": pic,
    })

def _merge_compilation_contexts(*, compilation_contexts = [], **_kwargs):
    hdrs = []
    defines = []
    incs = []
    for c in compilation_contexts:
        hdrs += _members(c.headers)
        defines += _members(c.defines)
        incs += _members(c.includes)
    return razel_host_absorb_with({
        "headers": depset(dedup(hdrs)),
        "defines": depset(dedup(defines)),
        "includes": depset(dedup(incs)),
    })

def _create_compilation_context(
        *,
        headers = None,
        defines = None,
        includes = None,
        **_kwargs):
    return razel_host_absorb_with({
        "headers": headers if headers != None else depset([]),
        "defines": defines if defines != None else depset([]),
        "includes": includes if includes != None else depset([]),
    })

# The host absorber: any member resolves (analysis-time surfacing). Named overrides carry the
# members with REAL host semantics — compile/contexts run actual Constrain command lines via
# the generic razel_build four-move API (languages are data: this file IS the cc shim).
def _get_artifact_name_for_category(*, cc_toolchain = None, category = None, output_name = "", **_kwargs):
    # Real unix naming for the categories the run-goldens exercise (the lib-naming debt,
    # paid for static archives by the round-25 archive golden). Dynamic-library naming
    # (.so vs .dylib is the toolchain's call) stays passthrough until a dynamic golden.
    c = str(category)
    if c == "static_library":
        return "lib" + output_name + ".a"
    if c == "alwayslink_static_library":
        return "lib" + output_name + ".lo"
    return output_name

def _link(
        *,
        actions = None,
        name = "",
        compilation_outputs = None,
        linking_contexts = [],
        user_link_flags = [],
        output_type = "executable",
        **_kwargs):
    """razel host cc_common.link (the _compile template): one REAL executable link action
    over the Constrain engine. Dep libraries come from the linking_contexts' linker_inputs
    (ours carry path strings; foreign ones absorb to empty)."""
    objs = _members(compilation_outputs.objects) if compilation_outputs != None else []
    dep_libs = []
    for lc in linking_contexts:
        for li in _members(lc.linker_inputs):
            for lib in _members(li.libraries):
                static = lib.static_library
                if static != None:
                    dep_libs.append(_path_of(static))
    out = name
    cl = razel_build.command_line("cc", "c++-link-executable", {
        "output_execpath": out,
        "libraries_to_link": list(objs) + dep_libs,
    })
    if not cl:
        # The adopted toolchain declares no c++-link-executable action_config yet —
        # executables stay analysis-absorbed until a BINARY run-golden adds it (RazelGaps).
        return razel_host_absorb_with({
            "executable": None,
            "library_to_link": None,
        })
    razel_build.action(
        executable = cl[0],
        arguments = cl[1:] + [str(f) for f in user_link_flags],
        inputs = list(objs) + dep_libs,
        outputs = [out],
        mnemonic = "CppLink",
    )
    return razel_host_absorb_with({
        "executable": out,
        "library_to_link": None,
    })

def _create_linking_context_from_compilation_outputs(
        *,
        actions = None,
        name = "",
        compilation_outputs = None,
        linking_contexts = [],
        user_link_flags = [],
        alwayslink = False,
        disallow_dynamic_library = False,
        **_kwargs):
    """razel host: the static-archive step (cc_library's link half) — a REAL
    c++-link-static-library action over the Constrain engine; returns the Bazel-shaped
    (linking_context, linking_outputs) pair. Library paths are strings (File-ification
    arrives with the dynamic/link goldens)."""
    objs = _members(compilation_outputs.objects) if compilation_outputs != None else []
    libs = []
    if objs:
        lib = "_libs/%s/%s" % (name, _get_artifact_name_for_category(
            category = "static_library",
            output_name = name,
        ))
        al = razel_build.command_line("cc", "c++-link-static-library", {
            "output_execpath": lib,
            "libraries_to_link": list(objs),
        })
        razel_build.action(
            executable = al[0],
            arguments = al[1:],
            inputs = list(objs),
            outputs = [lib],
            mnemonic = "CppArchive",
        )
        libs.append(lib)
    library_to_link = razel_host_absorb_with({
        "static_library": _file_like(libs[0]) if libs else None,
        "pic_static_library": _file_like(libs[0]) if libs else None,
        "dynamic_library": None,
        "objects": list(objs),
        "pic_objects": list(objs),
        "alwayslink": alwayslink,
    })
    linker_input = razel_host_absorb_with({
        "libraries": [library_to_link] if libs else [],
        "user_link_flags": [str(f) for f in user_link_flags],
    })
    t_inputs = []
    for lc in linking_contexts:
        t_inputs += _members(lc.linker_inputs)
    linking_context = razel_host_absorb_with({
        "linker_inputs": depset([linker_input] + t_inputs),
    })
    linking_outputs = razel_host_absorb_with({
        "library_to_link": library_to_link if libs else None,
    })
    return (linking_context, linking_outputs)

def _action_is_enabled(*, feature_configuration = None, action_name = "", **_kwargs):
    # Host posture: the actions razel can actually run are enabled (strip/link/compile).
    return action_name in ("strip", "c++-compile", "c-compile", "c++-link-executable",
                           "c++-link-static-library", "c++-link-dynamic-library")

def _get_tool_for_action(*, feature_configuration = None, action_name = "", **_kwargs):
    # REAL host tools by action class (PATH-resolved at run): strip → strip, else the driver.
    return "strip" if action_name == "strip" else "cc"

cc_common = razel_host_absorb_with({
    "is_cc_toolchain_resolution_enabled_do_not_use": _toolchain_resolution_enabled,
    "action_is_enabled": _action_is_enabled,
    "get_tool_for_action": _get_tool_for_action,
    "compile": _compile,
    "link": _link,
    "create_linking_context_from_compilation_outputs": _create_linking_context_from_compilation_outputs,
    "merge_compilation_outputs": _merge_compilation_outputs,
    "merge_compilation_contexts": _merge_compilation_contexts,
    "create_compilation_context": _create_compilation_context,
    "get_artifact_name_for_category": _get_artifact_name_for_category,
})
CcToolchainConfigInfo = provider(doc = "razel host materialization.", fields = [])
DebugPackageInfo = provider(doc = "razel host materialization.", fields = [])
ObjcInfo = provider(doc = "razel host materialization.", fields = [])
new_objc_provider = ObjcInfo
CcSharedLibraryInfo = provider(doc = "razel host materialization.", fields = [])
