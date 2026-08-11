# cleanrs — Vision & Design

> **Status:** design document. No implementation yet. This file is the reference
> for what cleanrs is, why it exists, and how it should be built.

---

## 1. Thesis

Every disk usage tool on macOS answers the wrong question. `du` and everything
built on top of it (`ncdu`, `gdu`, `dust`, Finder's Get Info) report *logical*
file sizes. On APFS that number is frequently, and sometimes wildly, wrong.

cleanrs answers one question instead:

> **Where did my disk go, and how much do I actually get back if I delete this?**

"Freeable bytes" — the number of blocks that would actually return to `df` — is
the headline metric everywhere: in the interactive browser, in `clean --dry-run`,
in the weekly scheduled report. That single framing unifies the two halves of the
project. The analyzer and the cleaner are not two products; they are one metric
computed at two scopes.

Two design values sit underneath everything:

- **Efficiency.** A full-system scan should be fast enough that you never avoid
  running it. The target is a large multiple of `du`, achieved by minimising
  syscalls rather than by throwing threads at the problem.
- **Transparency.** The tool never deletes anything you did not see listed
  first. Dry-run is the default. Every destructive action is journalled. The
  exact commands and paths are always printed before they run.

### 1.1 Non-goals

- Being a general-purpose file manager.
- A GUI. The TUI is the visual surface; anything richer is out of scope.
- Beating `gdu` on Linux. Linux support is a portability exercise, not a
  competitive one.
- Cleaning things that are not developer-generated. cleanrs is not a "Mac
  cleaner" in the consumer-grade, adware-adjacent sense, and should never
  present itself as one.
- Heuristic "smart" deletion of user data. If cleanrs cannot explain in one line
  why a path is safe to remove, it does not remove it.

---

## 2. Why the existing tools are wrong on macOS

These are the accounting hazards the core has to handle correctly. Getting these
right *is* the differentiator.

**APFS clones.** Copying a file on APFS does not copy its blocks; the copy
shares them until one side diverges. Xcode builds, Docker layers, `cp`, and
Finder duplicates all produce clones. Two files that both report 5 GB may occupy
5 GB in total. `du` reports 10 GB. Deleting one of them frees nothing.

**Sparse files.** A file's logical length can vastly exceed its allocated
blocks. `Docker.raw` is the canonical example: a nominally 64 GB disk image
occupying 9 GB. Reporting the logical size is useless; reporting allocated
blocks is correct.

**Hardlinks.** Multiple directory entries, one inode, one set of blocks.
Counting each entry double-counts the data, and deleting one entry frees
nothing.

**Firmlinks.** On modern macOS the read-only System volume and the writable Data
volume are stitched together by firmlinks. Scanning `/` traverses `/Users` and
`/System/Volumes/Data/Users` as two ordinary directories, producing a phantom
duplicate of the entire home directory. `stat` does not reliably surface the
`SF_FIRMLINK` flag, so this has to be handled structurally (see §4.3).

**Snapshots and purgeable space.** APFS local snapshots (Time Machine) and
purgeable content occupy real blocks that no directory walk will ever see. This
is why `df` says the volume is 95% full while a scan accounts for far less.
Users read that discrepancy as a bug in the tool.

**TCC / Full Disk Access.** An unsandboxed CLI binary without Full Disk Access
receives `EPERM` across large parts of `~/Library`. Silently skipping those
paths produces a confidently wrong total.

---

## 3. The three numbers

Every node in the tree carries three sizes. The distinction is exposed in the
UI, not hidden behind a single "size" column.

| Metric | Definition | Use |
|---|---|---|
| **Apparent** | Sum of logical file lengths (`st_size`). | Compatibility with `du`; answering "how big is this file conceptually". |
| **Allocated** | Sum of `st_blocks * 512`. Sparse-correct. | Honest on-disk footprint of the entries as counted. |
| **Freeable** | Allocated, with each set of shared blocks credited exactly once, at the lowest common ancestor of the entries that share them. | **The default.** What `rm -rf` on this path would return to `df`. |

Freeable is computed by deduplicating two kinds of sharing:

- **Hardlinks** — group by `(st_dev, st_ino)`.
- **APFS clone families** — group by clone ID.

A shared-block family is credited to the lowest common ancestor of all its
members *within the scanned scope*. If members exist outside the scope, the
family credits **zero** freeable bytes to the scanned path, because deleting
your copy does not release blocks another copy still references. This is the
detail that makes the number trustworthy.

Freeable is therefore not additive across sibling subtrees in the naive sense —
two siblings can each report 0 freeable while their parent reports 5 GB. The UI
must make this legible rather than pretending it away; the parent row is the
truthful one.

---

## 4. The scanner

### 4.1 It is a syscall problem, not a CPU problem

The classic walk is `readdir()` for names plus one `lstat()` per entry — roughly
two syscalls per file. For 400k files, ~800k syscalls. In a profiled, optimised
macOS scanner, **~91% of wall time is inside syscalls**; lock contention is
~1.5% and scheduler overhead is the remainder. Any threading cleverness is
competing for the last 9%.

`getattrlistbulk(2)` collapses that cost. One call returns many directory
entries *together with their attributes*, and the kernel avoids materialising a
vnode per entry. A 10,000-file directory costs on the order of 10 syscalls
instead of 20,000.

The decisive property for cleanrs: **file type, name, logical size, allocated
blocks, inode, mtime, and clone ID all arrive in the same bulk call.** APFS-
correct accounting costs no extra syscalls. Speed and accuracy are the same
optimisation, not a trade-off between them.

Reference points on a 4,095-directory / 409,500-file tree: `du -sh` 3.33 s,
`diskus` 1.34 s, a `getattrlistbulk`-based Rust walker 0.52 s (~6.4× `du`).

### 4.2 Concurrency model

- Work-stealing pool over a queue of **directories** (`rayon` or a hand-rolled
  `crossbeam-deque`). Parallelism is at directory granularity; bulk reads carry
  the within-directory case.
- Bounded in-flight directory count (default in the region of 64), tunable.
- **Storage-aware defaults.** On NVMe, oversubscribe (2–4× cores). On rotational
  or network-backed mounts, parallelism actively hurts — detect via `statfs` and
  drop to 1–2 workers.
- **Sharded inode/clone-ID sets** (16–64 shards, hashed). A single shared set is
  the one place this design reliably develops lock contention.
- **No async runtime.** This is pure blocking syscall work; tokio would add a
  scheduler that buys nothing. Threads and a work queue are the correct shape.

### 4.3 Traversal rules

- **Never cross mount boundaries by default.** Compare `st_dev` against the
  root's; `--one-file-system` behaviour is the default, `--cross-mounts` opts
  out. This alone neutralises most firmlink double-counting.
- **Explicit firmlink handling** when scanning `/`: `/System/Volumes/Data` and
  its firmlinked children are recognised and traversed once, from one side only.
- **Never follow symlinks.** Count the link entry itself, never its target.
- **Skip pseudo-filesystems and known traps**: `/dev`, `/proc`-like mounts,
  `.fseventsd`, `.Spotlight-V100`, network mounts under `/Volumes` unless asked.
- **Record, never swallow, permission errors.** Every `EPERM`/`EACCES` path is
  counted and surfaced as "unaccounted" (see §5).

### 4.4 Memory layout

Do not build `Arc<Node>` trees. Use an arena:

- `Vec<Node>` indexed by `u32`, with `parent` / `first_child` / `next_sibling`
  indices.
- Names interned per directory into a flat byte buffer with `(offset, len)`.
- `u64` sizes, packed flags for kind/error/shared state.

For a 5M-file scan this is the difference between roughly 250 MB and 2 GB. It
also makes the on-disk index format close to a memcpy of the arena (§7).

Notably, `gdu` resorts to disabling Go's garbage collector during analysis to
stay fast. In Rust the equivalent problem is avoided by choosing the layout — a
good demonstration of why the language choice pays off here.

---

## 5. `doctor` — reconciling with reality

A directory walk will never sum to what `df` reports, and users read that as a
bug. Make it a first-class feature instead:

```
cleanrs doctor

  Volume  /System/Volumes/Data                  1.86 TB
  ├─ Files (freeable)                           1.02 TB
  ├─ APFS local snapshots                        186 GB   tmutil deletelocalsnapshots
  ├─ Purgeable / other                            94 GB
  └─ Unaccounted (permission denied)              12 GB   47 paths — needs Full Disk Access
```

`doctor` is also the natural home for environment diagnostics:

- Full Disk Access status, with the exact remediation path for the user's
  terminal binary (`System Settings → Privacy & Security → Full Disk Access`).
- Index freshness and whether FSEvents-based incremental scanning is viable.
- Which cleanup targets were detected, and which required tools are missing.

This is the command you run when you are at 98% and need to know what is going
on. It should be cheap and it should never require a full scan to give a useful
first answer.

---

## 6. The cleaner

### 6.1 Classify by regeneration cost, not by "cache vs. not"

"Is it a cache?" is the wrong axis — it does not tell the user what it costs to
be wrong. cleanrs classifies every target by **what it takes to get the data
back**, and the intensity levels map directly onto that classification.

| Class | Meaning | Examples |
|---|---|---|
| `free` | Regenerates offline, effectively instantly | Xcode `DerivedData`, `target/`, `.next`, `__pycache__`, `.turbo` |
| `cheap` | Regenerates offline, costs CPU time | Docker build cache, `ccache`, `~/.cargo/registry/cache` |
| `network` | Requires the internet to come back | `node_modules`, cargo registry sources, Go module cache |
| `expensive` | Hours of work or a very large download | Xcode simulator runtimes, Docker images, model weights |
| `irreplaceable` | **Real data that cannot be regenerated** | Docker *volumes*, iOS device backups, Simulator app data |

Levels are defined purely in terms of these classes:

| Level | Removes | Intended cadence |
|---|---|---|
| `safe` | `free`, `cheap` | Weekly, scheduled, unattended |
| `standard` | `+ network` | Monthly, or when space is tight |
| `aggressive` | `+ expensive` | Manual, when you actually need the space |
| — | `irreplaceable` | **Never.** Only via an explicit per-target opt-in flag. |

The `irreplaceable` class exists primarily to encode one specific hazard:
`docker system prune --volumes` is a single flag away from destroying someone's
development database. Images and build cache are `cheap`/`expensive`; volumes
are `irreplaceable` and must be gated separately. No level ever reaches them.

### 6.2 Delegate to the owning tool

Where a tool owns its own storage and ships a cleanup command, **call it**.
`rm -rf` on Homebrew's cellar or Docker's storage corrupts their state databases
and leaves the user worse off than before.

| Target | Correct reclaim |
|---|---|
| Homebrew | `brew cleanup -s` |
| Docker build cache | `docker builder prune -f` |
| Docker images | `docker image prune -a -f` |
| Nix | `nix-collect-garbage -d` |
| Xcode simulators | `xcrun simctl delete unavailable` |
| Cargo build artifacts | `cargo clean`, or `cargo sweep` where available |

Direct deletion is reserved for artifacts with no owning cleanup command:
`DerivedData`, `node_modules`, `.next`, `dist`, `__pycache__`, and similar.

### 6.3 Safety mechanics

- **Dry-run is the default.** `clean` plans and prints; `--apply` executes.
- **Trash, don't unlink.** Manual deletions go to the macOS Trash at `safe` and
  `standard` levels. Permanent removal requires `aggressive` or `--permanent`.
- **Journal everything.** Every action — target id, paths, bytes reclaimed,
  command run, exit status, timestamp — is appended to a log under
  `~/.local/state/cleanrs/`. `cleanrs history` reads it back. When a user asks
  "what ate my files last Tuesday", the tool can answer.
- **Confirm before running external commands** unless `--apply --yes`, and
  always print the exact argv.
- **Never run as root by default.** Refuse with an explanation if invoked as
  root without an explicit override.

### 6.4 Staleness

`atime` is not trustworthy on APFS — updates are lazy and it is a poor proxy for
"was this used recently". Use instead:

- `mtime` of the project marker (`Cargo.toml`, `package.json`, `go.mod`).
- `mtime` of the artifact directory itself.
- Where the ecosystem provides better signal, use it (e.g. `cargo sweep`'s
  toolchain/fingerprint-based dating of build artifacts).

Targets declare `stale_after`; `clean` skips anything fresher unless
`--ignore-staleness` is passed.

---

## 7. Targets: config file *and* pure CLI

Targets are **data, not code**. A curated default set ships embedded in the
binary; users extend or override it via TOML; and **any target expressible in
TOML must also be expressible entirely on the command line**, so that cleanrs is
fully usable with no configuration file at all.

### 7.1 The target model

| Field | Meaning |
|---|---|
| `id` | Stable identifier, e.g. `xcode.derived-data`. Used by `--only` / `--exclude`. |
| `detect` | How to know this target applies: a path that exists, a marker file found by scan, or an available command. |
| `class` | One of `free`, `cheap`, `network`, `expensive`, `irreplaceable`. |
| `reclaim` | `delete` (with `children_only`) or `run` (an argv). |
| `stale_after` | Optional minimum age before the target is eligible. |
| `scope` | Optional roots to search under, for marker-based targets. |

### 7.2 Config form

```toml
[[target]]
id = "xcode.derived-data"
detect = { path = "~/Library/Developer/Xcode/DerivedData", exists = true }
class = "free"
reclaim = { method = "delete", children_only = true }
stale_after = "14d"

[[target]]
id = "node.modules"
detect = { marker = "package.json", dir = "node_modules" }
scope = ["~/Developer", "~/Projects"]
class = "network"
reclaim = { method = "delete" }
stale_after = "30d"

[[target]]
id = "docker.build-cache"
detect = { command = "docker" }
class = "cheap"
reclaim = { method = "run", cmd = ["docker", "builder", "prune", "-f"] }
```

Resolution order, later overriding earlier by `id`:

1. Embedded default manifest (`include_str!`).
2. `~/.config/cleanrs/targets.toml`.
3. `./cleanrs.toml` in the working directory, if present.
4. CLI flags.

`--no-builtin-targets` drops step 1 entirely, for users who want a fully
explicit set.

### 7.3 CLI form

Ad-hoc targets are defined with repeatable `--target` flags. A compact spec
covers the common case:

```
cleanrs clean --target '~/Library/Developer/Xcode/DerivedData:free:delete-children'
cleanrs clean --target '~/Downloads/sdks:expensive:delete' --level aggressive
```

Spec grammar: `PATH:CLASS[:METHOD]`, where `METHOD` is `delete` (default) or
`delete-children`, and `CLASS` is one of the five classes.

The long form covers everything the TOML can express:

```
cleanrs clean \
  --target-path ~/Projects \
  --target-marker package.json \
  --target-dir node_modules \
  --target-class network \
  --target-stale-after 30d \
  --dry-run

cleanrs clean \
  --target-cmd 'docker builder prune -f' \
  --target-class cheap \
  --target-requires docker \
  --apply
```

Ad-hoc targets compose with the manifest by default. `--only <id>...` and
`--exclude <id>...` select from the resolved set; `--targets-only-cli`
suppresses the manifest so the run is exactly what was typed.

The same specs work for `cleanrs targets` (inspect and size them without
cleaning), so a target can be measured before it is ever written to a file.
`cleanrs targets --export` prints the resolved set as TOML, giving a direct path
from an experimental CLI invocation to a persisted config entry.

---

## 8. The index

A cached scan makes the tool feel instant, and the index itself is a few MB for
millions of files — this does not contradict the project's purpose.

The correctness trap is worth stating explicitly:

> **Directory `mtime` is not a valid cache key for subtree size.** A directory's
> mtime changes when entries are added or removed, *not* when an existing file
> grows. Reusing a cached subtree because its dir mtime matched will silently
> under-report a 40 GB log file. Most naive incremental scanners get this wrong.

The sound approach on macOS is **FSEvents**:

- Persist the last event ID alongside the index.
- On re-scan, replay events `since` that ID, invalidate exactly those subtrees,
  and reuse the rest.
- Events are coalesced to directory granularity — which is the invalidation unit
  anyway.
- If the stored event ID predates the retained log (purged, wrapped, or the
  volume was restored from backup), the API reports this. **Fall back to a full
  scan.** Incremental is an optimisation that degrades to correct; it must never
  be a mode that silently lies.

Storage: an `rkyv`-serialised arena, `mmap`ed back in, mirroring the in-memory
layout for near-zero-cost load. (`redb` is the alternative if transactional
range queries turn out to be needed; start with the mmap'd arena.)

UI contract: index age is always visible ("index 4h old, 12k paths changed").
`--no-cache` bypasses it for one run; `cleanrs index --reindex` rebuilds
unconditionally; `cleanrs index clear` removes it.

---

## 9. Command surface

```
cleanrs scan [PATH]           One-shot analysis.
                              --json --apparent --allocated --freeable
                              --depth N --min-size --exclude
cleanrs tui  [PATH]           Interactive browser (ratatui).
cleanrs doctor                Reconcile df vs. scan vs. snapshots vs. inaccessible.
cleanrs targets               List detected targets with sizes.
                              --only --exclude --target... --export
cleanrs clean                 Reclaim space. Dry-run by DEFAULT.
                              --level safe|standard|aggressive
                              --apply --yes --permanent
                              --only --exclude --target...
cleanrs history               Audit log of past cleans.
cleanrs schedule install|remove|status
                              launchd agent for unattended runs.
cleanrs index [--reindex|--clear|--status]
```

Cross-cutting CLI rules:

- Dry-run is the default for anything destructive; `--apply` is always required.
- `--json` on every command, so cleanrs composes with other tooling.
- Meaningful exit codes for scripting (e.g. distinct codes for "nothing to do",
  "would reclaim N bytes", "partial failure").
- Shell completions (`clap_complete`) and a man page (`clap_mangen`) shipped.
- Respect `NO_COLOR`; detect non-TTY and degrade to plain output.

### 9.1 Scheduling

`cleanrs schedule install` writes a launchd `LaunchAgent` — this is what closes
the loop on the original problem. Default cadence:

- Weekly: `clean --level safe --apply --notify`
- Monthly: `clean --level standard --apply --notify`

Scheduled runs write to the same journal as manual ones and post a native
notification summarising bytes reclaimed. Anything above `standard` is never
scheduled.

---

## 10. Architecture

```
cleanrs-fs       Platform syscall layer.
                 macos: getattrlistbulk, statfs, volume/firmlink topology
                 linux: getdents64 + statx (later)
cleanrs-core     Arena tree, traversal engine, accounting, hardlink and
                 clone-family deduplication. Platform-agnostic.
cleanrs-index    FSEvents-backed incremental cache.
cleanrs-targets  Manifest parsing, CLI target specs, detection, reclaim planning.
cleanrs-tui      ratatui frontend.
cleanrs          CLI binary; wires everything together.
```

Keeping all platform specifics inside `cleanrs-fs` means Linux support later
reduces to one crate: `getdents64` + `statx`, `st_blocks`, reflink detection via
FIEMAP, and optionally `io_uring` to chase the same syscall-batching win.

### 10.1 Dependencies

| Need | Choice | Rationale |
|---|---|---|
| CLI | `clap` (derive), `clap_complete`, `clap_mangen` | Standard; free completions and man page. |
| TUI | `ratatui` + `crossterm` | The obvious choice. |
| Parallelism | `rayon` or `crossbeam-deque` | Work-stealing over a directory queue. **Not tokio.** |
| Syscalls | `libc` / `nix` + hand-written FFI | `getattrlistbulk` needs custom bindings. |
| Hashing | `rustc-hash` / `ahash` | Inode and clone-ID sets are hot; SipHash is wasted. |
| Index | `rkyv` + `memmap2` | Zero-copy load of the arena. |
| Config | `serde` + `toml` | Target manifests. |
| Deletion | `trash` | Native macOS Trash. |
| Errors | `anyhow` + `thiserror`, or `miette` | `miette` gives notably good CLI diagnostics. |
| Logging | `tracing` + `tracing-subscriber` | Span timings are needed for the perf work. |
| Progress | `indicatif` | Non-TUI progress. |
| Benchmarks | `criterion`, `hyperfine` | Micro and end-to-end. |
| Testing | `insta`, `tempfile`, `proptest` | Snapshot-test accounting against fixture trees. |

### 10.2 Known implementation gotchas

- `ATTR_CMNEXT_CLONEID` is observed at bit `0x100` in practice, not the `0x40`
  documented in Apple's headers. Ship a self-test that creates a real clone and
  a real copy and asserts the clone-ID behaviour end to end.
- `getattrlistbulk` returns variable-length packed records; the buffer walk is
  easy to get subtly wrong. This deserves dedicated unit tests with fixtures.
- Attribute availability differs by filesystem. Detect capability per mount and
  degrade to `readdir` + `lstat` where bulk attributes are unavailable, rather
  than failing.

---

## 11. Roadmap

Ordered so that the systems-heavy learning happens first and the weekly-utility
value lands in the middle.

1. **`cleanrs-fs` and a `scan` that prints one number.** Get `getattrlistbulk`
   working end to end. Benchmark against `du`, `diskus`, and `gdu` from day one.
2. **Correct accounting.** Hardlinks, sparse files, clone families credited at
   the lowest common ancestor, mount boundaries, firmlinks, permission errors as
   first-class data.
3. **`doctor`.** Cheap on top of step 2 and immediately the most useful command
   in the tool.
4. **Targets and `clean --dry-run`.** Manifest, CLI target specs, detection,
   sizing. Real day-to-day utility begins here.
5. **`clean --apply`.** Trash, journal, per-class gating, external command
   delegation, `history`.
6. **TUI.** Deliberately last. It is the most enjoyable part and the least
   load-bearing; building it against a stable core is far nicer than
   co-evolving the two.
7. **Index and FSEvents incremental scanning.**
8. **`schedule` / launchd integration.**
9. **Linux support** via `cleanrs-fs`.

---

## 12. Testing and benchmarking discipline

**Correctness.** Accounting is the product; it gets the strongest tests.

- Fixture trees built in the test suite covering: hardlinks, clones (created via
  `clonefile`), sparse files, symlinks, mount boundaries, unreadable
  directories, deep nesting, and very flat directories.
- Snapshot tests (`insta`) over the resulting tree and all three size metrics.
- Differential testing against `du` on trees with no clones or sparse files,
  where the two *should* agree — divergence there is a bug in cleanrs.
- A dedicated clone-ID self-test, given the header discrepancy noted in §10.2.

**Performance.**

- A fixed synthetic tree generator so numbers are comparable over time.
- `hyperfine` for end-to-end comparisons against `du`, `diskus`, `gdu`, `dust`.
- Warm-cache measurements only. There is no reliable way to drop the page cache
  on macOS; rather than pretend otherwise, measure warm and be consistent about
  it. Document this alongside every published benchmark.
- Track regressions in CI on a fixed tree; treat a >10% regression as a bug.

**Safety.**

- `clean` logic is tested against a sandboxed fixture tree, never the real
  filesystem.
- Every reclaim plan is asserted to be a no-op without `--apply`.
- Property test: no target resolution path, under any level, ever produces an
  `irreplaceable` action without the explicit opt-in flag.

---

## 13. Risks and open questions

- **FSEvents retention** is not guaranteed. The fallback-to-full-scan path is
  mandatory and must be exercised in tests, not merely written.
- **Clone-family LCA accounting** is the most complex piece of the design. It
  may need a second pass over the arena after the walk completes; the memory
  cost of holding clone IDs for every file needs measuring early.
- **macOS version drift.** Attribute layouts and firmlink topology are not
  stable API surface. Capability detection per mount, plus self-tests, is the
  mitigation.
- **Docker's data lives inside a VM.** On macOS, `Docker.raw` is a sparse image;
  reclaiming space inside it does not necessarily shrink the image on the host.
  cleanrs should explain this rather than report a misleading number.
- **Notarisation and distribution.** A CLI distributed via Homebrew avoids most
  of this, but Full Disk Access UX needs testing across install methods.
- **Should `scan` and `targets` share a single scan pass?** Probably yes
  eventually — targets are a narrow-scope instance of the same walk — but the
  first implementation may keep them separate for simplicity.

---

## 14. Glossary

- **Apparent size** — logical file length (`st_size`).
- **Allocated size** — blocks actually occupied (`st_blocks * 512`).
- **Freeable** — allocated blocks that would return to `df` if the path were
  deleted, with shared blocks credited once.
- **Clone** — an APFS file sharing blocks with another until modified.
- **Clone family** — the set of files sharing a clone ID.
- **Firmlink** — a bidirectional link stitching the System and Data volumes.
- **Purgeable** — space macOS reports as reclaimable on demand but which is
  currently occupied.
- **Target** — a declaratively described location or command that reclaims space.
- **Class** — a target's regeneration cost: `free`, `cheap`, `network`,
  `expensive`, `irreplaceable`.
- **Level** — how far a `clean` run is permitted to go: `safe`, `standard`,
  `aggressive`.
