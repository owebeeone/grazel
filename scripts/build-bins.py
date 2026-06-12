#!/usr/bin/env python3
"""build-bins.py — produce the two distribution binaries: razel and grazel.

    python3 scripts/build-bins.py [--debug] [--verify] [--out DIR]

Cross-platform (Windows/Linux/macOS). Release by default (workspace profile:
fat LTO, stripped, panic=abort — the §13.2 small-single-binary posture).
Binaries land in DIR (default <repo>/dist/bin), each reported with size +
sha256 (digest-logging culture).

  --debug    dev profile instead of release (fast iteration)
  --verify   run the binaries' self-checks after building: version smoke for
             both, plus the full `grazel ws test` ladder (needs host cc + node;
             on Windows the daemon transport is the loopback-TCP fallback)
  --out DIR  copy destination (default: <repo>/dist/bin)
"""

import argparse
import hashlib
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
EXE = ".exe" if sys.platform == "win32" else ""
BINS = ["razel", "grazel"]


def run(cmd: list, **kw) -> None:
    print(f"$ {' '.join(str(c) for c in cmd)}", flush=True)
    subprocess.run(cmd, check=True, **kw)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--debug", action="store_true", help="dev profile (fast)")
    ap.add_argument("--verify", action="store_true", help="version smoke + ws-test ladder")
    ap.add_argument("--out", type=Path, default=REPO / "dist" / "bin")
    args = ap.parse_args()

    profile = "dev" if args.debug else "release"
    target_dir = REPO / "target" / ("debug" if args.debug else "release")

    print(f"== building razel + grazel ({profile}) ==")
    run([
        "cargo", "build", "--manifest-path", REPO / "Cargo.toml",
        "--profile", profile, "-p", "razel-cli", "-p", "grazel-cli",
    ])

    args.out.mkdir(parents=True, exist_ok=True)
    print("== artifacts ==")
    for bin_name in BINS:
        src = target_dir / f"{bin_name}{EXE}"
        dst = args.out / f"{bin_name}{EXE}"
        # Unlink first: overwriting an executable in place invalidates its
        # ad-hoc code signature on macOS (the kernel SIGKILLs it on exec).
        dst.unlink(missing_ok=True)
        shutil.copy2(src, dst)
        digest = hashlib.sha256(dst.read_bytes()).hexdigest()
        print(f"{bin_name}  {dst.stat().st_size} bytes  sha256={digest}  {dst}")

    if args.verify:
        print("== verify: version smoke ==")
        for bin_name in BINS:
            run([args.out / f"{bin_name}{EXE}", "version"])
        print("== verify: grazel ws test (full ladder) ==")
        # The ladder's build-parity stage wants a sibling razel binary, and the
        # js-client stage finds clients/grazel-js by walking up from the binary
        # — both hold for the TARGET-DIR binary (same bytes, dev-tree layout),
        # not necessarily for the copies in --out.
        run([target_dir / f"grazel{EXE}", "ws", "test"])

    print("done.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
