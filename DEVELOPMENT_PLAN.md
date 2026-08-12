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

## Implementation progress audit — 2026-08-12

This section is the source of truth for implementation status. It was rebuilt
from the current source, tests, manifests, and workflows rather than inferred
from module names or earlier progress notes.

Legend: `[x]` fulfilled by the current repository; `[~]` partially implemented
or covered only by a narrower test than the criterion requires; `[ ]` missing.

| Phase | Status | Summary |
|---|---|---|
| 0 | **In progress — hard gate open** | Findings and a provisional attribute-request decision exist; live dataless and snapshot experiments remain unrun. |
| 1 | **In progress — read-only MVP implemented** | `scan` and a useful `doctor` exist, but ground-truth reclamation, live cloud, root-topology, benchmark, and regression gates remain open. |
| 2 | **In progress — workspace-target slice implemented** | Built-in marker targets and dry-run planning work; configuration, complete safety invariants, providers, and acceptance-baseline coverage are missing. |
| 3 | **Not started** | `--apply` refuses before scanning; providers and journal are inert modules. |
| 4 | **Not started** | Terminal presentation exists, but there is no TUI command, index implementation, or scheduler. |
| 5 | **Not started** | Portable metadata fallback permits development/tests on Linux, but Linux product support is not implemented. |

The phase-ordering rule at the top of this document has not been followed
strictly: Phase 1 product code and Phase 2 groundwork were added while the Phase
0 hard gate remained open, and Phase 2 began before Phase 1 exit criteria were
met. This work is useful, but **no phase is complete and no v0.1/v0.2 exit may
be claimed yet**.

### Verification snapshot

Run locally on 2026-08-12:

- [x] `cargo fmt --all -- --check`
- [x] `cargo clippy --workspace --all-targets -- -D warnings`
- [x] `cargo test --workspace --all-targets` — 35 passed, 0 failed (17 core,
      4 binary-unit, 14 CLI integration tests).
- [x] The workspace declares Rust 1.85 and pins `globset` 0.4.19 and
      `comfy-table` 7.1.4 for that MSRV. CI contains an explicit Rust 1.85
      `cargo check` job.
- [~] CI currently runs its test matrix on Ubuntu only; the `macos-15` matrix
      entry is commented out. macOS-only `getattrlistbulk`, clone, `du`, and
      topology behavior therefore are not continuously tested.
- [~] `.github/workflows/benchmark.yml` defines the requested five-command
      warm-cache `hyperfine` run (`cleanrs`, `du`, `diskus`, `dust`, `gdu`), but
      it is manual-dispatch only, has no checked-in result, and does not compare
      against a regression threshold. Its schedule is commented out.

### Phase 0 audit — Feasibility research

Implemented or recorded:

- `docs/findings-phase0.md` records the Apple Silicon/macOS/APFS environment,
  installed-SDK constants, completed measurements, unrun experiments, the
  current promise, and a provisional one-tier request decision.
- The native backend requests returned masks, entry errors, name, device, type,
  mtime, flags, inode/link data, apparent/allocated/private sizes, clone ID, and
  clone refcount in one `getattrlistbulk` call. `SF_FIRMLINK` and `SF_DATALESS`
  match the installed SDK values.
- Unit fixtures cover hardlink set accounting and the mutual-clone interval
  algorithm. A macOS test creates a real 1 MiB clone and verifies that clone
  metadata is observed.
- A fixed synthetic-tree generator and preliminary `cleanrs`/`du` timings
  exist. Those measurements show that the current implementation does not beat
  `du`; they are not the required full benchmark.
- Container/Data/VM topology was observed on the development machine.

Missing research:

- No controlled evicted iCloud/File Provider fixture has been scanned, so zero
  materialization and zero allocation change are unproven. Eviction semantics
  are also untested. This remains the hard gate on scanning a real home tree.
- Snapshot creation/expiry, untouched-file `PRIVATESIZE` changes, absence of
  FSEvents, and snapshot-generation-token cost are unmeasured.
