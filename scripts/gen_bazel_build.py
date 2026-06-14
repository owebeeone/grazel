#!/usr/bin/env python3
"""Generate per-crate BUILD.bazel for the razel workspace from `cargo metadata`.

SCAFFOLD — run from the workspace root (`python3 scripts/gen_bazel_build.py`) to (re)generate
the per-crate BUILD.bazel. It OVERWRITES them, so after running, re-apply the hand-patches
this scaffold does not emit (see git history / the current BUILD.bazel for the exact form):
  - razel-loading lib: the 14 `//crates/razel-loading/host-repos/...:files` compile_data
    labels (+ those filegroup BUILD.bazel, and the host-repos sub-package shadows).
  - parity-corpus goldens: compile_data on razel-cc-toolchain's unit test (cc) and
    razel-loading's tests (cc + java), plus the parity/corpus/{cc,java} BUILD.bazel shadows.
  - CARGO_BIN_EXE_*: data + rustc_env on the razel-cli integration tests.
  - tags = ["manual"|"local"] on the host-env-coupled tests (node / cc-parity / daemon e2e).
Adding a single new crate is usually easier to hand-write than to regenerate-and-repatch.

Design:
- External (registry) deps come from crate_universe's all_crate_deps()/aliases() — but ONLY
  loaded when the crate actually has external deps (keeps pure-internal crates' BUILD files
  free of the @crates load, closer to razel-readable).
- Internal (workspace path) deps are emitted as plain // labels (work for bazel AND razel).
- lib -> rust_library; [[bin]] -> rust_binary (deps += the lib); unit tests -> rust_test
  (crate=:lib, + dev deps); each tests/*.rs -> a rust_test.
- srcs put lib.rs/the bin root FIRST + explicit crate_root, so razel (srcs[0]) and bazel agree.
"""
import json, os, re, subprocess, sys

ROOT = "/Users/owebeeone/limbo/glial-dev/razel"
meta = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=ROOT))

def us(name):  # crate name -> rust crate / target name
    return name.replace("-", "_")

# crate name -> bazel label of its lib (//crates/foo:foo_us  or  //xtask:xtask)
pkgdir = {}   # name -> dir relative to root (e.g. "crates/razel-core")
for p in meta["packages"]:
    d = os.path.relpath(os.path.dirname(p["manifest_path"]), ROOT)
    pkgdir[p["name"]] = d

def lib_label(name):
    return f"//{pkgdir[name]}:{us(name)}"

def deps_split(pkg, kinds):
    """Return (internal_labels, has_external) for dependency kinds (None=normal, 'dev','build')."""
    internal, has_ext = [], False
    for dep in pkg["dependencies"]:
        if dep.get("kind") not in kinds:
            continue
        if dep.get("path") and dep["name"] in pkgdir:
            internal.append(lib_label(dep["name"]))
        else:
            has_ext = True
    return sorted(set(internal)), has_ext

SRCS_LIB = '["src/lib.rs"] + glob(["src/**/*.rs"], exclude = {excl}, allow_empty = True)'
# Non-source files in the crate that include_str!/include_bytes! may pull in at compile time
# (fixtures/, host-repos/, src/*.bzl, test data, …). Empty for crates with none.
CD = '    compile_data = glob(["**/*"], exclude = ["**/*.rs", "BUILD.bazel", "Cargo.toml"], allow_empty = True),'

