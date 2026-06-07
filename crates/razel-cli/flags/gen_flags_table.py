#!/usr/bin/env python3
"""Generate the Rust Bazel-flag recognition table from the JSON inventory.

`bazel-flags-<ver>.json` → `../src/bazel_flags.rs` (a `BAZEL_FLAGS` table the CLI
parser uses to recognize every Bazel flag, know whether it takes a value, and
resolve abbreviations). Run via `cargo xtask flags` (which rustfmts + drift-checks
the output), or directly:  python3 gen_flags_table.py bazel-flags-9.1.1.json --stdout
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

# razel policy: flags for these languages/rulesets are ones razel will never
# implement — recognize them but stay silent (no "unsupported" diagnostic).
# Reapplied every Bazel version, matched on either the source path OR the flag name
# (many lang flags are registered centrally in BazelRulesModule, not under a lang dir).
SILENT_LANGS = ("java", "python", "android", "objc", "apple", "swift", "j2objc", "dotnet")
SILENT_LANG_RE = re.compile(r"(?i)(^|/)(" + "|".join(SILENT_LANGS) + ")")


def is_silent(flag: dict) -> bool:
    return bool(SILENT_LANG_RE.search(flag.get("source", ""))) or flag["name"].startswith(
        SILENT_LANGS
    )

HERE = Path(__file__).resolve().parent
DEFAULT_JSON = HERE / "bazel-flags-9.1.1.json"
OUT = HERE.parent / "src" / "bazel_flags.rs"


def emit(inventory: dict) -> str:
    flags = inventory["flags"]
    lines = [
        "// GENERATED from flags/bazel-flags-*.json — do not edit. `cargo xtask flags`.",
        "//! Bazel's full command-line flag inventory, so razel parses real Bazel",
        "//! command lines: recognized flags are typed (value-taking vs boolean) and",
        "//! abbreviations resolved; unsupported ones are diagnosed, not rejected.",
        "",
        "/// One Bazel flag's parse-relevant shape.",
        "pub struct FlagSpec {",
        "    pub name: &'static str,",
        "    pub abbrev: Option<char>,",
        "    /// false = boolean (`--name`/`--noname`); true = takes a value.",
        "    pub takes_value: bool,",
        "    /// Bazel's repeatable flags accumulate; informational (handlers decide).",
        "    #[allow(dead_code)]",
        "    pub allow_multiple: bool,",
        "    /// razel will never implement this (language-specific) — recognize it but",
        "    /// stay silent, no `unsupported` diagnostic.",
        "    pub silent: bool,",
        "}",
        "",
        "pub static BAZEL_FLAGS: &[FlagSpec] = &[",
    ]
    for name in sorted(flags):
        f = flags[name]
        ab = f"Some('{f['abbrev']}')" if f.get("abbrev") else "None"
        takes = "false" if f["type"] == "bool" else "true"
        multi = "true" if f.get("allow_multiple") else "false"
        silent = "true" if is_silent(f) else "false"
        lines.append(
            f'    FlagSpec {{ name: "{name}", abbrev: {ab}, '
            f"takes_value: {takes}, allow_multiple: {multi}, silent: {silent} }},"
        )
    lines.append("];")
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    src = Path(argv[0]) if argv and not argv[0].startswith("--") else DEFAULT_JSON
    out = emit(json.loads(src.read_text()))
    if "--stdout" in argv:
        sys.stdout.write(out)
    else:
        OUT.write_text(out)
        print(f"wrote {OUT} ({out.count('FlagSpec {')} flags)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
