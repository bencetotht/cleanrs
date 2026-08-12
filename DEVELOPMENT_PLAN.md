# cleanrs — Development Plan

Five phases, plus an optional sixth. Each is a shippable increment with testable
exit criteria. **Do not start a phase until the previous one's exit criteria are
met** — the ordering encodes real dependencies, not preference.

The phase boundaries were chosen sparingly. Phases 0→1 split because everything
downstream depends on an unanswered research question. Phases 2→3 split because
that is the boundary between a tool that only reads and a tool that destroys
data. Nothing else splits.

Section references (§) point into [VISION.md](VISION.md).

| Phase | Delivers | Destructive? |
|---|---|---|
| 0 | A written statement of what can be promised | No |
| 1 | `scan`, `doctor` — read-only analysis | No |
| 2 | `targets`, `clean --dry-run` — plans, never executed | No |
| 3 | `clean --apply`, `history` — execution and verification | **Yes** |
| 4 | `tui`, index, `schedule` — daily-driver quality | Yes |
| 5 | Linux support | Yes |

---

## Phase 0 — Feasibility research

**Goal:** determine what cleanrs can honestly promise, before any architecture
is built on assumptions. Output is a document, not a product.

Everything in VISION.md §3 is currently a hypothesis. If the floor/ceiling
interval does not bracket reality, or if the attribute set is too expensive to
collect, the design changes — and it is far cheaper to learn that now.

Code written in this phase is **throwaway probe code**. Do not attempt to build
reusable abstractions here; the point is to answer questions fast.

### Work

**Accounting semantics**
- Bind `getattrlistbulk` well enough to request the full attribute set:
  `ATTR_CMN_*` basics, `ATTR_CMNEXT_PRIVATESIZE`, `ATTR_CMNEXT_CLONEID`,
  `ATTR_CMNEXT_CLONE_REFCNT`, flags including `SF_FIRMLINK`.
- Build fixtures with `clonefile`: pure clones, diverged clones, partially
  diverged clones, hardlinks, sparse files, and the **mutual-clone case** (files
  that clone only each other).
- For each fixture, record every attribute, then delete and measure the real
  container free-space delta.
- **Answer:** does `[Σ PRIVATESIZE, clone-deduplicated allocated]` actually
  bracket the observed reclamation in every case? Where does it fail?
- **Answer:** how tight is the interval on a real developer home directory? A
  correct but perpetually wide interval is honest and useless.

**Snapshot behaviour**
- Take a snapshot; observe `PRIVATESIZE` for untouched files. Delete it; observe
  the values return. Confirm no filesystem events fire throughout (§10.3).
- Measure the cost of obtaining a snapshot generation token.

**Cloud / dataless safety — this gates everything**
- Identify the reliable attribute or flag for dataless / ubiquitous items.
- Scan a directory of evicted iCloud and `~/Library/CloudStorage` content and
  **assert zero bytes downloaded and zero change in local allocation.**
- Confirm eviction (`brctl evict` or File Provider APIs) frees local space and
  leaves the remote copy intact.

**Cost and topology**
- Marginal cost of the extended attribute set versus a minimal request. Decide
  whether a two-tier request is needed (§14.2).
- Container topology: enumerate volumes, confirm shared free-space behaviour,
  and check how `SF_FIRMLINK` and `st_dev` actually relate across `/`, `/Users`,
  `/System/Volumes/Data`, and `/System/Volumes/VM`.
- Cost and privilege requirements of a `proc_listpids` descriptor sweep against
  a known path set (§8.3).

### Exit criteria

- [ ] `docs/findings-phase0.md` exists, stating for each question: what was
      measured, on what hardware and macOS version, and what cleanrs can and
      cannot promise as a result.
- [ ] The dataless materialization test passes. **This is a hard gate** — no
      traversal of a real home directory before it does.
- [ ] VISION.md §3 has been updated to match reality, with `[hyp]` labels
      promoted to `[exp]` or the design revised.
- [ ] A decision is recorded on one-tier versus two-tier attribute requests.

### Out of scope

CLI structure, error handling, performance tuning, any reusable code.

---

## Phase 1 — Read-only truth

**Goal:** a tool that tells you where your disk went, accurately, and reconciles
against what storage actually reports. Ships as v0.1 and is genuinely useful on
its own.

