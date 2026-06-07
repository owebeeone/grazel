#!/usr/bin/env python3
"""Extract Bazel's command-line flag inventory from a Bazel **source checkout**.

Parses `@Option(...)` annotations across the Bazel `lib` tree into a stable JSON
inventory keyed by flag name. This is the version-pinned, no-build source of truth
razel's CLI parses against; re-running it on a newer Bazel checkout + diffing the
JSON surfaces added/removed/changed flags (the version-comparison oracle).

(When a `bazel`/`bazelisk` binary is available, `bazel help flags-as-proto` is the
more authoritative dump — includes defaults/effect-tags/per-command applicability.
This source parser is the bootstrap that needs no build.)

Usage:
  extract_bazel_flags.py <bazel-src-dir>            # emit JSON inventory to stdout
  extract_bazel_flags.py <bazel-src-dir> --diff old.json   # report flag deltas
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

OPTION_RE = re.compile(r"@Option\(")
NAME_RE = re.compile(r'name\s*=\s*"([^"]+)"')
ABBREV_RE = re.compile(r"abbrev\s*=\s*'(.)'")
MULTI_RE = re.compile(r"allowMultiple\s*=\s*true")
CAT_RE = re.compile(r"documentationCategory\s*=\s*OptionDocumentationCategory\.(\w+)")
OLD_RE = re.compile(r'oldName\s*=\s*"([^"]+)"')
DEPRECATED_RE = re.compile(r"deprecationWarning\s*=")
DEFAULT_RE = re.compile(r'defaultValue\s*=\s*"([^"]*)"')
FIELD_RE = re.compile(r"public\s+(?:final\s+)?([\w<>,.\[\] ]+?)\s+\w+\s*[;=]")


def classify(java_type: str) -> str:
    t = java_type.replace(" ", "")
    if t == "boolean":
        return "bool"
    if t in ("int", "long", "Integer", "Long", "double"):
        return "int"
    if t.startswith(("List<", "ImmutableList<")):
        return "list"
    return "string"  # String, enums, and converter-typed flags parse as one value


def _block(text: str, start: int) -> tuple[str, int]:
    """Return the contents of a parenthesized block beginning at `start` (just after
    the opening `(`), and the index past its closing `)`."""
    depth, j = 1, start
    while j < len(text) and depth:
        c = text[j]
        if c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
        j += 1
    return text[start : j - 1], j


def extract(bazel_src: Path) -> dict[str, dict]:
    lib = bazel_src / "src/main/java/com/google/devtools/build/lib"
    flags: dict[str, dict] = {}
    for jf in sorted(lib.rglob("*.java")):
        text = jf.read_text(errors="ignore")
        for m in OPTION_RE.finditer(text):
            block, end = _block(text, m.end())
            name_m = NAME_RE.search(block)
            if not name_m:
                continue
            name = name_m.group(1)
            abbrev = ABBREV_RE.search(block)
            cat = CAT_RE.search(block)
            old = OLD_RE.search(block)
            default_m = DEFAULT_RE.search(block)
            default = default_m.group(1) if default_m else None
            # Field type (the `public <type> ...;` after the annotation; help strings
            # can be long, so scan a generous window).
            ftype_m = FIELD_RE.search(text[end : end + 1500])
            java_type = ftype_m.group(1).strip() if ftype_m else "?"
            typ = classify(java_type)
            # Robust boolean signal: a boolean field OR a true/false default. (Critical:
            # a bool mustn't consume the next token as its value when parsing.)
            if default in ("true", "false"):
                typ = "bool"
            # Keep the first definition seen (dedupe redefinitions across configs).
            flags.setdefault(
                name,
                {
                    "name": name,
                    "abbrev": abbrev.group(1) if abbrev else None,
                    "type": typ,
                    "has_negation": typ == "bool",  # Bazel auto-adds --noNAME for bools
                    "allow_multiple": bool(MULTI_RE.search(block)),
                    "default": default,
                    "category": cat.group(1) if cat else jf.stem,
                    "old_name": old.group(1) if old else None,
                    "deprecated": bool(DEPRECATED_RE.search(block)),
                    "source": str(jf.relative_to(lib)),
                },
            )
    return flags


def diff(old: dict[str, dict], new: dict[str, dict]) -> int:
    added = sorted(set(new) - set(old))
    removed = sorted(set(old) - set(new))
    changed = sorted(
        n
        for n in set(old) & set(new)
        if {k: old[n].get(k) for k in ("abbrev", "type", "allow_multiple")}
        != {k: new[n].get(k) for k in ("abbrev", "type", "allow_multiple")}
    )
    print(f"# bazel flag delta: +{len(added)} -{len(removed)} ~{len(changed)}")
    for n in added:
        print(f"  + {n}  ({new[n]['type']}{', -' + new[n]['abbrev'] if new[n]['abbrev'] else ''})")
    for n in removed:
        print(f"  - {n}")
    for n in changed:
        print(f"  ~ {n}: {old[n]} -> {new[n]}")
    return 1 if (added or removed or changed) else 0


def main(argv: list[str]) -> int:
    if not argv:
        print(__doc__)
        return 2
    src = Path(argv[0])
    flags = extract(src)
    if "--diff" in argv:
        old = json.loads(Path(argv[argv.index("--diff") + 1]).read_text())
        return diff(old.get("flags", old), flags)
    json.dump(
        {"flags": flags},
        sys.stdout,
        indent=1,
        sort_keys=True,
    )
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
