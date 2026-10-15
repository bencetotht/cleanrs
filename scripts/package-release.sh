#!/usr/bin/env bash
# Build a self-contained archive; cross builds use a native binary for generators.
set -euo pipefail
if [ "$#" -lt 4 ] || [ "$#" -gt 5 ]; then
  echo 'Usage: scripts/package-release.sh <target> <binary> <version> <out-dir> [<native-generator>]' >&2
  exit 1
fi
root=$(cd "$(dirname "$0")/.." && pwd)
target=$1; binary=$2; version=$3; out=$4; generator=${5:-$2}
[[ "$target" =~ ^[a-zA-Z0-9_-]+$ ]] && [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo 'Invalid target or version' >&2; exit 1;
}
# Resolve relative executable paths before changing directory for checksums.
case "$generator" in /*) ;; *) generator="$PWD/$generator" ;; esac
stage=$(mktemp -d); trap 'rm -rf "$stage"' EXIT
name="cleanrs-$version-$target"
mkdir -p "$stage/$name/completions" "$stage/$name/man" "$out"
cp "$binary" "$stage/$name/cleanrs"
chmod 755 "$stage/$name/cleanrs"
cp "$root/README.md" "$root/LICENSE" "$stage/$name/"
"$generator" completions bash > "$stage/$name/completions/cleanrs.bash"
"$generator" completions zsh > "$stage/$name/completions/_cleanrs"
"$generator" completions fish > "$stage/$name/completions/cleanrs.fish"
"$generator" man > "$stage/$name/man/cleanrs.1"
tar -czf "$out/$name.tar.gz" -C "$stage" "$name"
# Store only the archive basename so checksum files work after download.
cd "$out"
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "$name.tar.gz" > "$name.tar.gz.sha256"
else
  shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256"
fi
printf '%s\n' "$name.tar.gz"
