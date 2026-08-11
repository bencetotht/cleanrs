# cleanrs — Vision & Design

> **Status:** design document. No implementation yet. This file is the reference
> for what cleanrs is, why it exists, and how it should be built.
>
> **Claim discipline:** every quantitative statement in this document carries a
> label — `[ref]` external reference, `[exp]` preliminary experiment, `[target]`
> a goal to design toward, `[hyp]` a hypothesis requiring validation. Sources
> are listed in §16. Nothing here has been measured by this project, because
> this project has no code yet.

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

**Provider-managed storage.** Docker on macOS stores everything inside a sparse
disk image. Reclaiming space *inside* the VM does not necessarily shrink the
image on the host. Reporting the inner figure as recovered host space would be
misleading.

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
| **Immediately private** | Bytes the filesystem reports as freed immediately if this file were deleted — i.e. not trapped in a clone or snapshot. Derived from `ATTR_CMNEXT_PRIVATESIZE` [ref: getattrlist.2]. |
| **Estimated reclaimable** | A group- or path-level estimate of what deletion would return, carrying a **confidence** level and a one-line explanation. |
| **Actually reclaimed** | Observed after a cleanup: provider-reported figures plus measured volume free-space delta. |

**Estimated reclaimable is the headline**, and it is always shown with its
confidence:

```
18.2 GiB estimated reclaimable · medium confidence
Shared APFS extents and snapshots may reduce the result
```

### 3.1 Confidence must be decidable, not decorative

If most figures read "medium," users learn to ignore the label. Confidence is
therefore computed from explicit conditions, tuned so that **high is the common
case** for ordinary build artifacts:

| Level | Conditions |
|---|---|
| **high** | Every file in scope has link count 1 (or all links within scope), no clone references outside scope (`ATTR_CMNEXT_CLONE_REFCNT` indicates sole reference), `PRIVATESIZE` ≈ allocated, no snapshot covers the path, and the whole subtree is being removed. |
| **medium** | Clone references or snapshot coverage detected, but bounded — a lower bound on reclaimable space can still be stated. |
| **low** | Provider-managed storage, significant snapshot coverage, or incomplete scan coverage (permissions). Report a range, or decline to give a figure. |

A `--explain <path>` mode prints exactly which condition demoted a figure.

### 3.2 Accounting is a research task, not a solved one

**Exact freeable bytes is an aspiration, not a claimed capability.** Phase 0 of
the roadmap (§12) is a dedicated spike to determine what is actually derivable,
evaluating together:

- `ATTR_CMNEXT_PRIVATESIZE` — per-file immediately-private bytes
- `ATTR_CMNEXT_CLONEID` — clone family identity
- `ATTR_CMNEXT_CLONE_REFCNT` — how many references share the stream
- hardlink count versus links actually observed within scope
- snapshot enumeration and coverage
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
- **Storage-aware defaults.** Oversubscribe on NVMe; on rotational or
  network-backed mounts parallelism hurts — detect via `statfs` and reduce.
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

```
cleanrs doctor

  Volume  /System/Volumes/Data
    Capacity                                    1.86 TB
    Used (df)                                   1.71 TB
    Free (df)                                    153 GB

  Observed
    Files, immediately private (scanned)        1.02 TB    high confidence
    Purgeable (reported by APFS)                  94 GB
    Snapshots                                 12 present   size not directly attributable
    Unexplained gap                              292 GB    not attributed

  Coverage
    Inaccessible roots                                47   size unknown
    Scan coverage                             incomplete   Full Disk Access not granted
```

Note what this does *not* say: it does not claim the inaccessible roots contain
some number of gigabytes, and it does not pretend the categories sum. Snapshots,
purgeable space, volume sharing within an APFS container, and ordinary files do
not form a clean additive equation, and the output should not imply one.

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

---

## 7. Providers

The commands below are an **initial provider backlog**, not a set of verified
invocations. Each entry needs real investigation before implementation.

