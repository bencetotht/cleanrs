#!/usr/bin/env bash
# All commits and tags below are confined to disposable fixture repositories.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
fixture=$(mktemp -d); trap 'rm -rf "$fixture"' EXIT
mkdir -p "$fixture/scripts" "$fixture/cleanrs/src" "$fixture/cleanrs-core/src"
cp "$root/scripts/bump-version.sh" "$fixture/scripts/"
cd "$fixture"
cat > Cargo.toml <<'TOML'
[workspace]
members = ["cleanrs", "cleanrs-core"]
resolver = "2"
[workspace.package]
version = "0.1.0"
TOML
for member in cleanrs cleanrs-core; do
  printf '[package]\nname = "%s"\nversion.workspace = true\nedition = "2021"\n' "$member" > "$member/Cargo.toml"
  touch "$member/src/lib.rs"
done
cargo generate-lockfile --offline
git init -q; git config user.name Fixture; git config user.email fixture@example.invalid
# Local fixtures must not inherit interactive signing requirements.
git config commit.gpgsign false; git config tag.gpgsign false
git add .; git commit -qm 'chore: fixture'
seed=$(git rev-parse HEAD)
check() {
  expected=$1; shift
  output=$(scripts/bump-version.sh "$@")
  printf '%s\n' "$output" | grep -qx "version=$expected"
  echo "PASS: $* -> $expected"
}
check 0.1.0
check 0.1.0 major --dry-run
git diff --exit-code
git tag v0.1.0
if scripts/bump-version.sh > /dev/null 2> error; then echo 'Expected no-commits error' >&2; exit 1; fi
grep 'No commits since' error; rm error
for scenario in fix feat breaking scoped footer release; do
  git reset --hard -q "$seed"
  case "$scenario" in
    fix) message='fix: fixture'; expected=0.1.1 ;;
    feat) message='feat: fixture'; expected=0.2.0 ;;
    breaking) message='feat!: fixture'; expected=0.2.0 ;;
    scoped) message='fix(api)!: fixture'; expected=0.2.0 ;;
    footer) message=$(printf 'fix: fixture\n\nBREAKING CHANGE: fixture'); expected=0.2.0 ;;
    release) message=$(printf 'chore(release): v0.1.0\n\nBREAKING CHANGE: ignored release body\nfeat: ignored'); expected=0.1.1 ;;
  esac
  git commit --allow-empty -qm "$message"
  check "$expected" auto --dry-run
  git diff --exit-code
  check "$expected" auto
  # The fixture has no dependencies; both member versions must be refreshed.
  [ "$(awk '/^version = "/{print $3}' Cargo.lock | sort -u)" = "\"$expected\"" ]
done
git reset --hard -q "$seed"
git commit --allow-empty -qm 'fix: explicit major'
check 1.0.0 major
if scripts/bump-version.sh nonsense > /dev/null 2>&1; then exit 1; fi
echo 'PASS: invalid mode rejected'
