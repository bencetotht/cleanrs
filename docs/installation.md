# Installation

cleanrs distributes GitHub Release tarballs, a Homebrew tap, and a Nix flake for
macOS and Linux. These public channels require the first release to have been
published. There is no crates.io package or Windows distribution.

## Homebrew

```sh
brew install bencetotht/tap/cleanrs
cleanrs --version
```

The binary formula selects Apple Silicon/Intel macOS or aarch64/x86_64 Linux.
It installs `cleanrs`, `cleanrs(1)`, and bash/zsh/fish completions from the release
archive. Ensure your shell loads Homebrew's completion directories in the usual
way. Use `brew upgrade cleanrs` to upgrade and `brew uninstall cleanrs` to remove
it. `brew untap bencetotht/tap` removes the tap if you no longer need it.

## Nix

Enable the `nix-command` and `flakes` experimental features in Nix, then:

```sh
nix run github:bencetotht/cleanrs -- --help
nix profile install github:bencetotht/cleanrs
```

The flake supports `x86_64-linux`, `aarch64-linux`, and `aarch64-darwin`.
Intel Macs are not a flake system because current nixpkgs no longer supports
`x86_64-darwin`; use Homebrew or the prebuilt tarball there. See
[input maintenance](releasing.md#nix-input-maintenance).
`packages.default` and `packages.cleanrs` build from the locked
source dependencies and run workspace tests. Native builds install the binary,
man page and shell completions. Cross builds omit generated shell/man files
when the build platform cannot execute the resulting binary.

Use `nix profile list` to identify the installed entry, then
`nix profile upgrade <entry-name>` or `nix profile remove <entry-name>`.
`nix run` alone does not add a profile entry. In a checkout, `nix develop` provides
Cargo, rustc, Clippy, rustfmt, rust-analyzer and Git; `nix build .#default -L`
creates `result`. Flakes only see tracked files in a Git checkout; maintainers
can use intent-to-add for new files during development.

## Prebuilt tarballs

Download the matching archive and its `.sha256` file from
[GitHub Releases](https://github.com/bencetotht/cleanrs/releases):

| Machine | Target suffix |
|---|---|
| Apple Silicon Mac | `aarch64-apple-darwin` |
| Intel Mac | `x86_64-apple-darwin` |
| x86_64 Linux | `x86_64-unknown-linux-musl` |
| ARM64 Linux | `aarch64-unknown-linux-musl` |

For example, after downloading the version and target you chose:

```sh
# Linux:
sha256sum -c cleanrs-0.1.0-x86_64-unknown-linux-musl.tar.gz.sha256
# macOS (use the matching macOS filename):
shasum -a 256 -c cleanrs-0.1.0-aarch64-apple-darwin.tar.gz.sha256
```

Extract the archive with `tar -xzf <archive>`. Its single top-level directory
contains `cleanrs`, `README.md`, `LICENSE`, `man/cleanrs.1`, and
`completions/{cleanrs.bash,_cleanrs,cleanrs.fish}`. Copy the binary to a directory
on `PATH`, for example `~/.local/bin`, and the manual page to
`~/.local/share/man/man1`. Install the completion files as described below.

To upgrade, verify a new archive and replace those same files. To uninstall,
remove the files you installed. There is no background agent to stop.

## From source

Rust 1.85 or newer is required:

```sh
cargo install --locked --git https://github.com/bencetotht/cleanrs cleanrs
```

This installs only the binary under Cargo's bin directory (usually `~/.cargo/bin`).
Generate shell/man files with `cleanrs completions <shell>` and `cleanrs man`.
Run the same install command with `--force` to replace an existing installation;
use `cargo uninstall cleanrs` to remove it.

## Shell completions and manual pages

For a tarball, use its generated files. For a source install, generate them:

```sh
mkdir -p ~/.local/share/bash-completion/completions
cleanrs completions bash > ~/.local/share/bash-completion/completions/cleanrs
mkdir -p ~/.local/share/zsh/site-functions
cleanrs completions zsh > ~/.local/share/zsh/site-functions/_cleanrs
mkdir -p ~/.config/fish/completions
cleanrs completions fish > ~/.config/fish/completions/cleanrs.fish
mkdir -p ~/.local/share/man/man1
cleanrs man > ~/.local/share/man/man1/cleanrs.1
```

Bash needs the `bash-completion` framework, or explicitly source the generated
file in `.bashrc`. For zsh, add
`fpath=(~/.local/share/zsh/site-functions $fpath)` before `autoload -Uz compinit;
compinit` in `.zshrc`. Fish automatically loads its user completion directory.
Ensure your manual search path includes `~/.local/share/man` if it is not already
recognized. Nix profiles expose shared files through the profile; shells still
need their completion framework configured.

## macOS permissions and Gatekeeper

For TCC-protected directories, grant **the terminal application** Full Disk Access
in System Settings → Privacy & Security → Full Disk Access, then restart the
terminal if necessary. Denied entries remain scan coverage errors rather than
zero-size data. `doctor --scan PATH` carries explicit-scan coverage into its
report; plain `doctor` supplies access guidance but does not exhaustively probe
all protected directories. Full Disk Access improves visibility, not deletion
policy: protected/cloud candidates still face the same safety checks.

This tap is a Homebrew **formula**: its curl download normally does not acquire
a quarantine attribute. Homebrew's separate Cask quarantine policy does not apply
to this formula. See [Homebrew's security documentation](https://docs.brew.sh/Homebrew-Security-and-Supply-Chain).
Browser-downloaded tarballs commonly carry `com.apple.quarantine`, which can
propagate to extracted binaries.

When Apple signing secrets are configured, release binaries are Developer ID
signed and notarized before packaging. Gatekeeper can obtain their notarization
tickets online; bare Mach-O binaries cannot have a ticket stapled to them.
See [Apple's notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow).
**Correction:** signing is optional in the workflow, so a release without those
secrets is unsigned; not every downloadable build is guaranteed to be notarized.
Check the release's build information when this matters.

For an unsigned build whose origin and checksum you have verified, the local
fallback is:

```sh
xattr -d com.apple.quarantine ./cleanrs
```

Run it on the extracted binary. It removes that file's quarantine attribute;
it does not sign or notarize the executable.

## Removing user state

Package uninstall commands leave user configuration/history intact. To remove
those too, delete the specific files/directories you no longer want:

- `~/.config/cleanrs/targets.toml` and `~/.config/cleanrs/trusted-configs.json`.
- `~/.local/state/cleanrs/` (or `$XDG_STATE_HOME/cleanrs/`): history and rotation.
- `~/.cache/cleanrs/tui-v1/` (or absolute `$XDG_CACHE_HOME/cleanrs/tui-v1/`): cache.

The config/trust paths ignore `XDG_CONFIG_HOME`. See [architecture](architecture.md)
for resolver details. Uninstalling cleanrs never removes your developer caches.
