# cleanrs

Fast storage usage analyzer and cleaner for developers, written in Rust. Fully transparent.

> **Status: design stage.** No implementation yet. See [VISION.md](VISION.md)
> for the full design.

## Why

Every disk usage tool on macOS answers the wrong question. `du` and everything
built on it report *logical* file sizes, which on APFS are frequently wrong:
clones share blocks, sparse files lie about their footprint, hardlinks get
double-counted, and firmlinks make your home directory appear twice.

cleanrs answers the question you actually have:

> **Where did my disk go, and how much do I actually get back if I delete this?**

## What it does

**Analyze.** A full-system scan built on `getattrlistbulk(2)` — one syscall per
batch of entries instead of two per file — reporting apparent, allocated, and
*freeable* sizes, with APFS clones and hardlinks deduplicated. Browsable in an
interactive TUI or scriptable via `--json`.

**Reconcile.** `cleanrs doctor` explains the gap between what `df` reports and
what a directory walk can see: snapshots, purgeable space, and paths that need
Full Disk Access.

**Clean.** Known developer storage sinks — Xcode `DerivedData`, `node_modules`,
Docker build cache, Homebrew, cargo artifacts, Nix — classified by how expensive
they are to regenerate, and reclaimed at a chosen intensity level.

## Safety

- Dry-run is the **default**. `--apply` is always required.
- Targets are classified by regeneration cost: `free`, `cheap`, `network`,
  `expensive`, `irreplaceable`. Levels map onto those classes, and no level ever
  touches `irreplaceable` data such as Docker volumes.
- Deletions go to the Trash unless explicitly made permanent.
- Where a tool owns its own storage, its cleanup command is used
  (`brew cleanup -s`, `docker builder prune`) rather than deleting files behind
  its back.
- Every action is journalled and readable via `cleanrs history`.

## Planned commands

```
cleanrs scan [PATH]      One-shot analysis (--json, --freeable, --depth)
cleanrs tui  [PATH]      Interactive browser
cleanrs doctor           Reconcile df vs. scan vs. snapshots vs. inaccessible
cleanrs targets          List detected cleanup targets with sizes
cleanrs clean            Reclaim space — dry-run by default
cleanrs history          Audit log of past cleans
cleanrs schedule         launchd agent for unattended weekly runs
cleanrs index            Cache control
```

Targets can be declared in `~/.config/cleanrs/targets.toml`, or defined entirely
on the command line — no config file is required:

```
cleanrs clean --target '~/Library/Developer/Xcode/DerivedData:free:delete-children'
cleanrs clean --level standard --apply
```

## Platform

macOS first, targeting APFS specifics directly. Linux support is planned and
isolated to a single crate (`cleanrs-fs`).

## License

See [LICENSE](LICENSE).
