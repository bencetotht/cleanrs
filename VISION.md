# cleanrs — Vision & Design

> **Status:** design reference with Phases 0–4 (including 4A) and Phase 5A implemented: filesystem and
> accounting foundations, read-only inventory/planning, guarded cleanup
> execution, persistent command-config trust, lightweight history, interactive cleanup,
> and project/Git/worktree hygiene with retention and artifact recommendations.
> Empirical results and unresolved gates are tracked in
> [docs/findings-phase0.md](docs/findings-phase0.md). This file remains the
> reference for what cleanrs is, why it exists, and how it should be built.
> The implementation roadmap now prioritizes fast developer-storage inventory,
> native cleanup orchestration, scheduling, indexing, and the remaining TUI
> overview work. Deeper APFS research is pursued when it enables those workflows rather
> than acting as a release gate.
>
> **Claim discipline:** every quantitative statement in this document carries a
> label — `[ref]` external reference, `[exp]` preliminary experiment, `[target]`
> a goal to design toward, `[hyp]` a hypothesis requiring validation. Sources
> are listed in §16. **Correction:** the earlier “no code/no measurements”
> statement is obsolete. Implementations and measured fixture baselines are
> recorded in DEVELOPMENT_PLAN.md and docs/; aspirational claims below retain
> their original labels.

---

## 1. Thesis

A directory walker can tell you how much space a file *occupies*. It cannot
reliably tell you how much space *deleting it will return*.

On APFS those are different questions. Blocks can be shared between cloned
files, pinned by snapshots, held by purgeable content, hidden behind protected
paths, or managed inside a provider's own storage (a Docker disk image, a
package manager's cellar). Every one of those breaks the assumption that a
subtree's size is what you get back.

cleanrs exists to answer the second question:

> **Where did my disk go, and how much do I actually get back if I delete this?**

It does so by combining three things no general-purpose analyzer does together:
APFS-aware per-file accounting, **reconciliation** against what the volume
actually reports, and **verification** of what a cleanup run really recovered.

Two values sit underneath everything:

- **Efficiency.** A full-system scan should be fast enough that you never avoid
  running it. The approach is to minimise syscalls, not to add threads.
- **Transparency.** The tool never deletes anything you did not see listed
  first. Dry-run is the default. Every destructive action is journalled with its
  estimated *and* observed effect. Estimates carry explicit confidence.

### 1.1 What `du` actually does

An earlier draft of this document claimed macOS `du` reports logical sizes. That
was wrong, and the correction matters because it defines the real differentiator.

BSD `du` on macOS reports **filesystem block usage** by default, deduplicates
**hardlinks** by default, and consequently handles **sparse files** reasonably.
`du -A` switches to apparent size. [ref: du.1]

So cleanrs's accounting advantage is *not* "we use blocks and `du` doesn't." It is:

- **Clone awareness** — `du` will happily attribute the same physical blocks to
  two cloned files.
- **Snapshot and purgeable awareness** — invisible to any walker.
- **Reconciliation** — explaining the gap between the walk and `df`, including
  coverage the scan could not achieve.
- **Provider integration** — space managed inside Docker, Homebrew, or Nix is
  not reclaimed by removing files behind their backs.
- **Verification** — measuring what a cleanup actually returned.

### 1.2 Non-goals

- A general-purpose file manager.
- A GUI. The TUI is the visual surface; anything richer is out of scope.
- Beating `gdu` on Linux. Linux support is portability, not competition.
- A consumer-grade "Mac cleaner." cleanrs targets developer-generated storage
  and should never present itself as anything broader.
- Heuristic deletion of user data. If cleanrs cannot explain in one line why a
  path is safe to remove, it does not remove it.

---

## 2. The macOS accounting hazards

These are what the core has to get right. Getting them right *is* the product.

**APFS clones.** Copying a file on APFS shares its blocks until one side
diverges. Xcode builds, Docker layers, `cp`, and Finder duplicates all produce
clones. Two files that each report 5 GB may occupy 5 GB in total; deleting one
frees nothing. Block-based tools including `du` attribute the full amount to
each.

**Divergence after cloning.** A clone ID identifies files that originated as
clones of the same data stream. It does **not** describe which extents remain
shared after the files diverge. Grouping by clone ID therefore identifies
*candidates* for sharing, not a proven partition of shared blocks. This is the
central reason §3 does not promise exact figures.

**Snapshots.** APFS local snapshots (Time Machine) pin blocks belonging to files
that have already been deleted or modified. Deleting a file covered by a
snapshot frees nothing until the snapshot expires. No directory walk can see
this.

**Purgeable space.** Space macOS reports as reclaimable on demand but which is
currently occupied. It is neither free nor attributable to a path.

**Firmlinks.** The read-only System volume and the writable Data volume are
stitched together by firmlinks, so a naive walk of `/` can traverse `/Users` and
`/System/Volumes/Data/Users` as two ordinary directories and produce a phantom
duplicate of the entire home directory. `getattrlistbulk` does report the
`SF_FIRMLINK` flag via `ATTR_CMN_FLAGS` [ref: getattrlistbulk.2], so this is
detectable — but device relationships across `/`, `/Users`,
`/System/Volumes/Data`, and `/System/Volumes/Data/Users` are non-obvious, and
`st_dev` comparison alone is **not** a sufficient strategy. Topology handling
must be driven by capability tests on real machines.

**Hardlinks.** Multiple entries, one inode, one set of blocks. `du` already
dedupes these; cleanrs must too, and must additionally distinguish "all links
are inside the scan scope" (deleting the scope frees the blocks) from "links
exist outside" (it does not).

**Cloud / File Provider storage — the most dangerous case in this list.**
iCloud Drive (`~/Library/Mobile Documents`) and everything under
`~/Library/CloudStorage` (Dropbox, Google Drive, OneDrive, and others all use
File Provider now) contain **dataless files**: large apparent size, near-zero
allocated. Three distinct hazards:

1. **Deletion propagates.** Removing a dataless file deletes it from the cloud
   and from every other device. It is user data wearing a local-cache costume.
   `--trash` does not save you, and there is no undo. The correct operation for
   reclaiming local space is **evict** (`brctl evict` / File Provider APIs),
   never delete.
2. **The realistic failure mode is an intersection, not a top-level path.** No
   one writes a target for `~/Library/CloudStorage`. But a perfectly ordinary
   detector — `marker = "package.json"`, `dir = "node_modules"` — fires happily
   inside a synced folder, and `.next`, `dist`, and `target` have the same
   exposure. A location deny-list is therefore necessary but **not sufficient**:
   every path a detector produces must additionally be checked for dataless /
   ubiquitous status at plan time, and refused.
3. **Materialization.** If traversal causes dataless files to download, cleanrs
   could pull hundreds of gigabytes while claiming to save space — a
   catastrophic and particularly ironic failure. Reading attributes via
   `getattrlistbulk` should not materialize content, but "should not" is
   insufficient assurance here. [exp: on 2026-08-28, a manually evicted 32 MiB
   iCloud Drive fixture retained `SF_DATALESS`, zero allocated blocks, logical
   size, and mtime across an initial traversal and five repeated scans. This
   verifies the tested iCloud case, not every third-party File Provider.]

Usefully, the metrics behave in the safe direction: dataless files have huge
apparent size and near-zero allocated, so anything ranked by allocated size
naturally ignores them. Only apparent-ranked output surfaces them, and it
surfaces them as fake wins — another reason allocated is the default ranking.

**Provider-managed storage.** Docker on macOS stores everything inside a sparse
disk image. Reclaiming space *inside* the VM does not necessarily shrink the
image on the host. Reporting the inner figure as recovered host space would be
misleading.

**Open files.** Unlinking a file that a running process still holds open
succeeds but returns no blocks until the last descriptor closes. Cleaning
`node_modules` under a running `next dev` is the everyday instance. This is both
a safety footgun and a named source of divergence between estimated and observed
reclamation (§11). [hyp: a targeted `proc_listpids` + descriptor sweep over the
user's own processes, restricted to a known path set, is cheap — plausibly well
under a second. `lsof` is slow because of what `lsof` does, not because the
underlying calls are. To be measured in Phase 0; if it holds, run it as a
pre-flight warning for direct-deletion targets rather than only as an
after-the-fact explanation.]

**Multiple volumes share one pool.** APFS volumes within a container share free
space. Swap lives on `/System/Volumes/VM`, Nix commonly on its own `/nix`
volume, and `System`, `Preboot`, and `Recovery` are separate volumes again.
Reconciliation is therefore **container-scoped, not volume-scoped** (§5); a
single-volume equation cannot balance and cannot see storage the design has
already committed to reporting.

**TCC / Full Disk Access.** An unsandboxed CLI without Full Disk Access receives
`EPERM` across parts of `~/Library`. Silently skipping those paths yields a
confidently wrong total. Critically, the scanner knows *where* and *how many*
such roots are, but **cannot know how many bytes they contain** — that is
exactly what it was denied.

---

## 3. The metrics

cleanrs reports five distinct quantities. Each has a stated contract. The
distinction is exposed in the UI rather than collapsed into one "size" column.

| Metric | Contract |
|---|---|
| **Apparent** | Exact logical length (`st_size`) of everything within scan coverage. |
| **Allocated** | Attributed allocation as reported by the filesystem (`st_blocks`). Sparse-correct. Comparable to default `du`. |
| **Immediately private** | Bytes the filesystem reports as freed immediately if **this file alone** were deleted — i.e. not trapped in a clone or snapshot. Derived from `ATTR_CMNEXT_PRIVATESIZE` [ref: getattrlist.2]. Per-file only; see §3.1 before summing it. |
| **Estimated reclaimable** | A group- or path-level **interval** bracketing what deletion would return, with a derived confidence and a one-line explanation. |
| **Actually reclaimed** | Observed after a cleanup: provider-reported figures plus measured container free-space delta (§5.1). |

