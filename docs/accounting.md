# Accounting

A path's size is evidence about storage, not a promise about free space after
deletion. [accounting.rs](../cleanrs-core/src/accounting.rs) estimates selected
sets; [platform.rs](../cleanrs-core/src/fs/platform.rs) supplies platform metadata.

## Four numbers

| Metric | Meaning |
|---|---|
| Apparent | Logical file length; sparse holes and cloud placeholders can be large |
| Allocated | Filesystem blocks attributed to a file; may include shared extents |
| Immediately-private | Bytes the filesystem says a single-file deletion can immediately free |
| Reclaimable | A selected-set interval with confidence and explanations |

**Correction:** macOS `du` reports filesystem block usage by default, deduplicates
hardlinks, and handles sparse allocation; `du -A` requests apparent size.
cleanrs's APFS distinction is clone/snapshot uncertainty and reconciliation,
not a claim that `du` always reports logical length.

## Selected sets, sharing, and confidence

Per-file `PRIVATESIZE` is not additive as a reclamation estimate. Two mutual
clones can each have zero immediately-private bytes; deleting both can release
shared data. The private contribution is useful as a lower bound, with hardlink
identity deduplicated, but cannot represent the complete selected-set recovery.

The estimator groups file identities by `(device, inode)`, accounts for links
outside the selected set, and constructs clone-family bounds. A clone ID is an
origin-family hint, not proof that diverged files still share every extent.
Partially selected families and missing metadata leave uncertainty. A family's
ceiling must remain nonzero even when all members report zero private size.
Do not add per-row intervals to compute a combined cleanup total: estimate the
union of selected candidates.

Confidence derives from interval tightness and coverage: incomplete coverage or
missing private metadata is low confidence; a tight known interval can be high;
a wider known interval is medium. Missing bounds are unknown, not zero. The
current high-confidence tolerance is the larger of 4096 bytes and 1% of the ceiling.
See the implementation for the precise treatment of partially observed families.

Hardlinks outside the selected scope keep an inode alive. APFS snapshots can
pin old extents after unlink; snapshot presence is diagnostic evidence, not a
measured per-file adjustment. Purgeable storage is occupied space the OS may
reclaim on demand, neither free bytes nor a directory-owned cleanup target.
Open files, asynchronous accounting, and concurrent allocations can separate
removed allocation from a later observed free-space change. Provider-reported
bytes are another kind of evidence and remain separate from filesystem estimates.

## SDK-verified facts and bounded observations

The Phase 0 findings log was condensed here on 2026-10-15; the full log is in git
history. The following facts carry forward its `[exp]` evidence. They were checked
on Apple Silicon, macOS 26.5.2 (25F84), against the then-installed SDK under
`/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/`. They were not reverified
against an SDK on the Linux release-readiness host.

- `ATTR_CMNEXT_PRIVATESIZE = 0x00000008` describes bytes not trapped in a clone or
  snapshot and freed immediately by deleting that single file.
- `ATTR_CMNEXT_CLONEID = 0x00000100` and
  `ATTR_CMNEXT_CLONE_REFCNT = 0x00001000` expose clone-family metadata.
- `SF_FIRMLINK = 0x00800000` and `SF_DATALESS = 0x40000000` are available through
  `ATTR_CMN_FLAGS`.
- `getattrlistbulk` requires `ATTR_CMN_NAME | ATTR_CMN_RETURNED_ATTRS`, returns
  packed variable-length records, and supports extended common attributes via
  `FSOPT_ATTR_CMN_EXTENDED`. Returned masks distinguish unsupported attributes.
- The native parser requests safety, identity and accounting metadata in one
  request. Failed bulk calls fall back per directory to `readdir`/`lstat` with
  reduced confidence. It does not open file content during traversal.
- Hardlink fixtures verify one contribution per inode; a mutual-clone fixture
  retains a nonzero ceiling. A macOS-only test uses `clonefile` on a real 1 MiB
  fixture and checks that bulk clone metadata reaches the estimator.
- One ordinary repository scan agreed exactly with macOS `du -sk` at 380,870,656
  allocated bytes. That observation does not establish clone-aware accuracy for
  arbitrary trees or a performance advantage.
- A manually evicted, disposable 32 MiB iCloud file retained `SF_DATALESS`, zero
  allocation, logical size and mtime through an initial metadata scan and five
  repeats. This establishes that prepared iCloud case only. Third-party File
  Providers and programmatic cloud eviction remain unverified; synced candidates
  remain refused and no cloud-eviction operation is enabled.
- `/`, Data and VM were distinct volume presentations in one APFS container;
  Data and VM shared its free-space figure. Device IDs alone do not collapse
  their diagnostic rows. Firmlink flags and root/Data topology prevent duplicate
  traversal through visible and backing paths.

Snapshot creation/expiry effects on private size and a repeated, quiesced
container-delta harness remain unmeasured. Estimates are not claimed to bracket
every observed APFS outcome. A one-tier bulk request remains the implementation
choice; no claim that it beats `du` follows from the early timing samples.
The [benchmark workflow](../.github/workflows/benchmark.yml) and
[synthetic fixture](../cleanrs-core/examples/synthetic_tree.rs) remain available.

## `doctor` reconciliation

[doctor.rs](../cleanrs-core/src/fs/doctor.rs) reports APFS container capacity/free
space, System/Data/VM volume presentations, snapshots, swap and known coverage
limits. Free space is container-scoped: do not add volume-free rows or treat
Data-volume used bytes as a complete explanation of container consumption.
Other volumes, metadata, snapshots, purgeable estimates and permission-denied
paths can leave an unexplained residual.

`doctor --scan PATH` attributes an explicit scan against Data-volume diagnostics;
it does not silently scan all of home or erase coverage gaps. The current
`unattributed` field subtracts the scan estimate ceiling from Data-volume used
bytes (or portable total used when Data is absent), saturating at zero. That
residual is not a complete container accounting model. Full Disk Access
can improve visibility, but cannot make directory allocation equivalent to
container free space. Snapshot-pinned bytes and purgeable storage are not
invented when the platform does not supply them.

## Linux

The portable backend uses `lstat` and `st_blocks × 512` for allocated bytes,
records hardlinks, and reports apparent lengths. It supplies allocated size as a
private-size fallback, without APFS private or clone metadata. This cannot detect
reflink sharing on Linux filesystems; even a tight portable interval is not proof
of physical reclamation on a sharing filesystem. `doctor` has reduced portable
reporting, and macOS-specific system targets are hidden.