def gen(pkg):
    d = pkgdir[pkg["name"]]
    targets = pkg["targets"]
    lib = next((t for t in targets if "lib" in t["kind"] or "rlib" in t["kind"]), None)
    bins = [t for t in targets if "bin" in t["kind"]]
    tests = [t for t in targets if t["kind"] == ["test"]]

    # bin src paths relative to the crate dir (excluded from the lib glob).
    bin_srcs = [os.path.relpath(t["src_path"], os.path.join(ROOT, d)) for t in bins]

    norm_int, norm_ext = deps_split(pkg, {None})
    dev_int, dev_ext = deps_split(pkg, {None, "dev"})  # tests see normal+dev
    any_ext = norm_ext or dev_ext or deps_split(pkg, {"build"})[1]

    lines, loads = [], ['load("@rules_rust//rust:defs.bzl", "rust_library", "rust_binary", "rust_test")']
    if any_ext:
        loads.append('load("@crates//:defs.bzl", "aliases", "all_crate_deps")')
    lines.append('exports_files(["Cargo.toml"])  # crate_universe reads this')
    lines.append("")

    def acd(dev=False, pm=False):
        if not any_ext:
            return []
        kind = ("proc_macro" if pm else "normal") + ("_dev" if dev else "")
        return [f"all_crate_deps({kind} = True)"]

    def dep_expr(internal, dev=False):
        parts = []
        if any_ext:
            parts.append(f'all_crate_deps(normal{"_dev" if dev else ""} = True)')
        if internal:
            parts.append("[\n        " + ",\n        ".join(f'"{x}"' for x in internal) + ",\n    ]")
        return " + ".join(parts) if parts else "[]"

    aliases_line = "    aliases = aliases(),\n" if any_ext else ""
    pmd = '    proc_macro_deps = all_crate_deps(proc_macro = True),\n' if any_ext else ""

    if lib:
        excl = '["src/lib.rs"]' + ("".join(f' + ["{b}"]' for b in bin_srcs) if bin_srcs else "")
        lines += [
            "rust_library(",
            f'    name = "{us(pkg["name"])}",',
            f'    srcs = {SRCS_LIB.format(excl=excl)},',
            '    crate_root = "src/lib.rs",',
            '    edition = "2024",',
            CD,
            aliases_line.rstrip("\n") if aliases_line else None,
            f"    deps = {dep_expr(norm_int)},",
            pmd.rstrip("\n") if pmd else None,
            '    visibility = ["//visibility:public"],',
            ")", "",
        ]
        lines = [l for l in lines if l is not None]

    for t in bins:
        rel = os.path.relpath(t["src_path"], os.path.join(ROOT, d))
        bin_int = sorted(set(norm_int + ([lib_label(pkg["name"])] if lib else [])))
        lines += [
            "rust_binary(",
            f'    name = "{t["name"]}",',
            f'    srcs = ["{rel}"] + glob(["src/**/*.rs"], exclude = ["{rel}"], allow_empty = True),' if not lib else f'    srcs = ["{rel}"],',
            f'    crate_root = "{rel}",',
            '    edition = "2024",',
            CD,
            aliases_line.rstrip("\n") if aliases_line else None,
            f"    deps = {dep_expr(bin_int)},",
            pmd.rstrip("\n") if pmd else None,
            '    visibility = ["//visibility:public"],',
            ")", "",
        ]
        lines = [l for l in lines if l is not None]

    # Unit tests (compile the lib with --test). Only if the lib has #[cfg(test)]/#[test].
    if lib:
        srcdir = os.path.join(ROOT, d, "src")
        has_unit = any("#[test]" in open(os.path.join(D, f)).read() or "#[cfg(test)]" in open(os.path.join(D, f)).read()
                       for D, _, fs in os.walk(srcdir) for f in fs if f.endswith(".rs"))
        if has_unit:
            lines += [
                "rust_test(",
                f'    name = "{us(pkg["name"])}_unit",',
                f'    crate = ":{us(pkg["name"])}",',
                '    edition = "2024",',
            CD,
                (f"    deps = {dep_expr(sorted(set(dev_int)-set(norm_int)), dev=True)}," if (dev_ext or set(dev_int)-set(norm_int)) else None),
                ")", "",
            ]
            lines = [l for l in lines if l is not None]

    for t in tests:
        rel = os.path.relpath(t["src_path"], os.path.join(ROOT, d))
        test_int = sorted(set(dev_int + ([lib_label(pkg["name"])] if lib else [])))
        lines += [
            "rust_test(",
            f'    name = "{t["name"]}_test",',
            f'    srcs = ["{rel}"],',
            f'    crate_root = "{rel}",',
            '    edition = "2024",',
            CD,
            aliases_line.rstrip("\n") if aliases_line else None,
            f"    deps = {dep_expr(test_int, dev=True)},",
            ('    proc_macro_deps = all_crate_deps(proc_macro = True, proc_macro_dev = True),' if any_ext else None),
            ")", "",
        ]
        lines = [l for l in lines if l is not None]

    return "\n".join("\n".join(loads) + "\n\n" + "\n".join(lines).rstrip() + "\n" for _ in [0])

count = 0
for pkg in meta["packages"]:
    out = os.path.join(ROOT, pkgdir[pkg["name"]], "BUILD.bazel")
    open(out, "w").write(gen(pkg))
    count += 1
print(f"generated {count} BUILD.bazel files")
