#!/usr/bin/env bash
# One workspace version; the first release seeds the version already in Cargo.toml.
set -euo pipefail
cd "$(dirname "$0")/.."
mode=auto; dry_run=false; mode_seen=false
for arg in "$@"; do
  case "$arg" in
    --dry-run) dry_run=true ;;
    auto|patch|minor|major)
      if "$mode_seen"; then echo 'Specify only one bump mode' >&2; exit 1; fi
      mode=$arg; mode_seen=true ;;
    *) echo 'Usage: scripts/bump-version.sh [auto|patch|minor|major] [--dry-run]' >&2; exit 1 ;;
  esac
done
current=$(awk '/^\[workspace.package\]/{inside=1;next} /^\[/{inside=0}
  inside && /^version = /{gsub(/"/, "", $3);print $3}' Cargo.toml)
if ! [[ "$current" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Invalid workspace version: $current" >&2; exit 1
fi
last=$(git describe --tags --abbrev=0 --match 'v[0-9]*.[0-9]*.[0-9]*' 2>/dev/null || true)
next=$current
if [ -n "$last" ]; then
  if [ "$(git rev-list --count "$last..HEAD")" -eq 0 ]; then
    echo "No commits since $last; nothing to release" >&2; exit 1
  fi
  bump=$mode
  if [ "$mode" = auto ]; then
    # Record separators let us ignore release commits together with their bodies.
    commits=$(git log --format='%s%n%b%x1e' "$last..HEAD" | awk '
      BEGIN {RS="\036"} {sub(/^\n+/, ""); if ($0 !~ /^chore\(release\):/) print}')
    bump="patch"
    if printf '%s\n' "$commits" | grep -Eq '^feat(\([^)]*\))?(!)?:'; then bump=minor; fi
    if printf '%s\n' "$commits" | grep -Eq '^[a-zA-Z][a-zA-Z0-9_-]*(\([^)]*\))?!:|^BREAKING CHANGE:'; then bump=major; fi
  fi
  IFS=. read -r major minor patch <<< "$current"
  # Only inferred breaking changes obey the pre-1.0 rule; explicit major means 1.0.
  if [ "$mode" = auto ] && [ "$major" -eq 0 ] && [ "$bump" = major ]; then bump=minor; fi
  case "$bump" in
    major) next="$((major + 1)).0.0" ;;
    minor) next="$major.$((minor + 1)).0" ;;
    patch) next="$major.$minor.$((patch + 1))" ;;
  esac
else
  echo "No release tag: seed $current" >&2
fi
if "$dry_run"; then
  echo "Would release $next (current $current); no files changed" >&2
elif [ "$next" != "$current" ]; then
  # Target the workspace package section, never dependency version strings.
  tmp=$(mktemp); trap 'rm -f "$tmp"' EXIT
  awk -v version="$next" '/^\[workspace.package\]/{inside=1} /^\[/ && !/^\[workspace.package\]/{inside=0}
    inside && /^version = /{$0="version = \"" version "\""} {print}' Cargo.toml > "$tmp"
  cat "$tmp" > Cargo.toml
  cargo update --workspace --offline || cargo update --workspace
  for member in cleanrs cleanrs-core; do
    awk -v name="$member" -v version="$next" '
      /^\[\[package\]\]/{found=0}
      $0 == "name = \"" name "\"" {found=1}
      found && $0 == "version = \"" version "\"" {ok=1}
      END {exit !ok}' Cargo.lock || { echo "Lockfile version mismatch for $member" >&2; exit 1; }
  done
fi
printf 'version=%s\ntag=v%s\n' "$next" "$next"
if [ -n "${GITHUB_OUTPUT:-}" ]; then printf 'version=%s\ntag=v%s\n' "$next" "$next" >> "$GITHUB_OUTPUT"; fi
