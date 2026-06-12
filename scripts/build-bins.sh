#!/usr/bin/env bash
# build-bins.sh — produce the two distribution binaries: razel and grazel.
#
#   scripts/build-bins.sh [--debug] [--verify] [--out DIR]
#
# Release by default (workspace profile: fat LTO, stripped, panic=abort — the
# §13.2 small-single-binary posture). Binaries land in DIR (default dist/bin),
# each reported with size + sha256 (digest-logging culture).
#
#   --debug    dev profile instead of release (fast iteration)
#   --verify   run the binaries' self-checks after building: version smoke for
#              both, plus the full `grazel ws test` ladder (needs host cc+node)
#   --out DIR  copy destination (default: <repo>/dist/bin)
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
profile=release
verify=0
out="$repo/dist/bin"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --debug)  profile=dev; shift ;;
    --verify) verify=1; shift ;;
    --out)    out="$2"; shift 2 ;;
    *) echo "usage: $0 [--debug] [--verify] [--out DIR]" >&2; exit 64 ;;
  esac
done

target_dir="$repo/target/$([[ $profile == dev ]] && echo debug || echo release)"

echo "== building razel + grazel ($profile) =="
cargo build --manifest-path "$repo/Cargo.toml" --profile "$profile" \
  -p razel-cli -p grazel-cli

mkdir -p "$out"
for bin in razel grazel; do
  cp "$target_dir/$bin" "$out/$bin"
done

echo "== artifacts =="
for bin in razel grazel; do
  path="$out/$bin"
  size=$(wc -c < "$path" | tr -d ' ')
  sha=$(shasum -a 256 "$path" | cut -d' ' -f1)
  echo "$bin  $size bytes  sha256=$sha  $path"
done

if [[ $verify == 1 ]]; then
  echo "== verify: version smoke =="
  "$out/razel" version
  "$out/grazel" version
  echo "== verify: grazel ws test (full ladder) =="
  # The ladder's build-parity stage wants a sibling razel binary, and the
  # js-client stage finds clients/grazel-js by walking up from the binary —
  # both hold for the copies in $out only if $out is inside the repo, so run
  # the TARGET-DIR binary (same bytes, guaranteed dev-tree layout).
  "$target_dir/grazel" ws test
fi

echo "done."