**Estimated reclaimable is the headline.**

### 3.1 Group reclamation is superadditive — never sum private sizes

`PRIVATESIZE` answers "what if *this file alone* were deleted." That makes it
**superadditive over sets**: if two files share blocks only with each other,
those blocks appear in neither file's private size, yet deleting both frees
them. Formally, `reclaim(A ∪ B) ≥ reclaim(A) + reclaim(B)`.

Summing per-file private sizes therefore produces a lower bound, and it is worst
precisely where cleanrs is most useful — build trees such as `DerivedData`,
where the build system clones internally, or Bun's package installs, which clone
by design on macOS. A naive sum can report near-zero for a directory whose
removal would free gigabytes.

Compounding this, §2 establishes that a clone ID identifies files that
*originated* from the same stream, not which extents remain shared after
divergence. So the exact figure is not computable from available metadata at
all.

The response is to stop pretending it is a point value and **report an
interval**:

| Bound | Definition | Meaning |
|---|---|---|
| **Floor** | Σ per-file `PRIVATESIZE` over the set | What you get if every clone family has members outside the set. |
| **Ceiling** | Allocated, with each clone family counted **once** | What you get if every family is wholly contained in the set. |

The truth lies between, moving toward the ceiling as `ATTR_CMNEXT_CLONE_REFCNT`
matches the number of family members actually observed within scope. Hardlinks
are treated identically: all links inside the set pushes toward the ceiling, any
link outside pins toward the floor. Snapshot coverage pushes toward the floor
regardless.

### 3.2 Confidence is interval tightness, not a separate judgment

Confidence is not a hand-tuned label attached after the fact. It falls out of
the arithmetic:

| Level | Condition |
|---|---|
| **high** | Floor and ceiling agree within a small tolerance — typically no sharing at all, so the interval collapses to one number. |
| **medium** | A bounded interval: sharing exists, but both ends are derivable. |
| **low** | Either end is underivable — provider-managed storage, incomplete coverage from denied paths, or significant snapshot coverage. Report the range explicitly, or decline to give a figure. |

This has the property §3.1's problem demanded: for the common case — a
`node_modules`, a `.next`, most `target/` directories, none of which share
blocks with anything outside themselves — floor equals ceiling, and the tool
reports a single number at high confidence without special-casing.

The UI shows a single figure when the interval is tight, and the range when it
is not:

```
18.2 GiB reclaimable · high confidence

12.4–18.2 GiB reclaimable · medium confidence
Cloned blocks shared with paths outside this selection
```

`--explain <path>` prints which files or families widened the interval.

### 3.3 Accounting is a research task, not a solved one

**Exact freeable bytes is an aspiration, not a claimed capability.** Phase 0 of
the roadmap (§12) is a dedicated spike to determine what is actually derivable,
evaluating together:

- `ATTR_CMNEXT_PRIVATESIZE` — per-file immediately-private bytes
- `ATTR_CMNEXT_CLONEID` — clone family identity
- `ATTR_CMNEXT_CLONE_REFCNT` — how many references share the stream
- hardlink count versus links actually observed within scope
- snapshot enumeration and coverage, and how snapshot churn moves `PRIVATESIZE`
  for files that never changed (§10.3)
- dataless / ubiquitous file attributes, and whether traversal materializes them
- the cost of detecting open descriptors against a known path set
- **before/after volume capacity measurement as ground truth**

The last item is the arbiter. Any accounting model is validated by deleting a
known fixture set and comparing the prediction against the measured delta. Until
that harness exists and agrees, cleanrs reports estimates with confidence and
nothing stronger.

This also gives the design a feedback loop worth building deliberately: §11's
journal records estimate and observed delta side by side, so the model can be
calibrated against real runs over time rather than assumed correct.

---

## 4. The scanner

### 4.1 The cost model

The classic walk is `readdir()` for names plus one `lstat()` per entry.
`readdir()` is buffered and is not one kernel transition per entry; **the
per-entry `lstat` is the dominant cost**, and it is what `getattrlistbulk(2)`
eliminates.

`getattrlistbulk` returns many directory entries *together with their
attributes* in a single call, and avoids materialising a vnode per entry.
Published figures for a `getattrlistbulk`-based Rust walker versus `du -sh` on a
409,500-file tree: 3.33 s → 0.52 s, with profiling attributing the large
majority of remaining time to syscalls rather than to scheduling or locking
[ref: healeycodes]. Those are one author's measurements on one machine with a
warm cache; treat them as **motivation for the approach, not as targets cleanrs
has met**.

The decisive property for cleanrs is not raw speed but that **name, type,
`st_size`, allocated blocks, inode, link count, flags (including
`SF_FIRMLINK`), mtime, clone ID, clone refcount, and private size can all
arrive in the same bulk call.** APFS-aware accounting therefore costs no
additional syscalls. Speed and accuracy are the same optimisation. [hyp: the
full attribute set is available in one bulk request at acceptable cost — to be
measured in Phase 0, since requesting more attributes is not free.]

### 4.2 Concurrency model

- Work-stealing pool over a queue of **directories** (`rayon` or
  `crossbeam-deque`). Parallelism at directory granularity; bulk reads cover the
  within-directory case.
- Bounded in-flight directory count, tunable. [target: a default in the region
  of 64, chosen by measurement.]
- **Storage-aware defaults.** Parallelism may help on NVMe and hurt on
  rotational or network-backed mounts. `statfs` describes mount/filesystem
  properties but does not by itself reliably identify the physical medium;
  combine mount metadata with platform device APIs or provider hints, and fall
  back to a conservative auto setting. Worker counts are benchmark-derived.
- **Sharded inode / clone-ID sets.** A single shared set is the one place this
  design is likely to develop contention. [hyp]
- **No async runtime.** This is blocking syscall work; an async scheduler adds
  cost without benefit.

### 4.3 Traversal rules

- **Never cross mount boundaries by default.** `--cross-mounts` opts in.
- **Firmlink handling** uses the `SF_FIRMLINK` flag from `ATTR_CMN_FLAGS` *and*
  structural rules about volume-group topology. Neither alone is sufficient.
  Capability tests on real machines drive this, not assumptions.
- **Never follow symlinks.** Count the link entry, never its target.
- **Skip pseudo-filesystems and known traps**: `/dev`, `.fseventsd`,
  `.Spotlight-V100`, network mounts under `/Volumes` unless requested.
- **Record, never swallow, permission errors** — as *locations and counts*, not
  as bytes.
- **Degrade gracefully.** Attribute availability varies by filesystem; detect
  capability per mount and fall back to `readdir` + `lstat` where bulk
  attributes are unavailable.

### 4.4 Memory layout

An arena, not `Arc<Node>` trees:

- `Vec<Node>` indexed by `u32`, with `parent` / `first_child` / `next_sibling`.
- Names interned per directory into a flat byte buffer with `(offset, len)`.
- `u64` sizes; packed flags for kind, error state, and sharing state.

[target: bounded, measured memory per node — the per-node cost must be
established by measurement before any figure is published. Note that clone ID,
clone refcount, and private size each add per-node bytes, so accounting fidelity
and memory footprint trade against each other. §10 covers the on-disk
consequence.]

For context, `gdu` disables Go's garbage collector during analysis to stay fast
[ref: gdu]. Choosing the layout up front avoids that class of problem entirely.

---

## 5. `doctor` — reconciliation with confidence

A directory walk will never sum to `df`, and users read that as a bug. Make it a
feature — but **do not force every byte into a bucket**. Unexplained is a valid,
honest category.

### 5.1 Reconciliation is container-scoped

Free space belongs to the **APFS container**, not to any one volume. A
single-volume equation cannot balance, and worse, it cannot even see storage
this design has already committed to reporting: §7.3 requires `doctor` to
account for swap, which lives on `/System/Volumes/VM` — a different volume from
Data — and a Nix store commonly occupies its own `/nix` volume again.

`doctor` therefore enumerates every volume in the container and presents shared
free space as the single pool it actually is.

```
cleanrs doctor

  Container  disk3                              1.86 TB capacity
    Free (shared across volumes)                 153 GB
    Used                                        1.71 TB

  Volumes
    Data           /System/Volumes/Data         1.61 TB
    VM             /System/Volumes/VM             16 GB   swap, OS-managed
    System         /                             11 GB   read-only
    Nix            /nix                          68 GB
    Preboot, Recovery                            5.6 GB

  Data volume, attributed
    Files, clone-deduped allocated              1.31 TB   high confidence
      of which shared with paths outside scope    88 GB
    Unexplained                                  292 GB   not attributed

  Container-level, not attributable to paths
    Purgeable (reported by APFS)                  94 GB   ┐ these overlap:
    Snapshots                                 12 present  ┘ purgeable largely
                                                            includes snapshot space

  Coverage
    Inaccessible roots                                47   size unknown
    Scan coverage                             incomplete   Full Disk Access not granted
```

### 5.2 What the output deliberately refuses to do