Scanner and accounting are one phase because they are not separable: the
accounting is the reason the scanner exists, and a scanner without it cannot be
meaningfully validated. `doctor` joins them because it is the same capability —
understanding the disk — reached through a different data source.

### Work

**Platform layer (`cleanrs-core::fs`)**
- `getattrlistbulk` bindings with fixture-tested buffer walking of the
  variable-length packed records.
- Per-mount capability detection; graceful fallback to `readdir` + `lstat` where
  bulk attributes are unavailable.
- Container and volume topology enumeration.
- Everything behind a trait, so Phase 5 is mechanical.

**Traversal**
- Work-stealing pool over a directory queue; bounded in-flight count; no async
  runtime (§4.2).
- Storage-aware worker defaults via `statfs`.
- Traversal rules (§4.3): no mount crossing by default, no symlink following,
  firmlink handling by flag *and* topology, pseudo-filesystem skips.
- Permission errors recorded as counted locations, never as bytes.
- Arena tree: `Vec<Node>` with `u32` indices, per-directory name interning.
- Sharded inode and clone-ID sets.

**Accounting**
- Apparent, allocated, immediately private per file.
- Clone-family and hardlink grouping; floor/ceiling interval per §3.1.
- Confidence derived from interval tightness per §3.2.
- Dataless detection wired in at scan level — flagged and reported, never
  materialized.

**Commands**
- `scan [PATH]` with `--json`, `--apparent`, `--allocated`, `--private`,
  `--reclaimable`, `--depth`, `--min-size`, `--exclude`, `--explain`.
  **Allocated ranking by default** (§13).
- `doctor`, container-scoped from the first line of code — retrofitting
  volume-scoped output means rewriting it. Volume enumeration, snapshot
  presence, purgeable space with containment shown, unexplained gap, coverage
  reporting, Full Disk Access status and remediation.

### Exit criteria

- [ ] `scan` agrees with `du` on allocated size for trees containing no clones.
- [ ] The mutual-clone fixture reports a ceiling equal to real reclamation, not
      a floor of zero.
- [ ] Ground-truth harness: for every fixture, measured container delta falls
      within `[floor, ceiling]`. **Below floor is a hard failure.**
- [ ] Scanning `/` produces no phantom duplicate of the home directory.
- [ ] `doctor` accounts for swap on `/System/Volumes/VM` and any `/nix` volume.
- [ ] Materialization test passes against a live cloud folder.
- [ ] Benchmarked against `du`, `diskus`, `gdu`, `dust` with `hyperfine` on a
      fixed synthetic tree; numbers recorded, warm-cache stated.
- [ ] CI tracks scan-time regression on that tree.

### Out of scope

Anything that writes. No targets, no providers, no deletion planning.

---

## Phase 2 — Targets and planning

**Goal:** cleanrs tells you exactly what it *would* clean, how much that would
return, and why — while remaining incapable of deleting anything. Ships as v0.2.

This phase is deliberately read-only despite implementing the entire deletion
pipeline. Every safety mechanism is built and tested here, against a tool that
cannot yet act on them. That inverts the usual order and it is intentional: the
invariants get their test suite before there is any pressure to ship a working
`--apply`.

### Work

**Target model**
- The property model (§6.1) and `class` shorthand with defaults (§6.2).
- Embedded default manifest; `~/.config/cleanrs/targets.toml`; explicit
  `--config` with the trust rules (§9.1). **No implicit working-directory
  config, ever.**
- Trust store keyed to canonical path plus content hash.
- CLI target specs: compact `PATH:CLASS[:METHOD]`, the long form, and
  `--target-argv` as a JSON array. No shell anywhere.
- `--only`, `--exclude`, `--targets-only-cli`, `--no-builtin-targets`,
  `--export`.

**Detection**
- Path, marker, and command detectors, scoped.
- Staleness via marker and directory mtime, never `atime` (§8.5).
- Workspace awareness: Cargo workspace roots, shared `CARGO_TARGET_DIR`, no
  duplicate cleans of the same directory.

**Safety pipeline — implemented and tested, nothing executed**
- All ten path invariants (§8.2) as a checked pipeline every candidate passes.
- Cloud and dataless refusal at candidate level, not just target roots.
- `user_data` unreachable from any level.
- Bounded unattended deletion limits.

