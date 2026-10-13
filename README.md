# cleanrs

Storage usage analyzer and cleaner for developers, written in Rust. Fully transparent.

> **Status: Phases 0–3 implemented.** The read-only `scan`, `doctor`, and
> workspace/provider `targets` commands are joined by immutable cleanup plans,
> guarded `clean --apply`, persistent command-config trust, and action-level
> `history`. Direct deletion is descriptor-relative and provider commands are
> revalidated before execution. `cleanrs tui` now supports progressive sizes,
> navigation, accounting details, scoped target selection, and reviewed cleanup
> through the same guarded pipeline. Phase 5A polish comes next, followed by
> Phase 4 project hygiene; deeper snapshot
> and File Provider research remains deferred unless it enables a concrete
> capability. See
> [the findings](docs/findings-phase0.md), [VISION.md](VISION.md), and the
> explicit progress boundary in [DEVELOPMENT_PLAN.md](DEVELOPMENT_PLAN.md).

## Why

A directory walker can tell you how much space a file *occupies*. It cannot
reliably tell you how much space *deleting it will return*.

On APFS those are different questions. Blocks can be shared between cloned
files, pinned by snapshots, held as purgeable, hidden behind protected paths, or
managed inside a provider's own storage such as Docker's disk image. `du` gets
blocks and hardlinks right; none of the rest is visible to any walker.

cleanrs answers the second question:

> **Where did my disk go, and how much do I actually get back if I delete this?**

Its practical focus is recurring developer storage: discover caches, build
artifacts, runtimes, environments, and other known storage across tools such as
Homebrew, Nix, package managers, Docker/OrbStack, Cargo, and Xcode; then show or
run the correct native cleanup action at an explicit severity. Fast low-level
scanning supports that workflow rather than trying to replace a general-purpose
disk browser.

## What it does

**Analyze.** A scan built on `getattrlistbulk(2)` — attributes arrive with
directory entries instead of one `lstat` per file — reporting apparent,
allocated, and *immediately private* sizes, with clone and hardlink sharing
accounted for. Browsable in a TUI or scriptable via `--json`.

**Reconcile.** `cleanrs doctor` explains the gap between what storage reports
and what a walk can see: snapshots, purgeable space, and coverage the scan could
not achieve — without pretending the categories add up. Reconciliation is
**container-scoped**, because APFS volumes share one free-space pool; swap on
`/System/Volumes/VM` and a Nix store on `/nix` are invisible to any
single-volume accounting.

**Clean.** Known developer storage sinks — Xcode `DerivedData`, `node_modules`,
Docker and OrbStack build cache, Homebrew, pnpm/npm/Bun data, cargo artifacts,
Nix, and validated macOS caches — classified by how they recover and what that
costs, reclaimed at a chosen intensity.

**Protect.** Large but stateful objects such as VMware machines, Docker volumes,
Claude's Cowork VM, simulator device data, and macOS swap are still identified
and explained, but never mistaken for disposable cache data. System-managed
swap is diagnostic-only; user data requires an explicitly named action.

