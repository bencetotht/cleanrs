# Phase 0 feasibility findings

> Status: **in progress**, updated 2026-08-11. Results marked `[exp]` were
> measured on this machine. Unrun safety experiments remain `[hyp]`; they are
> not silently promoted to product guarantees.

## Environment

- Hardware: Apple Silicon (`arm64`); exact model was not queried because it is
  not needed to establish API semantics.
- OS: macOS 26.5.2 (build 25F84).
- Rust: rustc 1.96.0; workspace MSRV is 1.85.
- Filesystem: APFS container `disk3`, 494,384,795,648 bytes capacity as reported
  by `diskutil` at measurement time.
- SDK source of truth:
  `/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/`.

All file experiments use temporary fixture directories. No real home-directory
tree and no cloud-synced folder has been traversed by the test suite.

## Attribute availability and semantics

`[exp]` The installed SDK confirms:

- `ATTR_CMNEXT_PRIVATESIZE = 0x00000008`: bytes not trapped in a clone or
  snapshot and freed immediately if the single file is deleted.
- `ATTR_CMNEXT_CLONEID = 0x00000100` and
  `ATTR_CMNEXT_CLONE_REFCNT = 0x00001000`.
- `SF_FIRMLINK = 0x00800000` and `SF_DATALESS = 0x40000000`, both available
  through `ATTR_CMN_FLAGS`.
- `getattrlistbulk` requires `ATTR_CMN_NAME | ATTR_CMN_RETURNED_ATTRS`, returns
  packed variable-length records, and supports extended common attributes via
  `FSOPT_ATTR_CMN_EXTENDED`.

The native parser requests returned masks, per-entry errors, name, device,
object type, mtime, flags, file ID, link count, logical/allocated sizes,
private size, clone ID, and clone refcount in one call. Unsupported attributes
are identified through each record's returned mask. A failed bulk call falls
back for that directory to `readdir` plus `lstat` and lowers estimate confidence.

## Accounting fixtures

`[exp]` Unit fixtures establish these algorithmic properties:

- Hardlinks with the same `(device, inode)` contribute allocation and private
  size once per selected set.
- A mutual clone family whose individual private sizes are zero retains a
  non-zero ceiling, rather than being reported as zero reclaimable.
- On a scan of this repository, cleanrs reported 380,870,656 allocated bytes;
  macOS `du -sk` reported 371,944 KiB, also 380,870,656 bytes.
- A macOS-only test creates a real 1 MiB clone with `clonefile`, checks that both
  entries expose clone metadata through the bulk parser, and verifies that the
  selected set keeps at least a 1 MiB ceiling.

`[hyp]` A before/after APFS **container** free-space delta is not yet a reliable
automated assertion. Container free space is noisy because unrelated processes
allocate concurrently and APFS updates may be asynchronous. A repeated,
quiesced ground-truth harness still needs to demonstrate that every real clone,
divergence, sparse-file, hardlink, and snapshot fixture lands within its
interval. Until then, output is explicitly an estimate.

## Snapshot behaviour

`[hyp]` Snapshot creation/expiry and its effect on `PRIVATESIZE` has not been
measured. Creating and deleting Time Machine/APFS snapshots requires privileges
and changes machine state, so it is not run automatically. Snapshot count is
reported by `doctor`; estimates are not claimed snapshot-exact.

## Cloud and dataless safety

`[exp]` `SF_DATALESS` is present in the installed SDK and is collected in the
same metadata-only bulk request. Scanner code never opens file content and
reports the count of dataless entries it observed.

`[hyp — hard gate still open]` No controlled directory of evicted iCloud or
File Provider files was supplied. Therefore zero materialization and eviction
semantics have **not** been demonstrated. The test suite will not manufacture
or evict the user's cloud data. Do not treat scanning a real home directory as
validated until this experiment is run against an explicitly prepared fixture.

## Cost, topology, and open descriptors

- `[exp]` `/`, `/System/Volumes/Data`, and `/System/Volumes/VM` are separate
  APFS volume presentations within `disk3`; Data and VM share the container's
  free-space figure. `st_dev` is intentionally not used to collapse doctor
  rows. On this machine VM reported 17,188,655,104 used bytes at inspection.
- `[exp]` Bulk metadata works end-to-end on the repository fixture. The scanner
  bounds active blocking workers (default: available CPUs, capped at eight),
  never follows symlinks, and combines firmlink flags with root/Data topology so
  visible roots are not duplicated through their backing Data paths.
- `[exp]` A preliminary warm-cache run on the fixed 10,000-file synthetic tree
  took 0.06–0.07 s for default-worker cleanrs and 0.01 s for macOS `du -sk`
  (three `/usr/bin/time -p` samples each). One-worker cleanrs took 0.05 s. These
  coarse samples are useful because they reject any current claim that the MVP
  beats `du`; they are not the scheduled `hyperfine` benchmark and do not
  isolate the marginal cost of extended attributes.
- `[hyp]` Minimal versus extended attribute masks still need an A/B measurement
  on that tree.
- `[hyp]` A `proc_listpids`/descriptor sweep has not been implemented or timed.

## One-tier versus two-tier decision

Use a **one-tier request provisionally**: the safety flags, hardlink identity,
and accounting attributes arrive with directory entries, and avoiding a second
metadata pass is the design's primary performance premise. The scheduled
warm-cache benchmark records the full request. If its measured marginal cost is
material, this decision must be reopened; no performance claim is made yet.

## Current promise

cleanrs can promise a read-only, content-free traversal; allocated/apparent
accounting; hardlink deduplication; clone-family interval construction; explicit
unknowns for missing private metadata or denied coverage; dataless/firmlink
detection; and container-aware diagnostics. It cannot yet promise that every
interval brackets observed APFS reclamation, that snapshot effects are fully
modeled, or that metadata traversal never materializes every supported File
Provider's placeholders.
