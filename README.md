# cleanrs

Storage usage analyzer and cleaner for developers, written in Rust. Fully transparent.

> **Status: Phase 0/1 MVP plus Phase 2 groundwork.** The read-only `scan`,
> `doctor`, initial workspace-scoped `targets`, and dry-run `clean` planning
> commands are implemented.
> Phase 0's live cloud/snapshot safety experiments remain open; see
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

**Verify.** Every run records what was estimated, what the provider reported,
and the measured change in container free space. Where the result falls short,
`history` names the reason from a known set — open file descriptors, snapshot
coverage, clone sharing, provider-internal reclamation — instead of leaving you
to guess. Estimates are calibrated against reality rather than trusted.

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

- Dry-run is the **default**. `--apply` is always required.
- `--apply` deletes permanently, because moving files to the Trash does not
  return space to the disk. `--trash` is an opt-in recovery mode that reports
  *moved, not reclaimed* — and is never used by scheduled runs.
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
- Where a tool owns its storage, it is cleaned through its own interface —
  inspect, plan, execute, verify — not by deleting files behind its back.
- Provider identity is part of every target. Docker contexts such as OrbStack
  and Docker Desktop are inventoried separately, and a plan is pinned to the
  context it inspected so a context change cannot redirect cleanup.
- cleanrs **never loads configuration from the current directory**. A cloned
  repository cannot make `cleanrs clean --apply` run its commands. Commands are
  argv arrays executed directly; there is no shell anywhere.
- Deletion is guarded by enforced path invariants, not by dry-run alone.
- Every action is journalled and readable via `cleanrs history`.

## Planned commands

```
cleanrs scan [PATH]      One-shot analysis (--json, --reclaimable, --explain)
cleanrs tui  [PATH]      Interactive browser
cleanrs doctor           Reconcile container state; report the unexplained gap
cleanrs targets [PATH]   List workspace targets with sizes and confidence (initial slice)
cleanrs clean [PATH]     Plan workspace cleanup — dry-run only in the current slice
cleanrs history          Estimated vs. observed results of past runs
cleanrs schedule         launchd agent for unattended weekly runs
cleanrs index            Cache control
```

Targets live in `~/.config/cleanrs/targets.toml`, or can be defined entirely on
the command line — no config file is required:

```
cleanrs clean --target '~/Library/Developer/Xcode/DerivedData:free:delete-children'
cleanrs clean --level standard --apply
```

## Platform

macOS first, targeting APFS specifics directly. Linux support is planned;
several parts of the design have platform seams, not just the filesystem layer.

## Build and try the read-only MVP

Rust 1.85 or newer is required.

```sh
cargo build --release
./target/release/cleanrs scan .
./target/release/cleanrs scan . --reclaimable --depth 2
./target/release/cleanrs doctor
./target/release/cleanrs doctor --scan .
./target/release/cleanrs targets .
./target/release/cleanrs clean . --level standard
```

`scan` defaults to the current directory and allocated-size ranking. It never
defaults to the home directory, never follows symlinks, and never reads file
contents. JSON output, metric selection, depth/minimum-size filters, exclusions,
path explanations, shell completions, and a generated man page are available in
`cleanrs --help`. No deletion or provider-execution path exists yet.

`targets` also defaults to the current directory rather than the home directory.
The initial slice detects marker-scoped Cargo and Node.js build artifacts,
deduplicates candidates, applies staleness policy, and displays the same honest
reclaimability intervals. Candidate-level cloud/dataless and mount-boundary
checks can refuse a finding without hiding it. Configuration, provider targets,
and the full known-location catalog remain Phase 2 work.
Targets nested beneath an already detected artifact root are collapsed, so
package metadata inside `node_modules` does not flood or double-count the
result. Human output is sorted by reclaimable ceiling and shows the largest 25
targets by default; use `--top N` or `--min-size SIZE` to tune it. JSON remains
complete unless one of those filters is supplied.

`clean` uses the same explicit workspace scan and candidate safety checks, then
explains which targets the selected cleanup level and staleness policy would
include. It records each candidate's device, inode, and type in preparation for
the Phase 3 replacement check. Dry-run is the only available mode: `--apply`
refuses before scanning, and tests verify that neither form alters fixture data.
Use `--ignore-staleness` only when intentionally reviewing currently active
artifacts.

`doctor --scan PATH` reconciles one explicit scan scope against Data-volume
usage and labels the remainder as unattributed. Without `--scan`, it stays cheap
and reports the path attribution as unknown rather than inventing a number.

### Terminal output

Interactive scans show live directory, entry, and allocated-byte counters while
the filesystem walk is running. Human-readable `scan` and `doctor` results use
compact tables with consistent colors for confidence, coverage, warnings, and
unknown values. Output automatically adapts to its destination:

- TTY: Unicode tables, color, and an animated progress indicator.
- Pipe or redirected output: ASCII tables and no progress animation.
- `NO_COLOR` set: no ANSI colors.
- `--json`: stable JSON on stdout and no progress output on stderr.

Completion scripts and man pages remain raw output suitable for redirection.

## License

See [LICENSE](LICENSE).