- There is no repeated, quiesced before/after APFS container-delta harness for
  clones, divergence, partial divergence, hardlinks, sparse files, and
  snapshots. The interval is therefore not ground-truth validated.
- Interval tightness on a real developer home directory has not been measured.
- Minimal-versus-extended attribute-mask cost has not been A/B benchmarked, so
  the one-tier decision is explicitly provisional.
- Open-descriptor sweep cost and privilege requirements are unmeasured.
- Topology observations do not yet cover all required firmlink and volume-group
  cases on a disposable system.

Exit criteria:

- [x] `docs/findings-phase0.md` states what was measured, the environment,
      non-results, and the resulting limits on the current promise.
- [ ] The live dataless materialization test passes.
- [~] VISION.md reflects the conservative current promise, but §3 has not
      promoted the tested portions from hypotheses to `[exp]`, and the central
      interval guarantee remains unvalidated.
- [x] A provisional one-tier decision is recorded, with an explicit trigger for
      reopening it after the missing A/B benchmark.

**Phase 0 is not complete.** The dataless hard gate alone prevents completion.

### Phase 1 audit — Read-only truth

Implemented:

- Two-crate workspace with filesystem code behind the `FileSystem` trait.
- Native macOS `getattrlistbulk` parser plus per-directory `readdir`/`lstat`
  fallback; non-macOS portable backend.
- Parallel blocking directory workers, capped worker count, monotonic progress
  counters, no symlink following, mount-boundary refusal by default, a root/Data
  firmlink rule, built-in pseudo-filesystem excludes, and counted coverage
  errors with unknown size.
- A `u32`-indexed arena with parent/child/sibling links and a flat name buffer.
- Apparent/allocated/private metadata, hardlink deduplication, clone-family
  ceiling, private-size floor, selected-subtree union accounting, and confidence
  derived from interval width and coverage.
- `scan` supports JSON, metric selection, allocated default ranking, depth,
  minimum size, excludes, one-path explanation, mount opt-in, worker control,
  and TTY-aware progress/output.
- `doctor` reports shared container capacity/free space, System/Data/VM and an
  existing `/nix` mount, snapshot count, swap as diagnostic-only, optional
  explicit-scope attribution, unexplained space, and explicit unknowns for
  purgeable space and Full Disk Access.
- Shell completions, man-page output, semantic terminal colors, colorless
  redirected output, and `NO_COLOR` handling.

Partial or missing implementation:

- The packed-record parser has bounds checks but no dedicated constructed-record
  fixture tests for masks, offsets, truncation, and malformed records.
- Fallback occurs after a bulk call fails for a directory; capabilities are not
  detected/cached per mount.
- `doctor` probes a hard-coded root/Data/VM/Nix set rather than enumerating every
  volume in an APFS container. It has no `--container`/`--all-containers`,
  purgeable measurement, effective Full Disk Access probe, provider diagnostics,
  or index freshness diagnostics.
- Active worker count is bounded, but the directory channel and outstanding job
  count are not. Worker defaults are CPU-count based, not storage aware.
- There are no sharded inode/clone sets. Accounting uses ordinary local
  `HashMap`/`HashSet` collections after traversal.
- Firmlink logic is a fixed path list plus flags, not discovered volume-group
  topology. Its test exercises the predicate only; it does not scan `/` and
  prove absence of a duplicated home subtree.
- `--explain` selects one scanned path and reports the interval's generic reason;
  it does not identify the files/families that widened the interval.
- Dataless flags are collected and shown, but non-materialization is not proven.
- Snapshot coverage does not yet alter intervals or confidence beyond a
  separately reported snapshot count.

Exit criteria:

- [x] A macOS fixture test compares no-clone allocated bytes byte-for-byte with
      `du -sk`; the Phase 0 findings also record agreement on this repository.
- [~] Mutual-clone unit and real-`clonefile` tests retain a non-zero,
      clone-deduplicated ceiling, but they do **not** delete the fixture or prove
      that the ceiling equals real reclamation.