The initial known-location acceptance inventory—including the motivating
machine's home, Library and Developer trees, both container runtimes, package
manager caches, VMs, wallpaper cache, swap, and Xcode data—is mapped explicitly
in [VISION.md §7.3](VISION.md#73-baseline-known-location-coverage).

**Verify.** Every applied run records the reviewed estimate, action outcome,
removed tree allocation or provider-reported result, exact argv, exit status,
and bounded output/error evidence. `history` distinguishes success, failure,
skip, and validation abort.

**Correction:** an earlier README said Phase 3 already measured APFS container
free-space deltas and classified every discrepancy. It does not yet. Current
history calls its evidence “removed allocation” or “provider reported” and does
not attribute noisy container free-space changes to cleanrs. Container-delta
calibration remains planned accounting work.

## Honesty about numbers

Reclaimable space is reported as an **interval with a derived confidence**, not
as a promise:

```
18.2 GiB reclaimable · high confidence

12.4–18.2 GiB reclaimable · medium confidence
Cloned blocks shared with paths outside this selection
```

The reason is that group deletion is *superadditive*: two files that clone only
each other each report zero bytes as privately theirs, yet deleting both frees
everything. Summing per-file figures — which is the obvious implementation —
under-reports worst exactly where the tool is most useful, in build trees that
clone internally. So cleanrs brackets the answer instead: a floor of what is
provably private, a ceiling of clone-deduplicated allocation. When nothing is
shared the two collapse into one number, which is the common case.

Confidence is that interval's tightness, not an adjective added afterwards.

Exact freeable bytes is a research goal, not a claimed capability. What is
actually derivable from APFS metadata is the first thing the project will
establish, before anything is built on top of it.

## Safety

- Dry-run is the **default**. CLI mutation requires `--apply`; the TUI requires
  an explicit `y` on its review screen.
- `--apply` deletes permanently, because moving files to the Trash does not
  return space to the disk. `--trash` is intentionally deferred and currently
  refuses; its future accounting must report *moved, not reclaimed*.
- **Synced cloud storage is never deleted.** iCloud Drive and
  `~/Library/CloudStorage` hold dataless files whose deletion propagates to every
  other device, with no undo. Those locations are refused outright, and — because
  an ordinary `node_modules` detector will happily match inside a synced folder —
  every individual candidate path is also checked for dataless status before it
  can enter a plan. Freeing local space there means **evict**, not delete.
- Scanning never materializes cloud content. A tool that downloads 200 GB while
  claiming to save space is worse than useless.
- Targets are classified by how they recover (`generated`, `downloadable`,
  `user_data`) and what rebuilding costs. No intensity level ever touches
  `user_data` such as Docker volumes; those must be named explicitly.
- Where a tool owns its storage, cleanrs runs its native planned action after
  revalidation rather than deleting files behind the provider's back.
- Provider identity is part of every target. Docker contexts such as OrbStack
  and Docker Desktop are inventoried separately, and a plan is pinned to the
  context it inspected so a context change cannot redirect cleanup.
- cleanrs **never loads configuration from the current directory**. A cloned
  repository cannot make `cleanrs clean --apply` run its commands. Commands are
  argv arrays executed directly; there is no shell anywhere.
- Deletion is guarded by enforced path invariants, not by dry-run alone.
- Every action is journalled and readable via `cleanrs history`.

## Command surface

```
cleanrs scan [PATH]      One-shot analysis (--json, --reclaimable, --explain)
cleanrs doctor           Reconcile container state; report the unexplained gap
cleanrs targets [PATH]   List workspace targets and machine-wide provider actions
cleanrs clean [PATH]     Plan cleanup; --select PATH picks items; --apply executes
cleanrs history          Action outcomes and evidence from applied runs
cleanrs trust-config     Trust/list/revoke an explicit command config by hash
cleanrs tui [PATH]       Browse storage, select targets, review and run cleanup
cleanrs schedule         Planned launchd maintenance
cleanrs index            Planned cache control
```

Targets live in `~/.config/cleanrs/targets.toml`, or can be defined entirely on
the command line — no config file is required:

```
cleanrs targets . --target '~/Library/Developer/Xcode/DerivedData:free:delete-children'
cleanrs clean . --target-argv '["/absolute/tool", "cache", "prune"]'
cleanrs targets --export
```

`--target` uses `PATH:CLASS[:delete|delete-children]`; `--target-argv` is a JSON
argv array and is never parsed by a shell. `--targets-only-cli` ignores both
built-ins and config files, while `--no-builtin-targets` retains configured and
CLI targets. The implicit config location is only
`~/.config/cleanrs/targets.toml`; another file must be named with `--config`.
Explicit config files containing commands require persistent trust tied to the
canonical path and current SHA-256 content. Review and trust one with
`cleanrs trust-config PATH`; editing or replacing it invalidates trust.
`--trust-config-commands` is a convenience that records that same persistent
decision for the explicitly named `--config`.

```toml
disable = ["docker.images"]

[[target]]
id = "workspace.coverage"
description = "generated test coverage"
detect = { marker = "Cargo.toml", dir = "coverage" }
class = "free"
reclaim = { method = "delete" }
stale_after = "14d"
scope = ["~/Developer"]
```

The TOML schema rejects unknown fields, incomplete detectors, invalid age
values, and mismatched detector/operation pairs. A command target must use
`reclaim = { method = "run", argv = [...] }`, and its first argv element must
exactly match `detect.command`.

## Platform

macOS first, targeting APFS specifics directly. Linux support is planned;
several parts of the design have platform seams, not just the filesystem layer.

## Build and try it

Rust 1.85 or newer is required.

```sh
cargo build --release
./target/release/cleanrs scan .
./target/release/cleanrs scan . --reclaimable --depth 2
./target/release/cleanrs doctor
./target/release/cleanrs doctor --scan .
./target/release/cleanrs targets .
./target/release/cleanrs clean . --level standard
./target/release/cleanrs history
```

`scan` defaults to the current directory and allocated-size ranking. It never
defaults to the home directory, never follows symlinks, and never reads file
contents. JSON output, metric selection, depth/minimum-size filters, exclusions,
path explanations, shell completions, and a generated man page are available in
`cleanrs --help`.

`targets` also defaults its workspace scan to the current directory rather than
the home directory. It combines marker-scoped Node.js artifacts, Cargo-aware
workspace builds, configured targets, and machine-wide provider inventory;
deduplicates candidates, applies staleness policy, and displays the same honest
reclaimability intervals. `--workspace-only` skips provider inspection.
Candidate-level cloud/dataless and mount-boundary
checks can refuse a finding without hiding it. Configured paths additionally
refuse symlinked ancestors and paths that escape the explicit scan scope.
Targets nested beneath an already detected artifact root are collapsed, so
package metadata inside `node_modules` does not flood or double-count the
result. Human output is sorted by reclaimable ceiling and shows the largest 25
targets by default; use `--top N` or `--min-size SIZE` to tune it. JSON remains
complete unless one of those filters is supplied.
The default `targets` and `clean` tables lead with reclaimable size, project
name, and a home-relative location such as
`~/Developer/Projects/example/target`. Exact commands, consequences, provider
identities, and evidence remain in `--json`, keeping ordinary terminal rows
compact enough to scan quickly.

Homebrew is detected through its resolved executable, version, and prefix.
Routine cleanup, scrub, and autoremove are separate actions with exact argv
plans and explicit consequences. Native dry-run estimates remain labelled
provider-reported rather than being mixed with APFS intervals; overlapping
routine and scrub estimates are never added together. Ordinary Homebrew cleanup
starts at `standard` because the native operation mixes stale downloads with old
installed versions, while scrub starts at `aggressive`.

The provider inventory also covers Nix; pnpm, npm/npx, Yarn, and Bun; uv, pip,
Poetry, and Conda; Cargo and rustup; CocoaPods; Docker and OrbStack; and Xcode.
Each action has its own stable ID, severity, consequence, provider identity,
and exact argv when a supported native action exists. Unsupported or stateful
objects—npm's separate npx cache, Python environments, Rust toolchains and
shared Cargo caches, OrbStack machines, Docker volumes, Xcode archives/device
support/simulator data—remain informational. Whole-cache Xcode DerivedData is
generated but high-cost and enters a plan only when its stable ID is named.
Docker inspects local Unix-socket
contexts separately, pins both context name and endpoint, skips remote contexts,
and never combines build cache, stopped containers, images, and volumes into a
generic prune.

For Rust projects, Cargo metadata is queried with `--frozen --no-deps` for at
most 256 manifests under the explicit scan root. Workspace members sharing a
target directory collapse into one candidate. Activity is the newest
`Cargo.toml` or build-artifact timestamp, with 14 days as the default stale
threshold; `--cargo-stale-after DAYS` changes it for one inspection. A
workspace-local `target/` receives an exact, workspace-pinned
`cargo clean --frozen --manifest-path ... --target-dir ...` plan. Custom,
shared, or out-of-scope target directories are still shown but refused until
complete ownership can be proven; source repositories are never cleanup
targets.

`clean --select PATH` selects an individual detected candidate; repeat the flag
for multiple items. It selects exactly those items, overriding severity and
staleness policy, while safety refusals remain enforced. Manual choices are
marked in the plan and history; explicit-only/user-data actions still require
interactive confirmation. Command candidates can be selected by the exact
`candidate_key` string from `clean --json`. Dry-run remains the default.

`clean` uses the same explicit workspace scan, candidate safety checks, and
provider inspection, then explains which actions the selected cleanup level and
staleness policy include. Dry-run remains the default. `--apply` executes that
same owned reviewed plan after confirmation (or `--yes`), refusing root
execution and revalidating identities immediately before each action. JSON apply
requires `--yes`. Direct deletion performs another metadata-only cloud, mount,
coverage, and identity preflight before descriptor-relative traversal; provider
commands revalidate their executable and provider context and never use a shell.
Informational findings cannot enter a plan, explicit-only/user-data actions
require a named ID and interactive confirmation, and unattended plans are
capped at 100 actions or 50 GiB. Use `--ignore-staleness` only when intentionally
reviewing currently active artifacts.

All destructive project tests use unique temporary fixture directories. During
manual development, never run `--apply` without a specific sandbox path.

Applied runs are appended to a private, rotating JSONL journal under
`$XDG_STATE_HOME/cleanrs/history.jsonl` or
`~/.local/state/cleanrs/history.jsonl`. A pending record is durable before the
first mutation and each completed action adds a newer snapshot, so interruption
remains visible. `cleanrs history` collapses those snapshots into one run and
shows what ran, what was skipped, and what failed. `[manual]` means
“policy-eligible, picked by hand”; `[manual: <reason>]` means an explicit policy
override. Every final TUI tick, including a preset pick, is journalled as manual.

`doctor --scan PATH` reconciles one explicit scan scope against Data-volume
usage and labels the remainder as unattributed. Without `--scan`, it stays cheap
and reports the path attribution as unknown rather than inventing a number.

### Interactive cleanup

`cleanrs tui [PATH]` requires a terminal and starts in the working directory
when PATH is omitted. Use `j`/`k` to move, `l`/Enter to open, `h` to go up,
`/` to filter, `s` to sort, and `i` for accounting details. `T` opens cleanup
targets for the highlighted directory, or the current directory for a file.
On a recognized target row such as `node_modules/`, `T` uses the containing
folder and focuses that target.

In targets, Space toggles an item and moves down; `v` starts a range (move,
then `v` or Space to select it; Esc cancels). Ctrl-a adds level-eligible items,
Ctrl-r inverts selectable items, and `u` clears ticks. `1`/`2`/`3` reset to
safe/standard/aggressive, discarding manual ticks. `m` includes machine-wide
providers; `r` re-detects. Re-detection retains choices for surviving selectable
candidates. Returning with `q`/Esc keeps the cached selection and detection age until that
scope is evicted from the bounded session cache. The project-path column is
relative to the targets scope; the detail pane keeps the exact operation.

The selected interval updates immediately from cached detection facts, using
the same set accounting as CLI plans; ticks and level presets do not rescan.
`T` reuses completed browser scans when all child directories are available;
incomplete scans or newly added/replaced directories fall back to a full scan.
`r` always scans fresh. Cached detection keeps the browser facts' original age.
`x` rebuilds and reviews exactly the ticked candidates with visible progress,
scanning their trees and ancestor listings rather than unrelated projects.
If an item vanished, changed identity/operation, or became refused since
detection, review names the dropped selection. Ticking is explicit
naming: level/staleness overrides appear in a warned group, and explicit-only
or user-data choices carry a stronger warning. Refused and informational items
cannot be selected. Only `y` on review confirms permanent deletion; Enter does
not confirm, and `n`/Esc returns with ticks intact. During a run, `q` or Ctrl-c
asks to stop after the current action; `[`/`]` scroll provider output. Results
separate removed allocation from provider reports, show the run ID, and point
to `cleanrs history`. Returning to the browser refreshes affected directories.

Session caches evict the oldest completed scans as whole arenas, including
all descendant aliases and inactive listings that retain them. Browser arenas
are capped at 250,000 nodes and 4,096 path aliases; listings at 32 folders and 250,000 inactive rows.
Target detections are capped at eight scopes and 250,000 nodes in total.
The visible folder/targets, reviewed plan, and in-flight work are working data
and may exceed the cache limits; leaving oversized views releases their data.
Review groups show action counts, numbered operations, confidence and rebuild
consequences above the permanent-deletion confirmation.

`?` shows context help (scroll with `j`/`k`). `NO_COLOR` preserves meaningful
symbols. Browser scan age and target detection age use seconds/minutes/hours;
data older than ten minutes is yellow with “press r to refresh”. Errors appear
as red status messages; any key dismisses them and navigation still works.
The TUI supports terminals of at least 80×20, and shows a centered message
below that size. Diagnostics go to `~/.local/state/cleanrs/tui.log` (or
`$XDG_STATE_HOME/cleanrs/tui.log`) beside the history journal.

| Screen | Main keys |
|---|---|
| All | `?` help; `q`/Esc back (quit in browser); Ctrl-c quit (ask to stop during a run) |
| Lists | `j`/`k` or arrows move; `gg`/`G` first/last; Ctrl-d/Ctrl-u half page; `/` filter; `n`/`N` matches; `s` sort |
| Browser | `l`/Enter/right open; `h`/left/Backspace parent; `-` previous directory; `T` targets; `i` info; `r` rescan |
| Targets | Space toggle; `v` range; Ctrl-a eligible; Ctrl-r invert; `u` clear; `1`/`2`/`3` presets; `m` machine scope; `r` re-detect; `x` review |
| Review | `y` confirm; `n`/Esc cancel; Enter does not confirm |
| Run | `q`/Esc/Ctrl-c ask to stop; `y`/`n` answer; `[`/`]` scroll provider output |
| Results | `j`/`k`, `gg`/`G`, Ctrl-d/Ctrl-u scroll; `q`/Esc returns to refreshed browser |

To clean only some items:

1. Start `cleanrs tui PATH`, navigate to a project, and press `T`.
2. Press `u` to clear the initial preset. Move to each desired item and press
   Space; only those ticks will enter review. `[!]` warns about a policy
   override and `✗` marks an untickable refusal.
3. Press `x`, wait for the fresh reviewed plan, and read its actions and any
   dropped-item notice. Press `n` to revise your picks or `y` to delete exactly
   the reviewed items permanently.
4. Read results and use `cleanrs history` for the recorded evidence.

### Terminal output

Interactive scans show live directory, entry, and allocated-byte counters while
the filesystem walk is running. Human-readable `scan` and `doctor` results use
compact tables with consistent colors for confidence, coverage, warnings, and
unknown values. Output automatically adapts to its destination:

- TTY: Unicode tables, color, and an animated progress indicator.
- Pipe or redirected output: ASCII tables and no progress animation.
- `NO_COLOR` set: no ANSI colors.
- `--json`: stable JSON on stdout and no progress animation; `--apply` still
  prints the exact destructive operation to stderr immediately before it runs.

Completion scripts and man pages remain raw output suitable for redirection.

## License

See [LICENSE](LICENSE).