| Provider | Starting point | Known complications |
|---|---|---|
| Homebrew | `brew cleanup -s` | Scope of `-s`; pinned formulae. |
| Docker build cache | `docker builder prune` | Addresses the *selected* builder; Buildx supports multiple builders, each with its own cache. Age and max-space filters (`--filter until=`, `--keep-storage`) are far better for routine cleanup than unfiltered prune. |
| Docker images | `docker image prune` | `-a` can trigger very large future downloads; must not be presented as a generic safe action. |
| Docker volumes | — | **`user_data`.** Never automated. |
| Nix | `nix-collect-garbage` | `-d` additionally deletes old generations and destroys rollback capability. Plain GC and `-d` belong at different levels. |
| Cargo | `cargo clean` / `cargo sweep` | Must resolve workspace roots and honour shared or custom `CARGO_TARGET_DIR`; a member crate inside a workspace must not trigger a duplicate clean of the same directory. |
| Xcode simulators | `xcrun simctl delete unavailable` | Distinguish unavailable runtimes from in-use devices. |

### 7.1 Providers are an interface, not a command string

Every provider implements four operations:

- **inspect** — what exists, how much space it appears to hold, in what units,
  and whether that space is host-visible or provider-internal.
- **plan** — the concrete argv (or deletion set) that would run, with the
  provider's own estimate of what it would reclaim.
- **execute** — run it, capturing stdout/stderr and exit status.
- **verify** — re-inspect and report the provider's view of what changed,
  separately from the measured host volume delta.

This is what makes §3's "actually reclaimed" metric obtainable, and it is what
makes the Docker-on-macOS case (inner reclamation ≠ host reclamation) reportable
rather than misleading.

### 7.2 Direct deletion

Reserved for artifacts with no owning cleanup command — `DerivedData`,
`node_modules`, `.next`, `dist`, `__pycache__`. `rm -rf` on storage owned by a
provider corrupts that provider's state and leaves the user worse off.

---

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
  (§11), and the observed volume free-space delta is recorded independently of
  both.

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

### 8.3 Other mechanics

- **Dry-run is the default.** `clean` plans and prints; `--apply` executes.
- **Never run as root by default.** Refuse with an explanation absent an
  explicit override.
- **Print exact argv** before running any external command; require `--yes` for
  unattended execution.
- **Journal everything** (§11).

### 8.4 Staleness

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
| `recoverability`, `rebuild_cost`, `network_required`, `automation` | Optional overrides. |
| `reclaim` | `delete` (with `children_only`) or `run` (an argv array). |
| `stale_after` | Minimum age before eligibility. |
| `scope` | Roots to search, for marker-based targets. |

### 9.3 Configuration form

```toml
[[target]]
id = "xcode.derived-data"
detect = { path = "~/Library/Developer/Xcode/DerivedData", exists = true }
class = "free"
rebuild_cost = "high"          # override: full rebuilds are not trivial
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
network_required = true        # override: rebuilds may re-pull layers
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
containing `:` must use the long form.

The long form covers everything the configuration file can express:

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
  --target-argv '["docker","builder","prune","--filter","until=168h"]' \
  --target-class cheap \
  --target-requires docker \
  --apply
```

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

A cached scan makes the tool feel instant. Two constraints shape it.

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

### 10.3 Size and format are open questions

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
| `estimated_reclaimable` | cleanrs's estimate, with its confidence level. |
| `provider_estimated` | The provider's own pre-run estimate, where available. |
| `provider_reported` | What the provider says it reclaimed. |
| `volume_free_delta` | Measured free-space change on the volume. |
| `bytes_trashed` | Moved but **not** reclaimed. |

`volume_free_delta` is **observed evidence, not attribution** — other processes
allocate and free space concurrently, and snapshot expiry is asynchronous. It is
reported as such, never as a precise figure for what cleanrs did.

Together these fields are the calibration loop for §3.2: over many runs, the
divergence between `estimated_reclaimable` and `volume_free_delta` is the
measurement that tells us whether the accounting model is any good.

The journal also needs **rotation** and, because it records private directory,
project, and client names, an option for **path redaction** (hashing or
truncating paths while retaining sizes and target ids).

---

## 12. Roadmap

**Phase 0 — Accounting research spike.** Before designing around any accounting
model, determine empirically what is derivable: the behaviour of
`PRIVATESIZE`, `CLONEID`, `CLONE_REFCNT`, and link counts across clones,
divergence, snapshots, and hardlinks; the real cost of requesting the full
attribute set; and a fixture harness that deletes known trees and compares
predictions against measured volume deltas. **Output: a written statement of
what cleanrs can and cannot promise.** Everything downstream depends on it.

