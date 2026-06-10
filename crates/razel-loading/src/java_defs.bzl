# razel's java:defs.bzl — `java_library` over the existing engine (B3 spike; RazelStarlarkBoundaryPlan
# Phase B). The stress-test's findings, made concrete:
#  - THREE action kinds per target (Turbine header-jar + Javac + JavaSourceJar), vs cc's two.
#  - The command is a Starlark TEMPLATE (java + JavaBuilder + @args), NOT a Constrain feature config —
#    java is template-shaped (rust-like), so there is no razel_java engine seam (unlike razel_build.command_line, cc's Constrain seam).
#  - The compile classpath is ORDERED (dep.compile_jars, the OrderedDepset fold — B2): a dependent
#    compiles against deps' HEADER jars (compile-avoidance), not their full jars.
# The commands here are representative (structure, not byte-parity); byte-parity vs the golden is the
# B-A0 step (Phase-D-flavored, like cc's A5b — it runs the real @rules_java toolchain).

_CFG = "darwin_arm64-fastbuild"  # output-tree segment; normalize() maps it to <cfg>.
_JAVA = "external/<repo>/bin/java"
_JAVABUILDER = "external/<repo>/java_tools/JavaBuilder_deploy.jar"

def _java_library_impl(ctx):
    pkg = ctx.label.package
    name = ctx.label.name
    bin = "bazel-out/%s/bin" % _CFG
    prefix = "%s/%s" % (bin, pkg) if pkg else bin
    src_prefix = pkg + "/" if pkg else ""

    jar = "%s/lib%s.jar" % (prefix, name)
    hjar = "%s/lib%s-hjar.jar" % (prefix, name)
    srcjar = "%s/lib%s-src.jar" % (prefix, name)
    srcs = [src_prefix + s for s in getattr(ctx.attr, "srcs", [])]

    neverlink = getattr(ctx.attr, "neverlink", False)

    # Two ORDERED, non-cross-merging classpaths (B4): compile = deps' header jars; runtime = deps'
    # full jars (neverlink deps pruned from runtime). Folded independently (OrderedDepset).
    classpath = []
    runtime_cp = []
    for d in getattr(ctx.attr, "deps", []):
        classpath = classpath + d.compile_jars
        runtime_cp = runtime_cp + d.runtime_jars
    classpath = dedup(classpath)  # F1: cross-sibling dedup (a diamond must not list base's jar twice)
    runtime_cp = dedup(runtime_cp)

    # Turbine — the header/interface jar (fast; enables compile-avoidance for dependents).
    razel_build.action(
        executable = _JAVA,
        arguments = ["-jar", "external/<repo>/java_tools/turbine_deploy.jar", "--output", hjar, "--classpath"] + classpath + ["--sources"] + srcs,
        inputs = srcs + classpath,
        outputs = [hjar],
        mnemonic = "Turbine",
    )
    # Javac — the real compile, JavaBuilder run via java (the JVM-invocation command shape).
    razel_build.action(
        executable = _JAVA,
        arguments = [
            "--add-exports=jdk.compiler/com.sun.tools.javac.api=ALL-UNNAMED",
            "-jar",
            _JAVABUILDER,
            "--output",
            jar,
            "--classpath",
        ] + classpath + ["--runtime_classpath"] + runtime_cp + ["--sources"] + srcs,
        inputs = srcs + classpath,
        outputs = [jar],
        mnemonic = "Javac",
    )
    # JavaSourceJar — the source jar.
    razel_build.action(
        executable = _JAVA,
        arguments = ["-jar", "external/<repo>/java_tools/SourceJar_deploy.jar", "--output", srcjar] + srcs,
        inputs = srcs,
        outputs = [srcjar],
        mnemonic = "JavaSourceJar",
    )

    # JavaInfo: compile_jars = OWN header jar (always); runtime_jars = OWN full jar UNLESS neverlink
    # (compile-only → excluded from dependents' runtime). The two depsets don't cross-merge (B4).
    razel_build.info("JavaInfo", {  # C3: the generic provider constructor
        "compile_jars": [hjar],
        "runtime_jars": [] if neverlink else [jar],
        "neverlink": neverlink,
    })
    return [DefaultInfo(files = [jar])]

java_library = rule(implementation = _java_library_impl, attrs = {})

# Loading-grade java rules (TF imports the symbols; razel's java fidelity is library-level).
# Declare-only impls: DefaultInfo, no actions — registered debt (RazelGaps: java breadth).
def _java_stub_impl(ctx):
    return [DefaultInfo(files = [])]

java_test = rule(implementation = _java_stub_impl, attrs = {})
java_binary = rule(implementation = _java_stub_impl, attrs = {})
java_import = rule(implementation = _java_stub_impl, attrs = {})