**Providers — inspect and plan only**
- The four-operation interface (§7.1), with `execute` and `verify` unimplemented.
- Provider identity and context pinning captured in every plan.
- Coverage for the §7.3 baseline: Homebrew, Docker Desktop and OrbStack as
  distinct contexts, Cargo, pnpm, npm/npx, Bun, Xcode, VMware, Claude Cowork,
  wallpaper cache. Swap stays a `doctor` diagnostic.

**Commands**
- `targets` — detected targets with sizes, intervals, and confidence.
- `clean` — plans and prints. `--apply` is accepted and **refuses with a clear
  message** that execution lands in Phase 3.

### Exit criteria

- [ ] Property tests: no target resolution at any level yields an action against
      `recoverability = user_data` without explicit naming.
- [ ] A `node_modules` planted inside a synced cloud folder, matched by an
      ordinary correctly-scoped detector, is refused on dataless status alone.
- [ ] A repository containing `cleanrs.toml` with a `run` target does not
      execute it under any invocation lacking `--config`.
- [ ] Adversarial path tests pass: symlinked ancestors, `..` traversal, targets
      replaced between scan and plan, `children_only` normalization widening.
- [ ] A Docker plan inspected against OrbStack stays pinned to that context and
      aborts if the default changes.
- [ ] Every §7.3 row appears in `targets` or `doctor` output.
- [ ] `clean --apply` is verifiably inert.

---

## Phase 3 — Execution and verification

**Goal:** cleanrs reclaims space and proves what it reclaimed. Ships as v0.3.
This is the first release that can destroy data.

### Work

**Execution**
- Permanent deletion for regenerable targets; `openat`-relative descent that
  never follows symlinks.
- Revalidation of device, inode, and type immediately before each unlink (§8.2
  items 5–6), aborting the target on mismatch.
- `--trash` as opt-in recovery mode, accounted as *moved, not reclaimed*.
- Cloud eviction as a distinct operation from deletion, if Phase 0 supported it.
- Provider `execute` and `verify`; exact argv printed; `--yes` for unattended.
- Refuse to run as root without explicit override.

**Verification and the journal**
- All seven journal fields (§11), including `container_free_delta` and
  `bytes_evicted`.
- The five named discrepancy classes (§11.1), with the open-descriptor
  pre-flight check if Phase 0 showed it to be cheap.
- `history` reads it back, explains shortfalls, rotates, and supports path
  redaction.
- Estimate-versus-observed recorded per run as the §3.3 calibration loop.

### Exit criteria

- [ ] Every destructive test runs against sandboxed fixture trees. No test ever
      touches a real home directory.
- [ ] Deleting a file held open by a live process reports the shortfall and
      names the discrepancy class rather than claiming success.
- [ ] `--trash` never reports reclaimed bytes.
- [ ] Across the fixture suite, observed deltas fall within predicted intervals;
      any below-floor result is investigated before release.
- [ ] Journal rotation and redaction verified.
- [ ] A dry run and its subsequent `--apply` are proven to plan identically, or
      the divergence is surfaced.

---

## Phase 4 — Daily driver

**Goal:** the tool you actually keep installed. Ships as v1.0.

Three independent tracks, merged into one phase because none is safety-critical
and all three depend only on a stable Phase 3 core. Order them by preference;
the TUI is the most enjoyable and the least load-bearing.

**TUI** — ratatui browser over the arena tree. Interval and confidence displayed
inline, not hidden. Navigation, sorting, filtering, and a path to `targets` and
plan review. Read-only first; any deletion path goes through the same Phase 2
pipeline with no shortcuts.

**Index** — the §10 invariant holds: the index caches the tree, not the verdict.
Durable per-file facts only; private size tagged with a snapshot generation
token; plan-time estimates always re-derived. FSEvents as a conservative
dirty-root set, per volume, with whole-index invalidation whenever completeness
cannot be proven. Format chosen by benchmark (§10.4), with versioning,
validation before access, atomic replacement, and restrictive permissions.

**Scheduling** — launchd agent. Weekly `safe --apply --yes`; monthly `standard`
that **proposes without applying**. Pinned config path, never the working
directory, never Trash. Notifications summarising reclaimed space.

### Exit criteria

- [ ] The FSEvents fallback-to-full-scan path is exercised by tests, not just
      documented.
- [ ] Snapshot-churn test: cached figures degrade correctly with zero events
      fired.