1. **Platform layer and a `scan` that prints one number.** `getattrlistbulk` end
   to end, benchmarked against `du`, `diskus`, `gdu` from day one.
2. **Accounting.** Hardlinks, clone families, private size, snapshot detection,
   mount boundaries, firmlinks, permission errors as counted locations.
   Confidence computation per §3.1.
3. **`doctor`.** Cheap on top of step 2, and immediately the most useful command.
4. **Targets and `clean --dry-run`.** Manifest, CLI specs, detection, sizing.
5. **Providers.** inspect/plan/execute/verify for Homebrew, Docker, Cargo, Xcode.
6. **`clean --apply`.** Path invariants, permanent deletion, journal, `history`.
7. **TUI.** Deliberately late — most enjoyable, least load-bearing, and far
   nicer to build against a stable core.
8. **Index and incremental scanning.**
9. **`schedule` / launchd.**
10. **Linux support.**

---

## 13. Command surface

```
cleanrs scan [PATH]           One-shot analysis.
                              --json --apparent --allocated --private --reclaimable
                              --depth N --min-size --exclude --explain
cleanrs tui  [PATH]           Interactive browser (ratatui).
cleanrs doctor                Reconcile volume state; report unexplained gap.
cleanrs targets               List detected targets with sizes and confidence.
                              --only --exclude --target... --export
cleanrs clean                 Reclaim space. Dry-run by DEFAULT.
                              --level safe|standard|aggressive
                              --apply --yes --trash
                              --only --exclude --target... --config
cleanrs history               Journal of past runs, estimated vs. observed.
cleanrs schedule install|remove|status
cleanrs index [--reindex|--clear|--status]
```

Cross-cutting rules: dry-run default for anything destructive; `--json` on every
command; meaningful exit codes for scripting; shell completions and a man page
shipped; `NO_COLOR` respected; non-TTY degrades to plain output.

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
                 index, journal.
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
| Index | Undecided — benchmark `rkyv`+`memmap2`, custom, `redb`, SQLite | See §10.3. |
| Config | `serde` + `toml` | Target manifests. |
| Errors | `anyhow` + `thiserror`, or `miette` | `miette` gives good CLI diagnostics. |
| Logging | `tracing` + `tracing-subscriber` | Span timings for perf work. |
| Progress | `indicatif` | Non-TUI progress. |
| Benchmarks | `criterion`, `hyperfine` | Micro and end-to-end. |
| Testing | `insta`, `tempfile`, `proptest` | Snapshot and property tests. |

`trash` is a dependency only for the opt-in `--trash` mode, not the default path.

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
- Snapshot tests (`insta`) over the tree and all reported metrics.
- Differential testing against `du` on trees with no clones, where the two
  *should* agree on allocated size — divergence there is a cleanrs bug.
- **Ground-truth harness**: delete a fixture set, measure the real volume delta,
  compare against the prediction. This is the only test that validates the
  headline metric.
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

**To be sourced before implementation**
- Docker/Buildx cache reclamation semantics and filter behaviour on macOS,
  including whether inner reclamation shrinks the host disk image.
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
- Distribution and Full Disk Access UX across Homebrew, direct download, and
  `cargo install`.

---

## 18. Glossary

- **Apparent size** — logical file length (`st_size`).
- **Allocated size** — blocks attributed to the file (`st_blocks`).
- **Immediately private** — bytes freed at once on deletion, not held by a clone
  or snapshot (`ATTR_CMNEXT_PRIVATESIZE`).
- **Estimated reclaimable** — a path- or group-level estimate with confidence.
- **Actually reclaimed** — observed post-run result, from providers and measured
  volume delta.
- **Clone** — an APFS file sharing blocks with another until modified.
- **Clone family** — files sharing a clone ID; a sharing *candidate* set, not a
  proven partition of shared blocks.
- **Firmlink** — a bidirectional link stitching the System and Data volumes.
- **Purgeable** — space macOS reports as reclaimable on demand but currently
  occupied.
- **Target** — a declarative location or command that reclaims space.
- **Provider** — a tool owning its own storage, cleaned through its own
  interface (inspect/plan/execute/verify).
- **Class** — the user-facing shorthand (`free`, `cheap`, `network`,
  `expensive`, `irreplaceable`) expanding to internal properties.
- **Level** — how far a `clean` run may go: `safe`, `standard`, `aggressive`.
