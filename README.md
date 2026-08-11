# cleanrs

Storage usage analyzer and cleaner for developers, written in Rust. Fully transparent.

> **Status: design stage.** No implementation yet. See [VISION.md](VISION.md)
> for the full design, including what is still an open research question.

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

**Reconcile.** `cleanrs doctor` explains the gap between what the volume reports
and what a walk can see: snapshots, purgeable space, and coverage the scan could
not achieve — without pretending the categories add up.

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
and the measured change in free space. Estimates are calibrated against reality
rather than trusted.

## Honesty about numbers

Reclaimable space is an **estimate with a stated confidence**, not a promise:

```
18.2 GiB estimated reclaimable · medium confidence
Shared APFS extents and snapshots may reduce the result
```

Exact freeable bytes is a research goal, not a claimed capability. What is
actually derivable from APFS metadata is the first thing the project will
establish, before anything is built on top of it.

## Safety

- Dry-run is the **default**. `--apply` is always required.
- `--apply` deletes permanently, because moving files to the Trash does not
  return space to the disk. `--trash` is an opt-in recovery mode that reports
  *moved, not reclaimed* — and is never used by scheduled runs.
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
cleanrs doctor           Reconcile volume state; report the unexplained gap
cleanrs targets          List detected cleanup targets with sizes and confidence
cleanrs clean            Reclaim space — dry-run by default
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

## License

See [LICENSE](LICENSE).