- **It does not sum private sizes.** The headline attributed figure is
  **clone-deduped allocated** (§3.1's ceiling), because summing per-file
  `PRIVATESIZE` at volume scale counts every shared block zero times, which
  would both understate files and inflate the unexplained gap into something
  that looks alarming when it is mostly ordinary clone sharing.
- **It does not claim a size for inaccessible roots.** Their count and location
  are known; their contents are exactly what was denied.
- **It does not present purgeable and snapshots as disjoint rows that add.**
  Purgeable space largely *contains* snapshot space. Showing them as two flat
  entries invites exactly the wrong arithmetic, so containment is drawn
  explicitly.
- **It does not force the categories to balance.** Snapshots, purgeable space,
  container-level volume sharing, and ordinary files do not form a clean
  additive equation, and the layout must not imply one.

`doctor` is also where environment diagnostics live: Full Disk Access status
with the exact remediation path for the user's terminal binary, index freshness,
whether incremental scanning is viable, and which providers were detected or are
missing. It should give a useful first answer without requiring a full scan.

---

## 6. Target classification

### 6.1 One label for users, several properties internally

The five-class vocabulary is good for humans and bad as a data model. `target/`
and Xcode `DerivedData` can take hours to rebuild, so neither is meaningfully
"free"; Docker build cache may indirectly require network downloads; a model
weight is downloadable but very expensive; and a Docker volume is not the most
expensive cache — it is **user data**, a different kind of object entirely.

Internally, targets carry independent properties:

```
recoverability:    generated | downloadable | user_data
rebuild_cost:      trivial | moderate | high
network_required:  yes | no
confidence:        high | medium | low     # how sure we are it is safe to remove
automation:        unattended | confirmed | explicit-target-only
```

`recoverability = user_data` can never be selected by a level, at any setting.
It requires naming the target explicitly.

### 6.2 The label is an input with defaults

To keep the manifest authorable, targets are normally declared with a single
`class`, which expands to defaults; individual properties may then be
overridden. A contributor makes one judgment, not five.

| `class` | recoverability | rebuild_cost | network | automation |
|---|---|---|---|---|
| `free` | generated | trivial | no | unattended |
| `cheap` | generated | moderate | no | unattended |
| `network` | downloadable | moderate | yes | confirmed |
| `expensive` | downloadable | high | yes | confirmed |
| `irreplaceable` | user_data | — | — | explicit-target-only |

Examples of overriding: `class = "free"` with `rebuild_cost = "high"` for a
large Rust workspace's `target/`; `class = "cheap"` with `network_required =
true` for Docker build cache.

### 6.3 Levels

| Level | Selects | Cadence |
|---|---|---|
| `safe` | `recoverability = generated`, `automation = unattended` | Weekly, unattended |
| `standard` | `+ downloadable`, `automation ≤ confirmed` | Monthly, **proposed not applied** (§9.1) |
| `aggressive` | `+ rebuild_cost = high` | Manual only |
| — | `user_data` | **Never.** Explicit target naming only. |

**Correction:** an earlier roadmap example treated Homebrew downloads as a
standalone `safe` action. Homebrew's supported native `cleanup` command mixes
stale downloads with old installed versions. Until a supported narrower native
operation is available and validated, cleanrs assigns ordinary Homebrew cleanup
to `standard`, scrub to `aggressive`, and never schedules either as a
download-cache-only `safe` action.

---

## 7. Providers

The commands below began as the provider backlog. Phase 2 implemented read-only
detection, inspection, and planning for the developer-tool providers listed
here and in §7.3. Phase 3 now executes eligible planned commands only after
revalidating the executable and provider identity/context. Informational and
refused actions still have no executable capability.

| Provider | Starting point | Known complications |
|---|---|---|
| Homebrew | `brew cleanup --dry-run`; `brew autoremove --dry-run` | Implemented as distinct routine cleanup, scrub, and autoremove actions. Cleanup is run with implicit autoremove disabled; routine and scrub are mutually exclusive. |
| Docker build cache | `docker builder prune --filter until=336h` | Implemented per local Unix-socket context, with context and endpoint pinned. Provider-reported engine bytes do not imply host sparse-image reclamation. |
| Docker images / stopped containers | object-specific Docker prune commands | Implemented as separate actions; image cleanup is aggressive and never folded into build-cache cleanup. |
| Docker volumes | currently informational; separate anonymous/named cleanup planned in Phase 4B | **Correction (2026-10-14):** the earlier blanket ban on volume cleanup is superseded. Unused anonymous volumes get a confirmed native prune action, separate from explicitly selected unused named volumes. Named volumes remain `user_data`; no level selects them. Only volumes referenced by no running or stopped container qualify. |
| Nix | `nix-collect-garbage` | Implemented with plain GC at standard and `-d` at aggressive because the latter also destroys old generations and rollback capability. |
| Cargo | frozen `cargo metadata` plus workspace-pinned `cargo clean` | Implemented for explicit scan roots. Workspace members collapse by resolved target directory; custom/shared or out-of-scope directories are shown but refused until ownership is complete. |
| Xcode | known developer paths plus `xcrun simctl delete unavailable` | Implemented with whole-cache DerivedData explicit-ID-only because rebuild cost can be high, archives/device support/simulator data protected or informational, and unavailable simulators separate. |
| OrbStack | Docker context inventory plus OrbStack machine inventory | Docker actions are pinned to the OrbStack context; Linux machine disks remain informational user data. Never delete OrbStack backing files directly. |
| Docker Desktop | Docker CLI pinned to the Docker Desktop context; Desktop-supported reset/uninstall flows for decommissioning | A currently inactive context can still contain named volumes and container writable layers. “Stale installation” is not proof that its VM disk is disposable. |
| pnpm | `pnpm store path`, `pnpm store prune` | Implemented as a network-restored standard action. Experimental cache commands are not assumed. |
| npm / npx | `npm config get cache`, `npm cache verify` | Cache verification/GC is implemented. The separate npx cache is informational because the installed capability is not stable enough to plan. |
| Yarn / Bun | native cache-clean commands | Implemented as separate network-restored actions. Yarn detection deliberately does not invoke a Corepack shim that could download a tool merely to answer `--version`; Bun clone sharing remains a reclamation caveat. |
| Python managers | uv prune/clean, pip purge, Poetry cache, Conda dry-run/clean | Implemented with cache actions separate from Poetry/Conda environments, which remain informational user data. |
| Cargo / rustup shared state | registry/git cache and installed toolchain inventory | Informational until stable granular native cleanup and individual toolchain selection are available. |
| CocoaPods | `pod cache clean --all` | Implemented as a network-restored standard action. |
| VMware Fusion | Inspect `.vmwarevm` bundles; direct the user to Fusion's snapshot and disk-cleanup operations | A bundle is user data. Never delete `.vmdk`, snapshot, suspend, or lock files individually. Snapshot consolidation can require substantial temporary free space. |
| Claude Cowork VM | Inspect Claude's `vm_bundles` as an application-owned VM | The bundle may contain persistent Cowork/session state and there is no documented stable cleanup API. Report it separately; any reset/removal starts as explicit-target-only and requires Claude to be stopped plus a provider-version capability check. |
| macOS wallpaper cache | Version- and schema-validated direct-cache provider | Generated BMP renditions can be enormous, but source wallpapers must be distinguished from generated cache. Start as confirmed-only until regeneration and safe quiescing are validated on supported macOS versions. |
| macOS swap | `doctor` only | `/private/var/vm` is OS-managed transient state. Report current allocation and explain it; never delete swap files or run a pseudo-clean action. |

Phase 4A adds the following implemented system inventory:

| Provider | Operation / scope | Boundary |
|---|---|---|
| Gradle | configured caches, wrapper distributions and daemon storage | Network caches; confirmed cleanup only with successful daemon-state inspection. |
| Android SDK | SDK-pinned installed-package inventory and native uninstall | Images/build tools expensive; AVD state informational user data. |
| Metro / Expo / Watchman | exact temporary cache names, `.expo/cache`, native watch reset | Never target TMPDIR or Expo credentials; Watchman is confirmed, with unknown disk recovery. |
| Go | `go clean -cache` / `go clean -modcache` with pinned cache paths | Cheap build cache separate from network module cache; inspection cannot auto-download a toolchain. |
| Maven | `.m2/repository` | Network; refuse mixed repositories with unpublished local installs. |
| Playwright / Puppeteer | configured browser-download cache | Expensive/confirmed; downloads must be restored. |
| node-gyp / SwiftPM / TypeScript | exact headers/repository/typings caches | Native SwiftPM purge capability checked; unsupported native capability stays informational. |

System-only CLI `targets --system` / `clean --system` skip folder traversal;
known provider paths still receive metadata measurement and final safety checks.
TUI `S` opens a separate cached system list and returns to its origin with Esc;
`m` has been removed. Folder mode never silently includes system actions.

**Wallpaper implementation boundary:** only bounded discovery of the user-recalled
`extension-com.apple.wallpaper.extension.image` inside the agent container is
implemented, with installed-version evidence. VMware and Cowork are informational
user data. Wallpaper is also informational: no observed macOS version or validated
generated-cache schema/quiescence/regeneration evidence has been supplied. The
§7.3 size is historical motivation, not verified reclaimable cache or permission
to delete the extension tree. No macOS SDK claim was verified on this Linux host.

### 7.1 Providers are an interface, not a command string

Every **actionable** provider implements four operations:

- **inspect** — what exists, how much space it appears to hold, in what units,
  and whether that space is host-visible or provider-internal.
- **plan** — the concrete argv (or deletion set) that would run, with the
  provider's own estimate of what it would reclaim.
- **execute** — run it, capturing stdout/stderr and exit status.
- **verify** — re-inspect and report the provider's view of what changed,
  separately from the measured host volume delta.

`inspect` is the only mandatory capability. A provider for protected or
system-managed storage may deliberately return no plan or execution capability;
for VMware bundles, Claude Cowork state, simulator device data, and swap, that
is the safe result until a supported action and its preconditions are known.
Capabilities are versioned and discovered at runtime rather than inferred from
an executable merely existing on `PATH`.

This is what makes §3's "actually reclaimed" metric obtainable, and it is what
makes the Docker-on-macOS case (inner reclamation ≠ host reclamation) reportable
rather than misleading.

### 7.2 Direct deletion

Reserved for artifacts with no owning cleanup command — `DerivedData`,
`node_modules`, `.next`, `dist`, `__pycache__`. `rm -rf` on storage owned by a
provider corrupts that provider's state and leaves the user worse off.

### 7.3 Baseline known-location coverage

The following inventory came from the machine that motivated cleanrs. It is a
**product acceptance baseline**, not a set of hard-coded paths or size
thresholds. The figures are observations from one point in time, and the first
three rows overlap: `~/Library` and `~/Developer` are contained by the home
directory, so their sizes must never be added to the home total again.

Every row must appear in either `targets` or `doctor`; “handled” does not always
mean “deletable.” A result can be an ordinary scan scope, a provider cleanup
candidate, protected user data, or diagnostic-only system state.

| Observed area | Reported size | Required cleanrs handling |
|---|---:|---|
| Home directory | 295 GB | Primary scan scope and aggregation root. Never a cleanup target itself. Run detectors during the traversal and partition results into ordinary files, known targets, protected data, and unexplained/denied coverage. |
| `~/Library` | 146 GB | Scan and classify specific descendants. Never offer broad deletion of `~/Library`: `Caches` may contain candidates while `Application Support`, containers, backups, and app runtime state are presumptively user data until a provider says otherwise. |
| `~/Developer` | 86 GB | One scoped traversal with project/workspace detection. Recognize and deduplicate Cargo workspace `target` directories, `node_modules`, `.next`, `dist`, coverage, and similar generated output. Apply per-project staleness and rebuild-cost policy; never clean every matching directory merely because its name matches. |
| Cloud / File Provider storage | not in the baseline inventory | `~/Library/Mobile Documents` and `~/Library/CloudStorage` are a **hard deny-list for deletion**, and the deny-list alone is insufficient: any path produced by any detector must be checked for dataless/ubiquitous status at plan time and refused (§2). Report local allocated versus apparent so evictable content is visible, and surface **evict** as the space-reclaiming operation — never delete. Assert during traversal that no content is materialized. |
| VMware NixOS VM | 29 GB | Detect the `.vmwarevm` bundle and report apparent/allocated/private sizes plus recognizable snapshot/suspend components. The whole VM is `user_data`, explicit-target-only. Offer provider-guided disk compaction or snapshot management only after checking power state and required temporary free space; never delete bundle internals. This is not the Nix-store provider. |
| OrbStack | 29 GB | Detect OrbStack as the active Docker context and pin every inspect/plan/execute/verify call to that context. Inventory images, build caches, containers, and volumes separately; volumes and OrbStack Linux-machine disks are `user_data`. Never delete the host sparse image directly. Verify both engine-side change and host free-space delta. |
| Wallpaper cache | 24 GB | Detect the wallpaper agent's cache location by supported macOS version, validate container ownership and generated rendition schema, and report source images separately. The 1,410 BMP files are generated candidates, but cleanup starts `confirmed` rather than unattended until quiescing/regeneration behavior is proven. Journal them as an abnormal cache-growth finding. |
| Swap | 16 GB | `doctor` reports this as OS-managed, temporary storage with current allocation and memory-pressure context. It is never a target, and cleanrs never deletes `/private/var/vm/swapfile*`; closing memory-heavy workloads or rebooting is user guidance, not an executed cleanup. |
| Homebrew cache | 14 GB; provider estimated 3.9 GB reclaimable | Discover the configured cache through Homebrew, obtain a provider dry-run estimate, and keep routine cleanup distinct from scrub (`-s`). Execute through `brew`, respect pinned/retained artifacts, then record Homebrew's result and the host delta. Never infer 14 GB reclaimable from directory size. |
| pnpm store and cache | 12.5 GB | Discover configured locations (`pnpm store path` and configuration) rather than assuming defaults. Treat `pnpm store prune` as provider-owned removal of unreferenced, re-downloadable packages. Inspect metadata/dlx caches separately and capability-detect experimental cache commands. Network-restored, so not deleted by unattended `safe`. |
| Docker Desktop VM | 8 GB, possibly stale | Inventory it as a Docker context distinct from OrbStack. Inactivity is evidence for a decommission proposal, not permission to delete. If the daemon is available, enumerate containers, images, build cache, and especially named volumes; if unavailable, report the VM disk as opaque protected data. Full reset/uninstall cleanup is explicit-target-only and must disclose data loss. |
| npm cache and npx data | 7.4 GB | Resolve the configured cache with npm. Use `npm cache verify` for safe garbage collection; list and age npx entries separately where the installed npm supports `npm cache npx`. Full `npm cache clean --force` is a confirmed network-restored action, not routine raw deletion. |
| Claude Cowork VM bundle | 6.3 GB allocated; 10 GB apparent | Detect `~/Library/Application Support/Claude/vm_bundles/claudevm.bundle` and display sparse-aware metrics. Treat it as application-owned VM/session data, not a cache solely because it can be recreated. Until Anthropic exposes a stable cleanup/reset interface, removal is explicit-target-only, requires Claude stopped, warns that Cowork state may be lost and the image may be downloaded again, and is never scheduled. |
| Bun cache | 4.7 GB | Discover the configured path with `bun pm cache`; use `bun pm cache rm` rather than raw deletion. Classify as downloadable/network-required and verify APFS-aware host reclamation because Bun uses clones when installing packages on macOS. |
| Xcode data | 7.5 GB: 3 GB DerivedData, 4.5 GB simulator devices | Split into different targets. `DerivedData` is generated but potentially expensive to rebuild; remove only stale project subtrees or an explicitly selected whole cache. Unavailable simulator resources may use supported `simctl` cleanup, but simulator device/application data is `user_data` and never selected by a cleanup level. Downloadable simulator runtimes are a separate expensive target. |

This baseline also imposes four cross-provider requirements:

1. **No double counting.** Nested scan areas, APFS clones, pnpm links, Bun
   clones, and VM sparse images must remain distinguishable in both totals and
   estimates.
2. **Context pinning.** A cleanup plan stores the exact provider identity it
   inspected — including Docker context and endpoint — and aborts if that
   identity changes before execution.
3. **Detection without deletion is success.** VM state, volumes, simulator app
   data, swap, and synced cloud content are valuable findings even when cleanrs
   correctly refuses to remove them.
4. **Detectors are scoped, and scope is not enough.** A generic marker-based
   detector (`package.json` → `node_modules`) will match inside synced cloud
   folders, inside VM bundles, and inside provider-owned storage. Every
   candidate path passes the §8.2 invariants *and* the cloud/dataless refusal
   before it can enter a plan.

---

### 7.4 Project hygiene (Phase 4 implementation)

`projects [PATH]` inventories explicit-root repositories/worktrees and their
project-local artifacts. Git status distinguishes dirty/untracked, remote-less,
no-upstream, synchronized, unpushed, behind and diverged states; synchronization
is against local refs and never triggers a fetch. Source/manifest/dependency-
lockfile metadata, commit time, artifact activity and related worktrees support
or weaken inactivity recommendations. Directory mtime alone is insufficient.
Dormancy and artifact retention use separate thresholds; neither implies that
source may be deleted. Reports identify large inactive projects/worktrees but
create no repository-deletion action.

The inventory enriches the existing CLI/TUI planning pipeline. Dirty or unpushed
source can coexist with removable generated output, but tracked source inside
an artifact root refuses cleanup. Repository inspection is bounded and refuses
unknown/unsafe metadata; cloud/dataless state, symlinks and out-of-scope Git
admin references cannot silently authorize commands. Git inspection disables
fsmonitor, configured content filters, optional locks and lazy fetching and
removes inherited Git overrides; external config includes are refused.
Tracked-source protection is repeated before an artifact action.

Cargo output retains its observed workspace owners and completeness, is counted
once, and stays refused for incomplete shared/custom ownership. Native cleanup
revalidates metadata resolution and filesystem identity/safety. Project-local
Node dependency copies, Gradle cache, gitignored build output and Python
environments remain distinct from provider stores. **Correction:** the earlier
implementation classified project virtual environments as user data. Phase 4A
recognizes `pyvenv.cfg` under any directory name and assigns `network`, excluding
`safe` and disclosing manually installed packages. Generic build/output paths
require stored and freshly revalidated repository-local gitignore evidence.

Trusted user/explicit configuration accepts per-root/project `[[retention]]`
rules (`path`, `project_only`, `artifact_days`, `dormant_days`, `keep`). Exact
project policy wins, otherwise the most-specific root wins. `keep` is an
explicit-selection boundary even when age filtering is disabled. Recommendations
show intervals/confidence, supporting and weakening evidence, the exact proposed
operation and rebuild consequence; history preserves that evidence. Dependency
lockfile timestamps never prove process liveness. Richer TUI project panes,
indexing and scheduled maintenance remain Phase 5 work.

## 8. Reclaiming space safely

### 8.1 Trash does not reclaim storage

Moving a file to the Trash on the same volume relocates directory entries. It
does not return blocks to `df`. Making Trash the default for `--apply` would
mean a scheduled run could report "reclaimed 40 GB" while the disk stays full —
a direct contradiction of the product's core promise. A same-volume quarantine
directory has exactly the same problem, though it may still be useful for
delayed safety.

The rules:

- **`--apply` performs permanent deletion** for explicitly regenerable targets.
  That is the normal path.
- **`--trash` is an opt-in recovery mode.** Its accounting says
  *"moved, not yet reclaimed"* — never "reclaimed."
- **Scheduled runs never use Trash.**
- **Bytes moved to Trash and bytes reclaimed are separate journal fields**
  (§11), and the observed container free-space delta is recorded independently of
  both.

**Phase 3 implementation correction:** permanent `--apply` is implemented, but
`--trash` currently refuses and is deliberately deferred. Trash needs separate
same-volume/cross-volume movement and “moved, not reclaimed” accounting; it must
not borrow the permanent-deletion result path merely for convenience.

### 8.2 Path invariants for direct deletion

Dry-run is not protection against path-resolution bugs. These are mandatory,
enforced in code, and property-tested:

1. Expand `~`, then canonicalize the nearest existing ancestor.
2. Reject empty paths, `/`, the home directory itself, and any mount root.
3. Marker-based artifact paths must remain strictly beneath the detected project
   root after normalization.
4. Never follow symlinks during recursive deletion; use `openat`-style relative
   descent.
5. Revalidate device, inode, and file type immediately before applying — the
   filesystem may have changed since the scan.
6. Detect replacement between scan and deletion (inode or type mismatch) and
   abort that target.
7. Refuse paths that cross a mount boundary unless explicitly allowed.
8. Bound total unattended deletion per run; exceeding it requires confirmation.
9. `children_only` must never resolve, after normalization, to a path broader
   than the declared target.
10. **Refuse any path under `~/Library/Mobile Documents` or
    `~/Library/CloudStorage`, and refuse any individual file carrying dataless
    or ubiquitous status**, wherever it lives (§2). This check runs on candidate
    paths at plan time, not only on target roots, because the hazard arrives via
    ordinary detectors matching inside synced folders.

### 8.3 Open files

Unlinking a file held open by a running process returns no blocks until the last
descriptor closes. The estimate will be right and the observed delta will not,
and a developer machine hits this routinely — `node_modules` beneath a running
dev server is the standard case.

Handling, in order of preference pending the Phase 0 measurement in §2:

- **If the targeted descriptor sweep proves cheap**, run it as a pre-flight
  check on direct-deletion targets and warn — naming the holding process —
  before applying.
- **Regardless**, treat it as a named, expected discrepancy class in the
  journal. When `container_free_delta` falls well short of the estimate, open
  descriptors are among the first explanations `history` should offer, alongside
  snapshot coverage and clone sharing.

This is deliberately not a blocking check. cleanrs should explain the shortfall,
not refuse to work while any process is running.

### 8.4 Other mechanics

- **Dry-run is the default.** `clean` plans and prints; `--apply` executes.
- **Never run as root by default.** Refuse with an explanation absent an
  explicit override.
- **Print exact argv** before running any external command; require `--yes` for
  unattended execution.
- **Journal everything** (§11).

### 8.5 Staleness

`atime` is unreliable on APFS — updates are lazy and it is a poor proxy for
recent use. Use instead the `mtime` of the project marker (`Cargo.toml`,
`package.json`, `go.mod`), the `mtime` of the artifact directory, and any better
ecosystem-specific signal (e.g. `cargo sweep`'s toolchain/fingerprint dating).
Targets declare `stale_after`; `--ignore-staleness` overrides.

---

## 9. Targets: configuration and CLI

Targets are declarative. A curated default set ships embedded in the binary;
users extend or override it; and **any target expressible in configuration is
also expressible entirely on the command line**, so cleanrs is fully usable with
no config file.

### 9.1 Trust boundary — this is a security boundary, not a convenience

Targets can contain executable commands. Therefore **cleanrs never
automatically loads configuration from the current working directory.** Cloning
an untrusted repository and running `cleanrs clean --apply` must not execute
commands supplied by that repository.

Rules:

1. **No implicit `./cleanrs.toml`.** Project-local configuration is loaded only
   when named explicitly with `--config <path>`.
2. **A file loaded via `--config` that declares command targets requires a trust
   decision**, recorded against the file's canonical path *and* content hash.
   Editing the file re-prompts.
3. **Optionally, project-local files may be restricted** to path detectors with
   no `run` targets at all — the safest default, and the recommended starting
   position.
4. **Scheduled runs never load current-directory configuration** and never
   consult the trust store interactively. They use an explicitly pinned config
   path only.
5. **Commands are argv arrays, executed directly. There is no shell.** No
   string form is ever parsed into arguments, in configuration or on the
   command line.

**Phase 2 implementation boundary:** the implicit user config is trusted by its
fixed user-owned location. An explicitly named config containing a command
target is rejected unless that invocation also supplies
`--trust-config-commands`; no configured command can execute in Phase 2.
Persistent canonical-path plus content-hash trust is intentionally deferred to
Phase 3 and is required before configured command execution is enabled. This
corrects the earlier implication that the persistent trust store already had to
exist for read-only plan construction.

**Phase 3 implementation:** explicitly named command configs are trusted by
canonical path plus SHA-256 content using `cleanrs trust-config`; editing or
replacing a file invalidates the record. Trust can be listed or revoked.
`--trust-config-commands` records the same persistent decision as a convenience,
not a per-run bypass. Parsed command bytes are checked against the trusted hash.

Resolution order, later overriding earlier by `id`:

1. Embedded default manifest.
2. `~/.config/cleanrs/targets.toml`.
3. A file named explicitly by `--config`, subject to the trust rules above.
4. CLI flags.

`--no-builtin-targets` drops step 1.

### 9.2 The target model

| Field | Meaning |
|---|---|
| `id` | Stable identifier, e.g. `xcode.derived-data`. Used by `--only` / `--exclude`. |
| `detect` | An existing path, a marker file found by scan, or an available command. |
| `class` | Shorthand expanding to the §6.1 properties. |
| `recoverability`, `rebuild_cost`, `network_required`, `automation` | Future optional overrides; Phase 2 expands `class` atomically instead. |
| `reclaim` | `delete` (with `children_only`) or `run` (an argv array). |
| `stale_after` | Minimum age before eligibility. |
| `scope` | Roots to search, for marker-based targets. |

### 9.3 Configuration form

```toml
[[target]]
id = "xcode.derived-data"
detect = { path = "~/Library/Developer/Xcode/DerivedData" }
class = "expensive"
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
class = "network"
reclaim = { method = "run", argv = ["docker", "builder", "prune", "-f"] }
```

`argv` is an array. There is no string command form anywhere in the format.

### 9.4 CLI form

Path targets use a compact spec:

```
cleanrs clean --target '~/Library/Developer/Xcode/DerivedData:free:delete-children'
cleanrs clean --target '~/Downloads/sdks:expensive:delete' --level aggressive
```

Grammar: `PATH:CLASS[:METHOD]`, `METHOD` ∈ {`delete`, `delete-children`}. Paths
containing `:` must currently use a configuration file.

An expanded flag-per-field long form remains a future ergonomics improvement;
Phase 2 uses `--config` for marker declarations. It does not currently accept
the following aspirational form:

```
cleanrs clean \
  --target-path ~/Projects \
  --target-marker package.json \
  --target-dir node_modules \
  --target-class network \
  --target-stale-after 30d \
  --dry-run
```

Command targets are given as an explicit argv array, never a shell string:

```
cleanrs clean \
  --target-argv '["docker","builder","prune","--filter","until=168h"]'
```

Phase 2 assigns ad-hoc command targets the `cheap` class; use a config target
when another class, stable ID, or detector is required. In Phase 3 the argv
array remains inert without `--apply`; with apply it is shown in the reviewed
plan, pinned to the executable identity, and executed directly without a shell.

The trust boundary in §9.1 concerns *files*, not what the user types. A command
the user enters at their own prompt is theirs; the JSON array form exists to
remove any possibility of shell evaluation or argument-splitting ambiguity, not
to gate the user.

Ad-hoc targets compose with the manifest by default. `--only <id>...` and
`--exclude <id>...` select from the resolved set; `--targets-only-cli`
suppresses the manifest entirely. The same flags work with `cleanrs targets`, so
a target can be inspected and sized before it is ever written to a file, and
`cleanrs targets --export` emits the resolved set as TOML.

---

## 10. The index

A cached scan makes the tool feel instant. Three constraints shape it, and the
governing invariant is:

> **The index caches the tree, not the verdict.**

Durable per-file facts — apparent size, allocated size, inode, link count, clone
ID, mtime — are properties of the file and may be cached. Anything derived from
volume-global state — private size, the reclaimable interval, confidence — is a
*conclusion*, and conclusions go stale for reasons the file itself never
witnessed.

### 10.1 Directory mtime is not a valid cache key

A directory's mtime changes when entries are added or removed, **not** when an
existing file grows. Reusing a cached subtree because its dir mtime matched will
silently under-report a 40 GB log file. Many naive incremental scanners get this
wrong.

### 10.2 FSEvents gives a conservative dirty set, not an exact one

The contract:

> Determine a **conservative dirty-root set** from events; rescan every dirty
> root in full; and **invalidate the entire index whenever completeness cannot
> be proven.**

Specifics:

- Events are coalesced, so directory granularity is the working unit.
- Renames and moves require care — a moved subtree dirties both endpoints.
- Mount changes, volume replacement, and restore-from-backup invalidate
  wholesale.
- If the stored event ID predates the retained log (purged or wrapped), the API
  reports this [ref: FSEvents]; **fall back to a full scan.**
- **Event state is stored per volume**, not as one global stream.

Incremental scanning is an optimisation that degrades to correct. It must never
be a mode that silently lies. The fallback path is exercised in tests, not
merely written down.

### 10.3 Snapshots are a staleness channel FSEvents cannot see

FSEvents is not the only way cached data goes stale. **Snapshot creation and
expiry change `PRIVATESIZE` for files that experienced no filesystem event at
all.** A file untouched for months can move from "0 bytes private, pinned by a
snapshot" to "8 GB private" the moment that snapshot expires, and no event
fires. Cached reclaimability can therefore rot hourly with a perfectly valid
event stream. Directory mtime is not the only invalid cache key here.

Recomputing private size on read is the correct answer semantically but defeats
the index: it is a full re-stat, which is exactly what the cache existed to
avoid. The workable compromise:

- **Tag cached private sizes with a snapshot generation token** — the snapshot
  set identity and count for the volume, cheap to obtain. A token mismatch marks
  those figures *degraded*, downgrading confidence rather than invalidating the
  whole index.
- **Recompute lazily and locally.** Only the subtree the user actually opens or
  targets is re-derived, not the whole tree.
- **Estimates in a plan are always re-derived, never read from cache.** Browsing
  may show slightly stale figures; a cleanup plan never may.

That last rule is worth holding independently of snapshots. It also covers
ordinary drift between scan time and apply time, and it pairs naturally with the
§8.2 revalidation invariants — the plan re-reads the world it is about to
change.

### 10.4 Size and format are open questions

An earlier draft claimed the index would be "a few MB" for millions of files,
which contradicted the in-memory arena estimate in the same document. Both
figures were unmeasured. The honest position:

- **[target]** Establish a bounded-storage goal *after* measuring a real scan of
  a real machine. Per-node cost must be measured with the full attribute set
  (clone ID, refcount, private size), which is what dominates.
- A directly `mmap`-usable arena and dense compression are largely mutually
  exclusive. Which matters more depends on whether load latency or footprint
  turns out to be the binding constraint — that is a measurement, not a
  preference.

`rkyv` + `memmap2` is a **candidate**, not a decision, to be benchmarked against
a compact custom format, compressed serialization, SQLite, and `redb` on
realistic scans. Whatever wins must provide:

- schema and version headers, with migration or automatic invalidation
- volume identity, scan root identity, and configuration hash
- corruption validation **before** any access — archived bytes from disk are
  untrusted input and must never be dereferenced unchecked
- architecture and endianness handling
- atomic replacement on write
- size bounds and eviction
- **restrictive file permissions: a path index is privacy-sensitive**, recording
  project names, client names, and directory structure

UI contract: index age is always visible. `--no-cache` bypasses for one run;
`cleanrs index --reindex` rebuilds; `cleanrs index --clear` removes.

---

## 11. The journal

"Bytes reclaimed" is not one number. Each run records:

| Field | Meaning |
|---|---|
| `bytes_selected` | What the plan targeted (allocated size of the selection). |
| `estimated_floor` / `estimated_ceiling` | The reclaimable interval (§3.1) and its derived confidence. |
| `provider_estimated` | The provider's own pre-run estimate, where available. |
| `provider_reported` | What the provider says it reclaimed. |
| `container_free_delta` | Measured free-space change on the **container**, since that is where free space actually lives (§5.1). |
| `bytes_trashed` | Moved but **not** reclaimed. |
| `bytes_evicted` | Cloud content evicted rather than deleted — local space returned, remote copy intact. |

`container_free_delta` is **observed evidence, not attribution** — other
processes allocate and free space concurrently, and snapshot expiry is
asynchronous. It is reported as such, never as a precise figure for what cleanrs
did.

Together these fields are the calibration loop for §3.3: over many runs, where
the observed delta falls within `[floor, ceiling]` is the measurement that tells
us whether the accounting model is any good. An observation *below* the floor is
especially informative — it means something held blocks that the model believed
were private.

### 11.1 Named discrepancy classes

When the observed delta falls short of the estimate, `history` should offer the
known explanations rather than leaving the user to guess:

- **Open descriptors** — blocks not returned until the holding process exits
  (§8.3).
- **Snapshot coverage** — blocks pinned until the snapshot expires.
- **Clone sharing outside the selection** — the interval's floor was the honest
  answer.
- **Provider-internal reclamation** — space freed inside a VM disk image without
  the host image shrinking (§2).
- **Concurrent activity** — another process allocated during the run.

The journal also needs **rotation** and, because it records private directory,
project, and client names, an option for **path redaction** (hashing or
truncating paths while retaining sizes and target ids).

**Phase 3 implementation boundary:** the private rotating JSONL journal records
a durable pending snapshot before mutation and a newer snapshot after each
action. It currently stores plan estimates, removed tree allocation,
provider-reported output when parseable, exact argv, outcome, exit status, and
bounded stdout/stderr summaries. `history` collapses snapshots by run ID. It
does **not** yet measure or attribute `container_free_delta`, and path redaction
remains future privacy work; neither absent field is synthesized.

---

## 12. Roadmap

**Phase 0 — Accounting research spike.** Before designing around any accounting
model, determine empirically what is derivable. Specifically:

- The behaviour of `PRIVATESIZE`, `CLONEID`, `CLONE_REFCNT`, and link counts
  across clones, divergence, snapshots, and hardlinks — and in particular
  whether the §3.1 floor/ceiling interval actually brackets reality.
- How snapshot creation and expiry move `PRIVATESIZE` for untouched files
  (§10.3), and how cheaply a snapshot generation token can be obtained.
- **Dataless-file safety**: scan a folder of evicted iCloud/CloudStorage
  content and assert zero bytes downloaded and zero change in local allocation
  (§2). This gates any traversal of a home directory.
- The cost of an open-descriptor sweep against a known path set (§8.3).
- The real cost of requesting the full attribute set in one bulk call.
- A fixture harness that deletes known trees and compares predictions against
  measured **container** deltas.

**Output: a written statement of what cleanrs can and cannot promise.**
Everything downstream depends on it.

1. **Platform layer and a `scan` that prints one number.** `getattrlistbulk` end
   to end, benchmarked against `du`, `diskus`, `gdu` from day one.
2. **Accounting.** Hardlinks, clone families, the reclaimable interval, snapshot
   detection, mount boundaries, firmlinks, dataless-file refusal, permission
   errors as counted locations. Confidence derivation per §3.2.
3. **`doctor`.** Cheap on top of step 2, and immediately the most useful command.
   Container-scoped from the start (§5.1) — retrofitting volume-scoped output
   would mean rewriting it.
4. **Targets and `clean --dry-run`.** Manifest, CLI specs, detection, sizing.
5. **Providers.** inspect/plan/execute/verify for the complete §7.3 acceptance
   baseline: Homebrew; Docker Desktop and OrbStack as distinct contexts; Cargo;
   pnpm, npm/npx, and Bun; Xcode; VMware; Claude Cowork; and the validated
   wallpaper cache. Swap remains a `doctor` diagnostic, not a provider action.
6. **`clean --apply`.** Path invariants, permanent deletion, journal, `history`.
7. **TUI.** Deliberately late — most enjoyable, least load-bearing, and far
   nicer to build against a stable core.
8. **Index and incremental scanning.**
9. **`schedule` / launchd.**
10. **Linux support.**

---

## 13. Command surface

```
cleanrs scan [PATH]           One-shot analysis. Ranked by allocated by default.
                              --json --apparent --allocated --private --reclaimable
                              --depth N --min-size --exclude --explain
cleanrs tui  [PATH]           Browse storage, select targets, review and run cleanup.
                              Working directory default; y on review confirms.
cleanrs doctor                Reconcile container state; report unexplained gap.
                              --container <id> --all-containers
cleanrs projects [PATH]       Explain project/Git activity and artifact recommendations.
cleanrs targets               List detected targets with sizes and confidence.
                              --only --exclude --target... --export
cleanrs clean                 Reclaim space. Dry-run by DEFAULT.
                              --level safe|standard|aggressive
                              --apply --yes
                              --trash (reserved; currently refuses)
                              --only --exclude --select PATH --target... --config
cleanrs history               Journal of plan decisions, outcomes, removed
                              allocation, provider reports, and errors.
cleanrs trust-config PATH     Trust/list/revoke explicit command config content.
cleanrs schedule install|remove|status
cleanrs index [--reindex|--clear|--status]
```

Cross-cutting rules: dry-run default for anything destructive; meaningful exit
codes for scripting; shell completions and a man page shipped; `NO_COLOR`
respected. Ordinary CLI reports offer `--json` and non-TTY plain output.
**Correction (Phase 5A):** the interactive `tui` command requires a terminal
on stdin/stdout, has no JSON mode, and uses explicit `y` review confirmation
in place of the CLI’s `--apply`. It never implicitly starts at the home directory.

Filesystem scans have no knowable total until traversal completes, so interactive
`scan` and scan-backed diagnostics show an **indeterminate** progress indicator
with monotonic directory, entry, and allocated-byte counters rather than a fake
percentage. Progress is written only to an interactive stderr and is suppressed
for JSON and redirected output. Human results use metric-aware tables and
semantic colors; redirected results use colorless ASCII tables.
For `targets` and `clean`, the default human table is a decision overview:
reclaimable size, project, and location come first, followed by compact age and
selection information. Paths retain their complete home-relative context (for
example `~/Developer/Projects/example/target`) rather than becoming ambiguous
scan-root-relative fragments. Exact argv, consequences, provider identity, and
full evidence belong to JSON rather than being wrapped into every terminal row.

`clean --select PATH` is explicit naming of individual detected candidates.
Repeated selections use exactly the named candidates, even if severity,
staleness, or explicit-only policy would otherwise skip them. Safety refusals
remain absolute. Plans and history mark the manual choice and overridden policy
reason; user-data/explicit-only actions still require interactive confirmation.
Command candidates accept the exact `candidate_key` from plan JSON.

`cleanrs tui [PATH]` links a progressive browser (`T` opens scoped targets)
with per-item selection (Space, ranges, all/none/invert, level presets), review
(`x`), execution (`y` only), and results. `T` on a recognized target directory
uses its containing folder and focuses that target. Machine-wide providers
are opt-in with `m`; refused/informational rows cannot be ticked. Live selected
intervals use cached detection arenas and the shared plan-summary computation;
completed browser arenas can supply discovery facts, preserving their original
age. Explicit refresh scans fresh; incomplete browser data or changed child
identities fall back to scanning. `x` rebuilds through the authoritative pipeline,
walking selected candidate trees and ancestor listings, with progress and names any
selected candidates dropped after identity/operation changes or new refusals.
No cached arena bypasses mutation revalidation or journalling (§8.2).
The targets table shows project paths relative to its scope, with exact operations
in the detail pane. Review shows counted policy groups, numbered actions,
confidence, consequences and an explicit permanent-deletion confirmation.
Session caches evict oldest completed arenas and all their aliases: browser
storage is capped at 250,000 nodes/4,096 aliases and 32 listings/250,000 inactive rows; target detection
at eight scopes/250,000 nodes. Active views, immutable reviewed plans and
in-flight work may exceed cache limits, and are released when no longer needed.
**Correction to the initial Phase 5A behavior:** detection/review no longer
unconditionally rescan the full scope, and session caches are no longer unbounded.


**Correction to the post-M5 session-cache description:** completed browser
filesystem facts now persist across TUI restarts in the fixed user cache location
(`$XDG_CACHE_HOME/cleanrs/tui-v1`, absolute only, or `~/.cache/cleanrs/tui-v1`).
The owner-only, atomic, checksummed, versioned binary cache is bounded at 1 GiB/1,024 entries
and 256 MiB per entry. It stores metadata/parent indices/names, never cleanup
verdicts or executable plans. Restored facts retain their age and a cached label;
root identity/mtime changes and invalid formats fall back to scanning. Age is
kept separately from the monotonic clock so rebooting cannot reset freshness. It is a
static snapshot cache: deeper changes require explicit refresh until Phase 5's
incremental invalidation is implemented. `r` bypasses cached facts; mutation
cancels overlapping browser jobs and invalidates their disk entries before
execution. Cleanup review/execution continue to revalidate live paths.
Arena eviction now releases detailed row references independently of completed
parent listings, preserving measured sizes on return. Workers release temporary
inode maps when complete. Quit from the browser asks for `y`; Esc, Enter or `n`
keeps the session open. Back from other screens and cleanup stop behavior remain
context-sensitive.

Every final TUI tick is manual explicit naming: history’s `[manual]` means
“policy-eligible, picked by hand”, while `[manual: <reason>]` records an override.
Browser and detection ages use s/m/h and warn after ten minutes with a refresh
hint. Every screen has `?` help, NO_COLOR retains semantic symbols, and the
minimum layout is 80×20. Dismissable errors are logged to the fixed state
location (`~/.local/state/cleanrs/tui.log`, or `$XDG_STATE_HOME/cleanrs/tui.log`).
Results label removed allocation and provider reports separately; returning to
the browser refreshes affected directories. The README contains the keymap and
walkthrough for cleaning only selected items.

`scan` ranks by **allocated** size by default. This is not only the honest
default — it is also a safety property: dataless cloud files have enormous
apparent size and near-zero allocation, so apparent-ranked output would float
them to the top of every listing as attractive, catastrophic targets (§2).
`--apparent` remains available and is labelled accordingly.

### 13.1 Scheduling

`cleanrs schedule install` writes a launchd `LaunchAgent`. Defaults:

- **Weekly:** `clean --level safe --apply --yes --notify`. Unattended, permanent
  deletion, generated artifacts only.
- **Monthly:** `clean --level standard --notify`. **Proposes a plan; does not
  apply it.** Network-restored artifacts such as `node_modules` are not removed
  without a human. Users may opt in to unattended `standard` explicitly.
- Nothing above `standard` is ever scheduled.

Scheduled runs use a pinned configuration path, never the working directory
(§9.1), never use Trash (§8.1), and write to the same journal as manual runs.

---

## 14. Architecture

Start with two crates. Extract more when a platform or compile-time boundary
becomes real, not before — premature fragmentation slows early iteration.

```
cleanrs-core     Library. Modules: fs, accounting, targets, providers,
                 execution, index, journal.
cleanrs          Binary. CLI, TUI, scheduling.
```

`fs` keeps platform code behind a trait from the start, so extraction later is
mechanical. But note that **Linux support is not confined to that module**:
scheduling (launchd vs. systemd timers), filesystem events (FSEvents vs.
inotify/fanotify), Trash semantics, default paths, permission models, mount
topology, and provider availability are all platform-sensitive. Plan for
platform seams in several modules, not one.

### 14.1 Dependencies

| Need | Choice | Rationale |
|---|---|---|
| CLI | `clap` (derive), `clap_complete`, `clap_mangen` | Standard; free completions and man page. |
| TUI | `ratatui` + `crossterm` | The obvious choice. |
| Parallelism | `rayon` or `crossbeam-deque` | Work-stealing over a directory queue. Not tokio. |
| Syscalls | `libc` / `nix` + hand-written FFI | `getattrlistbulk` needs custom bindings. |
| Hashing | `rustc-hash` / `ahash` | Inode and clone-ID sets are hot. |
| Index | Undecided — benchmark `rkyv`+`memmap2`, custom, `redb`, SQLite | See §10.4. |
| Config | `serde` + `toml` | Target manifests. |
| Errors | `anyhow` + `thiserror`, or `miette` | `miette` gives good CLI diagnostics. |
| Logging | `tracing` + `tracing-subscriber` | Span timings for perf work. |
| Progress | `indicatif` | Non-TUI progress. |
| Benchmarks | `criterion`, `hyperfine` | Micro and end-to-end. |
| Testing | `insta`, `tempfile`, `proptest` | Snapshot and property tests. |

`trash` may become a dependency only when the deferred `--trash` mode implements
truthful moved-not-reclaimed semantics; Phase 3 does not include it.

### 14.2 Implementation notes

- `ATTR_CMNEXT_CLONEID` is `0x00000100` in the current SDK; `0x40` is
  `ATTR_CMNEXT_REALDEVID` [ref: attr.h]. An earlier draft repeated a claimed
  discrepancy with Apple's documentation; that claim is withdrawn. Bind against
  the SDK headers and keep the end-to-end self-test — creating a real clone and
  a real copy and asserting observed behaviour — because the *semantics* still
  need validating even though the constant is not in doubt.
- `getattrlistbulk` returns variable-length packed records; the buffer walk is
  easy to get subtly wrong and deserves dedicated fixture-based unit tests.
- Requesting more attributes is not free. Measure the marginal cost of the
  extended set in Phase 0 and consider a two-tier request (cheap set for
  traversal, full set only where accounting needs it).

---

## 15. Testing and benchmarking

**Correctness.** Accounting is the product; it gets the strongest tests.

- Fixture trees covering hardlinks, clones created via `clonefile`, clones that
  have since **diverged**, sparse files, symlinks, mount boundaries, unreadable
  directories, deep nesting, and very flat directories.
- **A mutual-clone fixture**: two or more files that clone *only each other*,
  where every per-file `PRIVATESIZE` is ~0 but deleting the whole set frees
  everything. The estimate must produce a ceiling equal to the real reclamation,
  not a floor of zero. This is the direct regression test for §3.1's
  superadditivity, and the case a naive implementation gets catastrophically
  wrong.
- Snapshot tests (`insta`) over the tree and all reported metrics.
- Differential testing against `du` on trees with no clones, where the two
  *should* agree on allocated size — divergence there is a cleanrs bug.
- **Ground-truth harness**: delete a fixture set, measure the real container
  delta, and assert it falls within `[floor, ceiling]`. This is the only test
  that validates the headline metric. A result below the floor is a hard
  failure, not a tolerance issue — it means the model believed blocks were
  private when they were not.
- **Snapshot-churn test**: take a snapshot, observe `PRIVATESIZE` collapse for
  untouched files, delete the snapshot, observe it return — with no filesystem
  events fired throughout. This is the §10.3 staleness channel made executable.
- Clone semantics self-test per §14.2.

**Performance.**

- A fixed synthetic tree generator so numbers are comparable over time.
- `hyperfine` against `du`, `diskus`, `gdu`, `dust`.
- **Warm-cache measurements only.** There is no reliable way to drop the page
  cache on macOS; measure warm, be consistent, and document this alongside every
  published number.
- Track regressions in CI on a fixed tree.

**Safety.**

- `clean` logic runs only against sandboxed fixture trees.
- Every plan is asserted to be a no-op without `--apply`.
- Property tests over the §8.2 path invariants, including adversarial inputs:
  symlinked ancestors, `..` traversal, paths replaced between scan and apply.
- Property test: no target resolution, at any level, ever yields an action
  against `recoverability = user_data` without explicit naming.
- Trust-boundary test: a repository containing `cleanrs.toml` with a `run`
  target must not execute it under any invocation that does not name it via
  `--config`.
- Provider identity test: a Docker plan inspected against OrbStack remains
  explicitly pinned to the captured context even if the process-wide default
  changes to Docker Desktop. It aborts if that captured endpoint disappears or
  resolves to a different daemon identity; it must never fall back to the new
  default context.
- Acceptance tests cover every row in §7.3. Destructive provider tests use
  fixtures or disposable instances; protected rows assert that no cleanup plan
  is produced without the required explicit naming.
- **Cloud-storage refusal tests**, at two levels: no plan may include any path
  under `~/Library/Mobile Documents` or `~/Library/CloudStorage`; *and* a
  `node_modules` planted inside a synced folder — matched by an ordinary,
  correctly-scoped detector — must be refused on its dataless/ubiquitous status
  alone. The second test is the one that matters, because it is the failure mode
  that actually occurs.
- **Materialization test**: scanning a directory of evicted cloud files
  downloads nothing and changes local allocation by zero.
- Open-descriptor test: deleting a file held open by a live process reports the
  shortfall and names the discrepancy class (§11.1) rather than claiming the
  space was reclaimed.

---

## 16. Research references

Claims in this document trace to the following. Local paths refer to the
installed macOS SDK and command-line tools on the author's machine.

**APFS attributes and file metadata**
- `getattrlist(2)` — `ATTR_CMNEXT_PRIVATESIZE` semantics ("bytes not trapped in
  a clone or snapshot, freed immediately on deletion"), `ATTR_CMNEXT_CLONEID`,
  `ATTR_CMNEXT_CLONE_REFCNT`.
  `/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/share/man/man2/getattrlist.2`
- `getattrlistbulk(2)` — bulk retrieval semantics, `ATTR_CMN_FLAGS` including
  `SF_FIRMLINK`.
  `/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/share/man/man2/getattrlistbulk.2`
- `sys/attr.h` — attribute constants; `ATTR_CMNEXT_CLONEID = 0x00000100`,
  `ATTR_CMNEXT_REALDEVID = 0x00000040`.
  `/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/include/sys/attr.h`
- `du(1)` — default block-usage reporting, `-A` apparent size, hardlink
  deduplication.
  `/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/share/man/man1/du.1`

**APFS behaviour (external)**
- Howard Oakley, "Free space on an APFS volume is an illusion" and related
  articles on clones, snapshots, and firmlinks — eclecticlight.co
- Apple, *File System Events Programming Guide* — event IDs, `since` semantics,
  history purging.

**Performance (external, unreplicated)**
- Andrew Healey, "Maybe the Fastest Disk Usage Program on macOS" — the
  `getattrlistbulk` benchmark figures and profile breakdown cited in §4.1.
- `gdu` documentation — garbage-collector behaviour during analysis.

**Prior art**
- `duh` — APFS-clone-aware disk usage; clone-family accounting at the lowest
  common ancestor.
- `kondo`, `cargo-sweep`, `npkill` — developer artifact cleaning.
- `dust`, `dua-cli`, `diskus`, `ncdu`, `gdu` — analyzers.

**Known-location providers**
- Homebrew command reference — `cleanup`, `--dry-run`, `--scrub`, and pruning
  semantics: <https://docs.brew.sh/Manpage.html>
- Docker pruning reference and Docker Desktop storage settings — object-specific
  pruning, contexts/builders, volume exclusions, and sparse VM storage:
  <https://docs.docker.com/engine/manage-resources/pruning/> and
  <https://docs.docker.com/desktop/settings-and-maintenance/settings/>
- OrbStack Docker documentation — OrbStack supplies a Docker engine with
  containers, images, and persistent volumes: <https://docs.orbstack.dev/docker/>
- pnpm store documentation — `pnpm store path` and `pnpm store prune` semantics:
  <https://pnpm.io/cli/store>
- npm cache documentation — `verify`, full cache cleaning, and npx cache
  inspection/removal: <https://docs.npmjs.com/cli/v11/commands/npm-cache/>
- Bun package-manager documentation — configured global cache and
  `bun pm cache rm`; macOS clonefile installation behavior:
  <https://bun.sh/docs/pm/cli/pm> and <https://bun.sh/docs/pm/global-cache>
- VMware Fusion disk cleanup and snapshot guidance — sparse-disk compaction and
  the temporary-space requirements of consolidation:
  <https://knowledge.broadcom.com/external/article/328883> and
  <https://knowledge.broadcom.com/external/article/302480>
- Apple Support — files moved to Trash do not free storage until Trash is
  emptied: <https://support.apple.com/102624>
- Anthropic documents Cowork's use of an isolated Linux VM, but a stable public
  host-side cleanup API for its VM bundle has not been found. Treat bundle paths
  and reset behavior as versioned research rather than a supported contract.

**Cloud / File Provider storage**
- Apple, File Provider framework and `NSFileProviderExtension` — dataless
  materialization and eviction semantics for `~/Library/CloudStorage`.
- `brctl(1)` — iCloud eviction from the command line.
- Apple, "Optimize storage" / evict-vs-delete behaviour for iCloud Drive.
- **To be verified experimentally (Phase 0):** which metadata reads, if any,
  trigger materialization of a dataless file; whether `getattrlistbulk` is safe
  in this respect; and the exact attribute or flag that reliably identifies a
  dataless or ubiquitous item.

**Container-level accounting**
- `diskutil apfs list` — container membership and volume roles.
- APFS shared free-space semantics across volumes in a container; relationship
  between reported purgeable space and snapshot storage (Oakley, eclecticlight.co).

**To be sourced before implementation**
- Whether and when Docker Desktop and OrbStack inner reclamation shrinks their
  host-side sparse storage on each supported version.
- Whether purgeable space, as reported by macOS, is strictly a superset of
  snapshot storage or merely overlaps it — the `doctor` layout depends on
  stating the relationship correctly.
- Cost and privilege requirements of enumerating open descriptors per path via
  `proc_listpids` / `proc_pidfdinfo` for the current user's processes.
- OrbStack Linux-machine inspection and supported deletion/compaction semantics.
- Anthropic-supported Claude Cowork VM inspection, reset, and persistence
  semantics; do not rely on an observed bundle layout as a stable API.
- macOS wallpaper cache paths, process-quiescing, and regeneration behavior on
  each supported macOS release; Apple does not publish a cleanup interface.
- Nix garbage collection and generation semantics.
- FSEvents retention guarantees in practice.
- TCC behaviour for CLI binaries across install methods.

---

## 17. Open questions

- What can accounting actually promise? (Phase 0. Everything else is downstream.)
- Does requesting the full attribute set materially slow the bulk walk?
- Can clone-family attribution be computed in one pass, or does it need a second
  pass over the arena, and what does holding clone metadata cost in memory?
- Should `scan` and `targets` share one walk? Probably eventually; likely
  separate in the first implementation.
- Does Docker's inner reclamation ever shrink `Docker.raw` on the host, and
  under what conditions?
- What is an honest confidence rule for provider-managed space, where cleanrs
  cannot inspect the internals at all?
- How tight is the floor/ceiling interval in practice on a real developer
  machine? If it is routinely wide, the interval is honest but not actionable,
  and the accounting model needs more than clone IDs to narrow it.
- Should cleanrs offer eviction of cloud content as an action at all, or only
  report it? Eviction is safe and genuinely frees local space, but it makes
  cleanrs a participant in sync behaviour, which is a larger surface than
  deleting build artifacts.
- Is a snapshot generation token cheap enough to check on every index read, or
  does it need its own caching?
- Distribution and Full Disk Access UX across Homebrew, direct download, and
  `cargo install`.

---

## 18. Glossary

- **Apparent size** — logical file length (`st_size`).
- **Allocated size** — blocks attributed to the file (`st_blocks`).
- **Immediately private** — bytes freed at once if **this file alone** is
  deleted, not held by a clone or snapshot (`ATTR_CMNEXT_PRIVATESIZE`). Never
  summed across a set; see *superadditivity*.
- **Superadditivity** — the property that deleting a set can free more than the
  sum of what deleting each member individually would free, because members may
  share blocks only with each other (§3.1).
- **Floor / ceiling** — the bounds of the reclaimable interval: Σ private size,
  and clone-family-deduplicated allocated, respectively.
- **Estimated reclaimable** — the interval `[floor, ceiling]` with derived
  confidence.
- **Actually reclaimed** — observed post-run result, from providers and measured
  container delta.
- **Clone** — an APFS file sharing blocks with another until modified.
- **Clone family** — files sharing a clone ID; a sharing *candidate* set, not a
  proven partition of shared blocks.
- **Container** — the APFS partition holding one or more volumes that share a
  single free-space pool. The unit of reconciliation.
- **Dataless file** — a File Provider placeholder with logical content stored
  remotely and little or no local allocation. Deleting one deletes the remote
  copy; **evicting** one frees local space only.
- **Firmlink** — a bidirectional link stitching the System and Data volumes.
- **Purgeable** — space macOS reports as reclaimable on demand but currently
  occupied. Largely contains snapshot storage; not a disjoint category.
- **Target** — a declarative location or command that reclaims space.
- **Provider** — a tool owning its own storage, cleaned through its own
  interface (inspect/plan/execute/verify).
- **Class** — the user-facing shorthand (`free`, `cheap`, `network`,
  `expensive`, `irreplaceable`) expanding to internal properties.
- **Level** — how far a `clean` run may go: `safe`, `standard`, `aggressive`.