- [ ] The ground-truth container-delta harness brackets every fixture.
- [~] Root/Data topology has predicate coverage, but no acceptance scan of `/`
      proves that the home directory is not duplicated.
- [x] `doctor` inventories `/System/Volumes/VM` as OS-managed swap and includes
      `/nix` when that mount exists.
- [ ] The live cloud materialization test passes.
- [~] The complete warm-cache benchmark command exists in a workflow, but no
      complete recorded result is present.
- [ ] CI tracks scan-time regression. The benchmark workflow records a manual
      sample only and enforces no baseline or threshold.

**Phase 1 is not complete.** Its read-only command surface is a useful MVP, but
six acceptance gates are missing or only partially covered.

### Phase 2 audit — Targets and planning

Implemented:

- The class/property model and `safe`/`standard`/`aggressive` selection logic,
  including level-independent protection for `user_data` unless an ID is named.
- Four embedded marker targets: Cargo `target`, `node_modules`, `.next`, and
  `dist`, with class defaults/overrides and staleness thresholds.
- One explicit workspace scan detects, deduplicates, sizes, and ranks candidates;
  nested candidates inside an outer artifact are collapsed.
- `targets [PATH]` supports JSON, `--only`, `--exclude`, `--top`, `--min-size`,
  worker control, progress, and human tables. Refused candidates remain visible.
- Candidate checks require a marker artifact to be a direct, strict descendant
  of its detected project, reject the root/home, reject known synced-cloud
  prefixes, and inspect descendants for dataless and mount-boundary flags.
- `clean [PATH]` plans by level and staleness, computes one deduplicated interval
  over the complete selection, captures device/inode/type, and prints an
  explicit dry-run result.
- `clean --apply` refuses before scanning. There is no reachable filesystem or
  provider write API.

Missing target/configuration work:

- No TOML loader, embedded manifest format, user config, explicit `--config`,
  trust store, canonical-path/content-hash trust decision, or override merge.
- No compact `--target`, long-form target flags, `--target-argv`,
  `--targets-only-cli`, `--no-builtin-targets`, or `--export`.
- Only marker detectors exist. Path/command detectors and configurable scopes
  are absent.
- Staleness uses only the artifact directory mtime, not both marker and artifact
  signals or ecosystem-specific evidence.
- Cargo workspace roots, shared/custom `CARGO_TARGET_DIR`, and duplicate
  workspace clean prevention are absent.

Missing safety work:

- The §8.2 pipeline is incomplete: no `~` expansion/nearest-existing-ancestor
  canonicalization, general empty/root/home/mount-root rejection for arbitrary
  definitions, `children_only` widening proof, plan-time replacement detection,
  openat-relative traversal model, or bounded unattended deletion limit.
- Device/inode/type are captured but cannot yet be revalidated because there is
  no apply path. There are no adversarial tests for `..`, symlinked ancestors,
  replacement races, or `children_only` widening.
- Dataless refusal has a deterministic unit fixture, not the required live cloud
  safety evidence. Ubiquitous status beyond `SF_DATALESS` is not implemented.
- The user-data test is deterministic rather than property-based, and it tests
  level selection directly rather than arbitrary resolved target definitions.

Missing provider/acceptance work:

- `providers.rs` is an inert `AVAILABLE = false` placeholder. There is no
  inspect/plan interface, identity/context capture, or provider implementation.
- Homebrew, Docker Desktop, OrbStack, Cargo, pnpm, npm/npx, Bun, Xcode, VMware,
  Claude Cowork, and wallpaper-cache acceptance rows are absent. Swap is the
  only §7.3 special area represented, through `doctor`.
- The broader home, Library, Developer, cloud-eviction, VM, runtime, and cache
  classification baseline is not covered by `targets`/`doctor` acceptance tests.

Exit criteria:

- [~] A deterministic test proves no cleanup level selects a
      `TargetClass::Irreplaceable` target unless explicitly named; the required
      property test over resolved target definitions is absent.
