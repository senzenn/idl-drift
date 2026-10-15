#!/usr/bin/env bash
# Pack the release binaries of one target as idl-drift-<tag>-<target>.tar.gz, with a .sha256
# file. action.yml downloads these names.
# Usage: scripts/package-release.sh <tag> <target> <out dir>
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 <tag> <target> <out dir>" >&2
  exit 2
fi
tag=$1
target=$2
out=$3
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
release="${CARGO_TARGET_DIR:-$root/target}/$target/release"
name="idl-drift-$tag-$target"

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$name" "$out"
cp "$release/idl-drift" "$release/idl-verify" "$root/README.md" "$root/LICENSE-APACHE" \
  "$root/LICENSE-MIT" "$stage/$name/"
# Without this, macOS tar adds AppleDouble `._` files for extended attributes.
COPYFILE_DISABLE=1 tar -czf "$out/$name.tar.gz" -C "$stage" "$name"

if command -v sha256sum >/dev/null; then
  (cd "$out" && sha256sum "$name.tar.gz" >"$name.tar.gz.sha256")
else
  (cd "$out" && shasum -a 256 "$name.tar.gz" >"$name.tar.gz.sha256")
fi
echo "$out/$name.tar.gz"