- [ ] A corrupted or truncated index file is rejected before any access.
- [ ] Index file permissions restrict access to the owner.
- [ ] A scheduled run cannot load working-directory config or use Trash, by
      test.
- [ ] TUI degrades cleanly on non-TTY and respects `NO_COLOR`.

---

## Phase 5 — Linux (optional)

Only worth starting once macOS is stable and someone actually wants it.

`getdents64` + `statx`; `st_blocks`; reflink detection via FIEMAP; optionally
`io_uring` for the same syscall-batching win. But note that the platform seams
are not confined to the filesystem module: scheduling (systemd timers), events
(inotify/fanotify), Trash semantics, default paths, permission models, mount
topology, and provider availability all differ.

Expect the accounting model to need genuine rework, not a port — btrfs and XFS
share blocks by different mechanisms with different metadata, and the
floor/ceiling formulation may not carry over.

---

## Standing rules

These apply in every phase:

- Accounting logic gets tests before implementation. It is the product.
- No filesystem test touches a real home directory.
- Benchmarks are warm-cache only and say so.
- Every number published anywhere carries its `[ref]` / `[exp]` / `[target]` /
  `[hyp]` label until it has been measured by this project.
- macOS claims are verified against the installed SDK, not secondary sources.
- When a phase invalidates something in VISION.md, update VISION.md in the same
  change — state the correction rather than silently deleting the old claim.

---

## Implementation progress — 2026-08-11

Phase 0 remains open because the repository does not have an explicitly
prepared evicted-cloud fixture and automated snapshot mutation would change the
developer machine. The exact measured/pending boundary is recorded in
`docs/findings-phase0.md`; no missing experiment is presented as a result.

Implemented Phase 0/1 foundations:

- Two-crate Rust workspace (`cleanrs-core` and `cleanrs`) with the prescribed
  `fs`, `accounting`, `targets`, `providers`, `index`, and `journal` modules.
  Later-phase modules are deliberately inert so this build has no write path.
- SDK-verified `getattrlistbulk` FFI and packed-record parser, including returned
  masks, entry errors, hardlink counts, private size, clone metadata,
  `SF_FIRMLINK`, and `SF_DATALESS`; safe per-directory `lstat` fallback.
- Parallel directory traversal with bounded active workers, a `u32`-indexed
  arena, flat interned names, no symlink following, mount-boundary refusal,
  firmlink refusal, excludes, and counted coverage errors with unknown size.
- Apparent and allocated totals, hardlink deduplication, clone-family ceiling,
  private-size floor, derived confidence, and a real macOS clone fixture.
- `scan` with JSON, all four metric views, allocated default, depth, minimum
  size, excludes, explanations, mount opt-in, and worker control.
- Container-oriented `doctor` with distinct System/Data/VM/Nix rows, APFS shared
  capacity/free space, optional explicit-scope reconciliation via `--scan`,
  snapshot presence, explicit unknown purgeable/FDA state, and diagnostic-only
  swap messaging.
- Shell completions, man-page generation, CI on macOS and Linux, release builds,
  dependency updates, a fixed synthetic-tree generator, and scheduled
  warm-cache comparisons against `du`, `diskus`, `dust`, and `gdu`.

Exit-criterion status:

- [x] `docs/findings-phase0.md` records the environment, results, non-results,
      and current promise.
- [ ] Dataless materialization and eviction test (hard gate; fixture required).
- [ ] Snapshot/private-size churn and snapshot-generation-token experiment.
- [x] One-tier attribute request decision recorded, provisional on benchmarks.
- [x] `scan` agrees byte-for-byte with macOS `du -sk` on a no-clone fixture and
      on the repository at the recorded measurement.
- [x] Mutual-clone unit and real-`clonefile` fixtures retain a non-zero ceiling.
- [ ] Quiesced container-delta harness across every APFS fixture.
- [ ] Root/firmlink acceptance test on a disposable macOS installation.
- [x] `doctor` explicitly inventories VM swap and an existing `/nix` volume.
- [ ] Live cloud materialization acceptance test.
- [ ] Complete the four-tool `hyperfine` benchmark. Preliminary warm-cache
      cleanrs/`du` numbers and the fixed-tree scheduled workflow are present.
- [x] CI runs formatting, strict Clippy, tests, and release builds on macOS and
      Linux; scheduled CI records warm-cache scan performance.

### Phase 1 terminal UX — 2026-08-12