- [x] An ordinary `node_modules` marker match with a dataless descendant is
      refused without relying on its location. This is an in-memory fixture,
      not the still-open Phase 0 live-materialization test.
- [~] A CLI test proves an unrequested working-directory `cleanrs.toml` is
      ignored and cannot execute. Explicit config loading and command targets do
      not exist yet, so the complete trust-boundary behavior is not testable.
- [ ] The full adversarial path suite passes.
- [ ] An OrbStack-inspected Docker plan stays pinned and aborts on context or
      daemon identity change.
- [ ] Every VISION.md §7.3 row appears in `targets` or `doctor` output.
- [x] `clean --apply` is verifiably inert; a CLI fixture asserts the candidate
      survives the refusal.

**Phase 2 is not complete.** The current slice is a sound read-only workspace
planner, not the complete v0.2 target/configuration/provider system.

### Phase 3 audit — Execution and verification

Status: **not started**.

- [x] The current command is safe by absence of execution: `--apply` exits with
      an error before scanning, and dry-run fixture tests preserve all files.
- [ ] No permanent deletion or openat-relative descent.
- [ ] No apply-time device/inode/type revalidation or replacement-race abort.
- [ ] No Trash mode, cloud eviction, root refusal, provider execution, provider
      verification, exact-argv display, or unattended confirmation behavior.
- [ ] `journal.rs` is an inert `AVAILABLE = false` placeholder. None of the
      seven journal fields, discrepancy classes, rotation, or redaction exist.
- [ ] There is no `history` command and no estimate-versus-observed calibration
      loop.
- [ ] None of the Phase 3 exit criteria are met. The existing inert-apply test
      is a Phase 2 gate, not evidence of destructive execution correctness.

### Phase 4 audit — Daily driver

Status: **not started**, with reusable terminal groundwork.

- [~] `scan`, `doctor`, `targets`, and `clean` have polished terminal tables,
      progress indicators, redirected-output behavior, semantic colors, and
      `NO_COLOR` support. This is CLI presentation, not the ratatui TUI required
      by Phase 4.
- [ ] No `tui` command, ratatui browser implementation, or interactive
      navigation, sorting, filtering, and plan review.
- [ ] `index.rs` is an inert `AVAILABLE = false` placeholder. There is no index
      format, validation, snapshot token, FSEvents integration, invalidation,
      permissions enforcement, or `index` command.
- [ ] No launchd integration, notifications, pinned scheduled configuration, or
      `schedule` command.
- [~] Non-TTY and `NO_COLOR` behavior is tested for ordinary CLI tables, but not
      for a TUI because none exists.
- [ ] None of the Phase 4 exit criteria are fully met.

### Phase 5 audit — Linux

Status: **not started**.

- [~] The filesystem trait and portable `readdir`/`lstat` fallback compile on
      non-macOS targets, and CI is configured to check/test them on Ubuntu.
      Platform-specific helpers are compilation-guarded.
- [ ] No `getdents64`/`statx`, reflink/FIEMAP accounting, Linux mount topology,
      inotify/fanotify, systemd scheduling, Trash semantics, Linux provider
      policy, or Linux-specific accounting model.
- [ ] The current portable fallback is development portability, not supported
      Linux product behavior.

### Next valid completion path

1. Close Phase 0 with explicitly prepared live dataless and snapshot fixtures,
   the container-delta accounting harness, interval-tightness measurement,
   attribute-mask A/B benchmark, and descriptor-sweep measurement; then update
   `docs/findings-phase0.md` and VISION.md labels.
2. Close Phase 1 with parser fixtures, discovered container topology, live
   root/cloud acceptance tests, a complete recorded benchmark, and an enforced
   CI regression check.
3. Finish Phase 2 configuration/trust, all ten path invariants, workspace-aware
   detection, provider inspect/plan, and the full §7.3 acceptance matrix before
   enabling any Phase 3 write path.
