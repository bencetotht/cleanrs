# cleanrs

cleanrs is a macOS-first storage analyzer and developer-cache cleaner written in
Rust. It separates how much storage a directory occupies from how much deleting
it might reclaim: APFS clones and snapshots make those different questions.
Browse developer artifacts, inspect tool-managed caches, and review cleanup
plans with reclaimability intervals. Cleanup defaults to a dry run.

## Install

Homebrew (macOS and Linux):

```sh
brew install bencetotht/tap/cleanrs
```

Nix (Apple Silicon macOS and Linux):

```sh
nix run github:bencetotht/cleanrs -- --help
nix profile install github:bencetotht/cleanrs
```

Prebuilt tarballs for Apple Silicon, Intel Macs, and x86_64/aarch64 Linux are
published on [GitHub Releases](https://github.com/bencetotht/cleanrs/releases).
They include the binary, manual page, and bash/zsh/fish completions.

From source (Rust 1.85 or later):

```sh
cargo install --locked --git https://github.com/bencetotht/cleanrs cleanrs
```

See [installation](docs/installation.md) for checksums, shell setup, upgrades,
Full Disk Access, and Gatekeeper. Release channels become available with the
first published release; signing depends on the maintainer's Apple secrets.

## Quick start — TUI

```sh
cleanrs tui ~/Developer
# Or browse the current working directory:
cleanrs tui
```

The TUI requires an interactive terminal of at least 80×20. It respects
`NO_COLOR`. Browse folders, open **Targets** with `T`, tick individual candidates,
press `x` to rebuild and review the plan, then press `y` to run permanent cleanup.
The run screen streams provider output; results distinguish removed allocation,
provider-reported bytes, skips, and failures. `?` opens context-sensitive help.

| Group | Keys | Action |
|---|---|---|
| Navigation | `j` / `↓`, `k` / `↑` | Next / previous row |
| Navigation | `gg`, `G` | First / last row |
| Navigation | `Ctrl-d`, `Ctrl-u` | Half page down / up |
| Navigation | `l` / `Enter` / `→` | Open a browser directory |
| Navigation | `h` / `←` / `Backspace`, `-` | Parent / previous directory |
| Navigation | `/`, `n`, `N` | Filter; next / previous match |
| Navigation | `s`, `r` | Cycle size/name/age sort; rescan or re-detect |
| Navigation | `i` | Browser accounting info |
| Selection / targets | `T`, `S` | Folder targets / separate system-provider scope |
| Selection / targets | `Space`, `v` | Toggle item and move down / visual range |
| Selection / targets | `Ctrl-a`, `Ctrl-r`, `u` | Select eligible / invert tickable / clear ticks |
| Selection / targets | `1`, `2`, `3` | Safe / standard / aggressive preset; replaces manual ticks |
| Review / run | `x` | Rebuild selected plan and review |
| Review / run | `y`, `n` | Confirm cleanup / cancel review; confirm / cancel a stop request during run |
| Review / run | `[`, `]` | Scroll provider output up / down during run |
| Global | `?` | Toggle help |
| Global | `q` / `Esc`, `Ctrl-c` | Back or request quit; during run, request stop after current action |

These are the bindings in [keymap.rs](cleanrs/src/tui/keymap.rs). While filtering,
`Enter` confirms the filter and `Esc` clears it. On review, `Enter` never confirms
cleanup. Browser quit asks for confirmation; other screens go back one screen.

### How it decides

- Detection and planning are read-only. The TUI requires review and `y` before
  running; CLI cleanup requires `--apply`.
- `safe`, `standard`, and `aggressive` select actions by recoverability, rebuild
  cost, minimum level, and staleness. Manual ticks can override level/age policy;
  they cannot override safety refusals.
- Filesystem estimates are intervals with confidence. Tool-reported estimates
  and unknown sizes are labelled separately.
- Trash means moved bytes, not reclaimed bytes. Trash execution is currently
  deferred: the reserved `--trash` flag refuses execution.
- Synced cloud content is refused. User data requires explicit selection;
  increasing the cleanup level never selects it implicitly.

### Your first target scan

1. Run `cleanrs tui ~/Developer` and wait for folder sizes to populate.
2. Highlight a project directory and press `T`. If the highlighted row is itself
   a target directory, cleanrs uses its containing folder for detection.
3. Read the candidate's classification, consequence, age, and refusal/skip reason.
   Use `Space` to tick a candidate; `u` clears all ticks.
4. Press `x`. Cleanrs refreshes the selected evidence and presents exact paths or
   argv commands in review. A changed or refused candidate cannot run.
5. Press `n` to return with ticks retained, or `y` to confirm permanent cleanup.
   After execution, inspect results and use `cleanrs history` for the journal.

Folder Targets inspect workspace artifacts; `S` opens machine-wide providers in
an independent scope. Some detected storage is informational only and cannot
be selected for deletion.

## CLI

| Command | Purpose |
|---|---|
| `scan [PATH]` | Scan metadata without reading file contents |
| `tui [PATH]` | Browse, select, review, and run in a terminal |
| `doctor` | Inspect container, volumes, snapshots, swap, and access state |
| `targets [PATH]` | Detect and size targets without executing cleanup |
| `projects [PATH]` | Explain activity, Git evidence, and artifact recommendations |
| `clean [PATH]` | Plan cleanup; execute only with `--apply` |
| `history` | Show cleanup runs and action outcomes |
| `trust-config [PATH]` | Persist, list, or revoke command-config trust |
| `completions <SHELL>` | Write shell completions to stdout |
| `man` | Write the cleanrs(1) manual page to stdout |
| `help [COMMAND]` | Show command usage |

```sh
# Display one level, while scanning the complete subtree:
cleanrs scan ~/Developer --depth 1

# Machine-readable accounting:
cleanrs scan . --json

# Inventory known targets (add --workspace-only to skip system providers):
cleanrs targets .

# Review the default safe plan without executing it:
cleanrs clean .

# Explicitly select a candidate path, review, then confirm execution:
cleanrs clean . --select /absolute/path/to/project/node_modules --apply

# Inspect previous action outcomes and system accounting:
cleanrs history
cleanrs doctor

# Generate shell integration:
cleanrs completions zsh
```

`--apply` is always explicit in the CLI, and the plan is revalidated before
execution. Omitted paths mean the working directory. `--depth` limits display,
not traversal; `--cross-mounts` opts into mount traversal. `scan --reclaimable`
shows intervals, and `scan --explain <path>` explains one path's accounting.
Use `cleanrs <command> --help` for the complete flag reference.

## Extending cleanrs

Add no-code targets to `~/.config/cleanrs/targets.toml`, for example:

```toml
[[target]]
id = "custom.gradle-cache"
description = "Gradle dependency cache; downloads may be needed again"
detect = { path = "~/.gradle/caches" }
class = "network"
minimum_level = "standard"
reclaim = { method = "delete-children" }
```

A path target must be within the explicit scan scope. Command targets use argv
arrays; commands in the default user config are implicitly trusted. Commands in
an explicitly named `--config` require `cleanrs trust-config /path/to/targets.toml`
(or `--trust-config-commands`) after reviewing the file.

For deeper integrations, implement the Rust `Provider` trait in
[providers.rs](cleanrs-core/src/providers.rs). See the [provider guide](docs/providers.md)
and [configuration reference](docs/configuration.md). System package managers
requiring root (`apt`, `dnf`/`yum`, `pacman`) are not supported yet: cleanrs refuses
to execute cleanup as root.

## Platform support

| Platform | Accounting and targets |
|---|---|
| macOS | Native APFS private-size and clone metadata, container diagnostics, workspace and system targets; missing capabilities remain explicit |
| Linux | Portable allocated/apparent accounting and reduced `doctor`; no APFS clone/private-size accounting; macOS-specific system targets hidden |

Linux's portable backend uses allocated size as its private-size fallback; it
cannot detect sharing on reflink filesystems. Neither platform promises exact
container free-space recovery for every cleanup. See [accounting](docs/accounting.md).

## Safety

- Dry-run is the default. Review exact paths and commands before confirming.
- Symlinks, protected roots, cloud paths, dataless candidates, and unsafe ancestry
  are checked in code; dry-run is not the safety boundary.
- No cleanup level implicitly selects `user_data`; explicit choices remain
  subject to safety checks.
- Configuration is never discovered in the working directory. Explicit command
  configs are trusted by canonical path and SHA-256; edits invalidate trust.
- Commands use executable-plus-argv arrays, never shell evaluation. Execution
  refuses root and rechecks candidate/provider identity immediately before acting.
- Removed allocation and provider-reported bytes are evidence, not a measured
  increase in free space. Moving to Trash would not reclaim storage.

Details and enforcement locations are in [safety](docs/safety.md).

## Documentation

- [Installation](docs/installation.md): packages, completions, macOS permissions.
- [Configuration](docs/configuration.md): target schema, CLI targets, trust.
- [Providers](docs/providers.md): TOML recipes and Rust integrations.
- [Accounting](docs/accounting.md): intervals, APFS facts, diagnostics, limitations.
- [Architecture](docs/architecture.md): modules, pipeline, state locations.
- [Safety](docs/safety.md): enforced invariants and code references.
- [Releasing](docs/releasing.md): maintainer workflow and prerequisites.
- [VISION.md](VISION.md): design reference, including future work.
- [DEVELOPMENT_PLAN.md](DEVELOPMENT_PLAN.md): roadmap / progress.

## License

[MIT](LICENSE).