- [x] The core scanner publishes ordered, monotonic directory, entry, and
      allocated-byte progress snapshots without depending on terminal code.
- [x] Interactive `scan` and `doctor --scan` runs show an indeterminate progress
      indicator with live counters; unknown total work is not presented as a
      fake percentage.
- [x] Human `scan` output uses metric-aware result tables, confidence colors,
      compact relative paths, a scan summary, and dedicated safety/coverage
      diagnostics.
- [x] Human `doctor` output groups container capacity, volumes, Data attribution,
      environment diagnostics, and explanatory notes into separate tables.
- [x] TTY output uses Unicode and semantic colors; redirected output uses ASCII;
      `NO_COLOR` is respected; JSON, completions, and man-page output remain
      animation- and decoration-free.

### MSRV compatibility — 2026-08-12

- [x] Pinned `globset` to 0.4.19 because 0.4.20 requires Rust 1.88 while the
      workspace promises Rust 1.85 support.
- [x] Verified `cargo check --workspace --all-targets` with rustc 1.85.0 rather
      than relying only on Cargo's dependency metadata.
- [x] Pinned `comfy-table` to 7.1.4; 7.2 uses let-chains unavailable on Rust
      1.85. Added platform-specific compilation guards so Linux builds do not
      emit warnings for macOS-only `diskutil` and filesystem-flag helpers.

### Phase 2 targets groundwork — 2026-08-12

This is an intentionally read-only vertical slice started at the user's
request. It does **not** close the open Phase 0/1 gates above and does not claim
that Phase 2 or v0.2 is complete.

- [x] Replaced the inert `targets` module with the §6 property model: class
      expansion, independent recoverability/rebuild/network/automation fields,
      and cleanup-level selection that keeps `user_data` unreachable unless a
      stable target is explicitly named.
- [x] Added an embedded initial workspace catalog for Cargo `target`,
      `node_modules`, `.next`, and `dist`, using marker-scoped detection and
      deduplicated candidate paths.
- [x] Added `cleanrs targets [PATH]` with an explicit scope (working directory
      by default, never the home directory), `--json`, `--only`, `--exclude`,
      worker control, TTY-aware progress, and human tables.
- [x] Each detected target is APFS-aware sized from the existing scan and shows
      apparent/allocated bytes, a reclaimability interval, derived confidence,
      artifact age, staleness policy, and safety status.
- [x] Candidate validation proves marker artifacts remain strictly below their
      project root and refuses synced-cloud locations, dataless descendants,
      and mount-boundary crossings. Refused targets stay visible in output.
- [x] Tests cover class expansion, level-independent `user_data` protection,
      marker detection/sizing, dataless refusal independent of location,
      machine-readable and human CLI output, unknown filters, and proof that a
      working-directory `cleanrs.toml` is ignored.
- [x] Target presentation collapses candidates nested beneath an already
      detected artifact root (notably package markers and `dist` directories
      inside `node_modules`), preventing redundant actions and double-counted
      output. Results sort by reclaimable ceiling; human output shows the top
      25 by default, while `--top N` and `--min-size SIZE` provide explicit
      control and JSON remains unlimited unless filtered.
- [ ] Remaining target-model work: TOML/CLI definitions and overrides, explicit
      config trust decisions keyed by canonical path and content hash, export,
      path/command detectors, and the complete §8.2 invariant pipeline.
- [ ] Remaining detection work: Cargo workspace/shared-target resolution,
      configured `CARGO_TARGET_DIR`, provider inspection/planning, and every
      §7.3 known-location acceptance row.
- [x] Added `clean [PATH]` planning with `safe`/`standard`/`aggressive` level
      selection, staleness enforcement and override, `--only`/`--exclude`,
      JSON and human review output, clone/hardlink-deduplicated reclaimability
      bounds over the complete selection, and an explicit dry-run result. Each
      planned candidate captures device, inode, and type for later Phase 3
      revalidation.
- [x] `clean --apply` is accepted but refuses before scanning or reaching any
      write API. CLI fixture tests prove both ordinary dry-runs and refused
      apply requests leave candidate trees untouched.
- [ ] Phase 3 execution remains gated: no deletion, Trash, provider execution,
      verification, or journal write API exists until the remaining Phase 2
      safety/configuration/provider exit criteria and earlier hard gates are
      closed.
