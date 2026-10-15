# Releasing cleanrs

Releases run through the dispatch-only [Release workflow](../.github/workflows/release.yml).
Do not bump versions or create release tags by hand. One workflow owns the bump,
tag, builds, GitHub Release and Homebrew update. Tags pushed with `GITHUB_TOKEN`
do not start other workflows; the release does not depend on that behavior.

## One-time prerequisites

- Make `bencetotht/cleanrs` public before the first published release. It is
  currently private; package URLs and anonymous Nix/source installs assume public access.
- Create the public repository `bencetotht/homebrew-tap`, with an initial commit,
  a default branch and a `Formula/` directory (Git needs a file to retain an empty
  directory, such as `Formula/.gitkeep`). This repository does not yet exist.
- Allow GitHub Actions to write repository contents and push the release commit
  to `main`. If branch protection/rulesets are added, give the release actor a
  suitable bypass or adapt authorization before running; a token alone does not
  bypass branch protection. Enable Actions on the repository.
- Configure `HOMEBREW_TAP_TOKEN`: a fine-grained PAT limited to
  `bencetotht/homebrew-tap`, with **Contents: read and write**. Store it as an
  Actions secret on `cleanrs`; do not grant unrelated repository access.
- Configure Apple secrets below for signed/notarized macOS assets, or accept
  an explicit unsigned-build warning. Once a certificate is supplied, incomplete
  signing credentials or a non-Accepted notarization result fail the build.

## Version conventions

[scripts/bump-version.sh](../scripts/bump-version.sh) reads the workspace package
version from root `Cargo.toml`. Versions currently start at **0.1.0**.

| Input / commit since last release | Bump |
|---|---|
| No reachable `vX.Y.Z` release tag | Seed the existing Cargo version unchanged, even with an explicit mode |
| `feat: …` or `feat(scope): …` | Minor |
| `type!: …`, `type(scope)!: …`, or `BREAKING CHANGE:` in the body/footer | Major; while 0.x, auto converts this to minor |
| Other commits, e.g. `fix: …`, `docs: …` | Patch |
| Explicit `patch`, `minor`, `major` input | Requested bump; explicit `major` permits 1.0.0 |

The highest applicable auto bump wins. Subjects starting `chore(release):` and
their bodies are ignored for classification. No commits since the last tag is
an error. Release-only commits do not raise severity; if present they fall back
to patch. The last tag is selected with `git describe` and the `v[0-9]*.[0-9]*.[0-9]*`
pattern, so maintain ordinary stable semver tags in reachable history.

The script only edits `[workspace.package].version`, refreshes workspace lockfile
entries offline first, and verifies both member versions. It prints `version=…`
and `tag=…`, also appending to `GITHUB_OUTPUT` when set. Its `--dry-run` writes no
project files. Releases require no flake version edit: it reads Cargo metadata.

## Nix input maintenance

The flake tracks `github:NixOS/nixpkgs/nixos-unstable` and exposes
`x86_64-linux`, `aarch64-linux`, and `aarch64-darwin`. Intel macOS is
deliberately not a flake system: current unstable refuses `x86_64-darwin`, and
pinning an older nixpkgs only to keep it would freeze the toolchain. Intel Mac
users install through Homebrew or the prebuilt tarball. Update the input with
`nix flake update`, then run `nix flake check --all-systems --no-build -L` in
addition to the Linux build/check. That evaluates the other platforms; it does
not prove their binaries build or run.

## Run a release

1. Merge the intended release tree to `main` and check Linux CI.
2. Open **Actions → Release → Run workflow**. Choose `main`, a `bump` input
   (`auto` normally), and **`dry_run = true` first**.
3. Inspect artifacts and every build result. The dry run computes and applies
   the bump locally in its job, uploads the two manifests, and overlays them on
   the checked-out HEAD in every builder. Thus binaries report the computed
   version, with no commit/tag/remote publication. Optional signing/notarization
   still runs when credentials are supplied. Non-main dry runs are allowed.
4. Dispatch again from `main` with `dry_run = false` to publish. The version may
   differ if commits or tags changed since the dry run.

Concurrency serializes releases without cancelling an earlier run. macOS runners
are reserved for the release pipeline; push/PR CI stays Linux-only. The separate
manual benchmark workflow remains available and is not part of push/PR CI.

| Job | Work |
|---|---|
| `version` | Full history checkout, clean-tree guard, bump, optional bump commit, annotated tag, push main then tag; expose exact build SHA |
| `build-macos` | One macos-15 runner builds ARM64 and Intel; optional signing/notarization; native fixture smoke checks; package both using native generators |
| `build-linux` | Native x86_64/ARM64 musl builds on Ubuntu, fixture smoke checks, packages |
| `nix` | Build the flake at the release SHA and assert its version |
| `publish` | Require every build, verify four tarballs/checksums, combine `SHA256SUMS`, create the GitHub Release with generated notes |
| `homebrew` | Render four architecture checksums into the binary formula and push to the tap |

Smoke checks use temporary trees and dry-run cleanup, never `--apply` or the real
home directory. The x86_64 macOS archive uses the ARM64 binary to generate
completions/man pages. Signing precedes tarball checksums. Dry runs upload artifacts
and skip commit, push, tag, Release creation and tap updates. The GitHub Release
is published before the formula is pushed so its download URLs already resolve.

## Apple signing secrets

Use a Developer ID Application certificate with its private key, and an App
Store Connect API key authorized for notarization. Export the certificate and
private key from Keychain Access as a password-protected `.p12`. Download the
API private key as `.p8` when creating it; record its key ID and issuer ID.
Base64 encode files locally, e.g. `base64 < certificate.p12 | tr -d '\n'` and
`base64 < AuthKey_KEYID.p8 | tr -d '\n'`, then copy the results into secrets.

| Secret | Value |
|---|---|
| `APPLE_CERTIFICATE_P12_BASE64` | Base64 of the exported Developer ID Application `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | Export password for that file |
| `APPLE_SIGNING_IDENTITY` | Exact identity, e.g. `Developer ID Application: Name (TEAMID)` |
| `APPLE_API_KEY_P8_BASE64` | Base64 of the App Store Connect `.p8` private key |
| `APPLE_API_KEY_ID` | API key ID |
| `APPLE_API_ISSUER_ID` | API issuer ID |

The job imports the certificate into a temporary unlocked keychain, sets the
signing partition list, codesigns both binaries with timestamp and hardened
runtime, verifies signatures, submits ZIPs with `notarytool --wait`, and requires
`Accepted`. An always-run cleanup deletes the keychain, certificate and `.p8`.
Bare binaries cannot be stapled; Gatekeeper retrieves tickets online. See
[Apple's workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow).
Treat certificate export files and API keys as credentials; keep them outside
the repository. Base64 is encoding, not encryption.

## Failure recovery

If a build fails after tagging, use **Re-run failed jobs** on the existing run.
That retains the version job outputs and exact release SHA. Dispatching a fresh
release instead is a new versioning decision and may produce a no-new-commits error
or a new patch version. If pushing main succeeded but pushing the tag failed,
inspect both remote states before retrying; the workflow fails instead of
silently overwriting tags.

If the tap job alone fails, the GitHub Release is already public. Fix the tap
repository/token and re-run that job (or failed jobs) in the same run. Rendering
uses the original artifacts, and a formula already matching the desired bytes
needs no new commit. Do not rerun successful publication just to repair the tap.

To abandon a partially published release, a maintainer can deliberately delete
the release and its remote tag and revert the bump commit, then dispatch again.
Check downloads and any tap update first: replacing an already consumed version
is disruptive. These are manual recovery operations, not local verification steps.
If a release already exists and only the tap failed, prefer completing that release.

## Local verification

```sh
scripts/test-bump-version.sh
scripts/bump-version.sh --dry-run
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --release -p cleanrs
out=$(mktemp -d)
scripts/package-release.sh x86_64-unknown-linux-gnu target/release/cleanrs 0.1.0 "$out"
tar -tzf "$out/cleanrs-0.1.0-x86_64-unknown-linux-gnu.tar.gz"
nix build .#default -L
./result/bin/cleanrs --version
nix flake check -L
nix flake check --all-systems --no-build -L
nix run nixpkgs#actionlint -- .github/workflows/*.yml
```

Adjust the local target/version to match your build. Native macOS, ARM Linux,
musl execution, signing/notarization and actual publication need release-runner
validation; Linux fixture tests cannot establish those outcomes. The first real
release, public visibility, tap setup and secrets remain maintainer prerequisites.
