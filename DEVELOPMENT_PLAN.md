# cleanrs — Development Plan

This plan is organized around the reason cleanrs exists: make routine developer
storage maintenance on the author's Macs fast, understandable, and safe enough
to automate.

cleanrs is not trying to replace `gdu`, become a general consumer cleaner, or
prove every detail of APFS allocation. It combines a high-performance scanner
with knowledge of developer tools: where they store data, how to inspect it,
which native cleanup commands they expose, and how risky each cleanup action is.

Performance engineering remains a first-class goal. The project is also a place
to learn and apply low-level filesystem techniques: batched metadata access,
bounded parallel traversal, compact memory layouts, incremental indexing, and
careful measurement. Performance work must preserve safety and truthful output.

## Utility loop

The daily workflow is:

1. `scan` explains large files and directories quickly.
2. `doctor` explains storage that an ordinary walk cannot attribute.
3. `targets` inventories known developer storage and available cleanup actions.
4. `clean` plans or executes those actions at a chosen severity.
5. `schedule` handles routine safe maintenance.
6. `tui` provides one overview for scanning, targets, project findings, and
   cleanup review.

## Cleanup severity

Severity selects individual actions, not merely applications or directories.
One provider may expose actions at several levels.

| Level | Intended meaning | Examples |
|---|---|---|
| `safe` | Downloadable caches and cheap generated output with low surprise | narrowly scoped package-manager caches, stale build output |
| `standard` | Broader generated state with noticeable rebuild cost | old toolchains, stopped runtime/build state, older project artifacts |
| `aggressive` | Large unused state whose removal may disrupt a workflow | unused images, dormant environments, extensive Xcode state |
| explicit-only | User data or unusually consequential state | Docker volumes, VM disks, repositories, unsynced project data |

No severity level reaches explicit-only actions. They require a named target or
action. Scheduled application is restricted to `safe`; a scheduled broader run
may produce a plan but must not apply it.

**Correction:** an earlier version listed Homebrew downloads as a `safe`
example. The supported native `brew cleanup` operation also removes old
installed versions, so cleanrs initially classifies that mixed action as
`standard`. It must not be scheduled as a `safe` download-cache-only action.

## Phase overview

| Phase | Delivers | Current state |
|---|---|---|
| 0 | Honest safety and accounting foundation | **Practical baseline complete; deeper research deferred** |
| 1 | Fast read-only scanner and diagnostics | **Useful MVP implemented; performance and UX work remains** |
| 2 | Developer-storage inventory and cleanup plans | **In progress; workspace and Homebrew planning slices implemented** |
| 3 | Cleanup execution and lightweight history | **Not started; inert apply path exists** |
| 4 | Project hygiene and stale-project intelligence | **Not started** |
| 5 | Index, scheduling, and ratatui daily-driver UI | **Terminal groundwork only** |
| 6 | Extensibility and optional portability | **Deferred** |

Phases define dependency order, but small supporting improvements may land when
they directly unblock the active phase. A phase does not need laboratory-grade
proof; it needs reproducible fixtures, truthful limitations, and confidence
that the commands are useful on the development machines.

---

## Phase 0 — Practical filesystem foundation

**Goal:** establish enough macOS behavior to scan and plan safely without
turning the project into an APFS research program.

### Already completed

- Installed-SDK constants and semantics were checked for
  `getattrlistbulk`, `ATTR_CMNEXT_PRIVATESIZE`, clone metadata,
  `SF_FIRMLINK`, and `SF_DATALESS`.
- The accounting model distinguishes apparent, allocated, private, and
  estimated-reclaimable values. Estimates are intervals with confidence.
- Hardlink and mutual-clone fixtures cover the important set-accounting cases.
- A real clone fixture verifies that macOS clone metadata reaches the scanner.
- An explicitly prepared 32 MiB iCloud Drive placeholder remained dataless with
  zero allocated blocks across the initial traversal and five repeated scans.
- The findings and limits are recorded in `docs/findings-phase0.md`.
- A one-tier extended attribute request is the current implementation choice.

### Reworked from the original plan

- Exact container-free-space reclamation is no longer a release gate. APFS
  container deltas are noisy, and cleanrs will not claim exact recovery where
  it only knows removed allocation or a provider-reported value.
- Snapshot mutation experiments are deferred. Snapshot presence lowers
  certainty and is surfaced by `doctor` instead of blocking the utility.
- Validation of every third-party File Provider is not required. Unverified
  synced locations remain conservatively refused.
- Descriptor sweeps and interval-tightness studies are optional enhancements,
  not prerequisites for inventory and cleanup orchestration.

### Practical completion criteria

- [x] Metadata-only traversal is demonstrated not to materialize the prepared
      iCloud fixture.
- [x] Ordinary no-clone fixture allocation agrees with macOS `du`.
- [x] Clone and hardlink uncertainty is represented honestly rather than as an
      exact byte promise.
- [x] Unmeasured behavior is documented as a limitation.

**Status: complete for current utility needs.** Reopen individual research
questions only when they enable a concrete capability or explain a real bug.

---

## Phase 1 — Fast read-only understanding

**Goal:** quickly answer “what is taking space?” while providing the filesystem
facts needed by targets and cleanup planning.

### Already completed

- `cleanrs-core::fs::FileSystem` platform seam.
- Native macOS `getattrlistbulk` traversal with per-directory
  `readdir`/`lstat` fallback.
- Parallel blocking workers without an async runtime.
- No symlink following, no mount crossing by default, firmlink/Data-volume
  duplicate protection, pseudo-filesystem skips, and counted coverage errors.
- Compact `u32`-indexed arena and flat name storage.
- Apparent, allocated, private, and interval accounting with hardlink and clone
  deduplication.
- `scan` JSON and human output, metric selection, depth/minimum-size/exclusion
  filters, path explanation, worker controls, and progress reporting.
- `doctor` container/Data/VM diagnostics, snapshot count, `/nix` recognition,
  explicit scan attribution, and honest unknowns.
- TTY-aware tables, semantic color, `NO_COLOR`, shell completions, and man-page
  generation.

### Needs reworking or completion

- Add a first-class largest-files view alongside the directory tree.
- Make `--explain` identify the concrete coverage, clone, hardlink, snapshot, or
  fallback reason affecting a result.
- Bound the directory queue as well as active workers.
- Choose worker defaults using storage characteristics and measured results,
  not CPU count alone.
- Add constructed packed-record parser fixtures for returned masks, offsets,
  truncation, and malformed records.
- Cache per-mount backend capabilities instead of retrying failed bulk requests.
- Improve APFS container/volume discovery in `doctor`; retain explicit unknowns
  where the OS does not expose an inexpensive answer.
- Keep scanner output useful for large real trees without trying to reproduce
  every interaction pattern of `gdu`.

### Performance work

- Maintain a deterministic synthetic-tree generator with mixed depths, file
  counts, hardlinks, sparse files, and representative developer layouts.
- Track wall time, peak resident memory, entries/second, and metadata fallbacks.
- Benchmark warm-cache and cold-ish-cache runs separately and label them.
- Compare against earlier cleanrs commits for regression detection. `du`, `gdu`,
  `diskus`, and `dust` are useful reference points, not pass/fail competitors.
- Profile before changing concurrency, allocation, or syscall strategies.
- Add a macOS benchmark lane when CI cost and availability are reasonable;
  retain a locally runnable benchmark command regardless of CI.

### Practical completion criteria

- [x] Plain fixture allocation agrees with `du`.
- [x] Symlinks are not followed and mount boundaries are explicit.
- [x] Permission failures lower coverage rather than inventing byte counts.
- [ ] Largest-file output is available in both human and JSON modes.
- [ ] Packed-record parser adversarial fixtures pass.
- [ ] The scanner has a checked-in local performance baseline and a simple
      regression comparison procedure.
- [ ] A representative large local directory can be scanned with bounded memory
      and no correctness regression.

This phase may continue receiving performance improvements throughout the
project; its remaining items do not block read-only Phase 2 inventory work.

---

## Phase 2 — Developer-storage inventory and cleanup planning

**Goal:** automatically map known developer storage, show why it exists, and
produce exact cleanup plans without executing them.

This is the next development priority and the main differentiator from ordinary
disk analyzers.

### Already completed

- Target classes and `safe`/`standard`/`aggressive` selection.
- Level-independent protection for `user_data`/explicit-only targets.
- Workspace marker detection for Cargo `target`, `node_modules`, `.next`, and
  `dist`.
- Candidate sizing, ranking, staleness, deduplication, and nested-target
  collapsing.
- Candidate-level cloud/dataless and mount-boundary refusal.
- `targets` human/JSON output with filters.
- `clean` dry-run planning, selection explanations, a deduplicated interval,
  and captured device/inode/type.
- `clean --apply` refuses before scanning and cannot currently mutate data.
- Unified workspace/provider inventory items with owned IDs, provider identity,
  typed usage sources, exact direct-delete or argv-array operations,
  consequences, evidence, and action-level minimum severity.
- A bounded read-only provider host with canonical executable discovery,
  subprocess timeout/output limits, and injectable command fixtures.
- Homebrew capability detection pinned to the resolved executable, version, and
  prefix, with routine cleanup, scrub, and autoremove represented separately.
- Homebrew cleanup uses the native dry-run estimate instead of treating the
  whole cache as reclaimable. Routine and scrub plans are mutually exclusive,
  and cleanup explicitly disables implicit autoremove so plans do not widen or
  double count.
- `targets` and `clean` combine workspace findings with machine-wide provider
  actions; `--workspace-only` suppresses provider inspection.

### Provider inventory

Start with tools actually present on the development Macs, then widen coverage:

- Homebrew (**initial slice implemented**): routine cleanup, scrub, and
  autoremove are distinct read-only plans. More granular formula/cask reporting
  may be added when it improves decisions without duplicating native output.
- Nix: store usage, roots/generations, and garbage-collection opportunities.
- pnpm, npm/npx, Yarn, and Bun: shared caches/stores and native prune commands.
- uv, pip, Poetry, and Conda: download caches, environments, and native cleanup
  actions with environments separated from disposable caches.
- Cargo/rustup: project targets, registry/git caches, and old toolchains.
- Docker Desktop and OrbStack: build cache, stopped objects, images, and volumes
  as separate actions and contexts.
- Xcode: DerivedData, archives, device support, simulator caches, and simulator
  user data as different risk classes.
- Gradle, Maven, CocoaPods, and other tools added when encountered locally.
- VMware/other VM disks and Claude Cowork storage as informational or
  explicit-only targets, not generic caches.

Each provider implements a read-only `detect`/`inspect`/`plan` path before any
execution exists. A planned command is stored as an executable plus argv array;
there is no shell.

### Target output contract

Every detected item or action should expose, where available:

- provider and target/action ID;
- path or provider context;
- measured allocated size or provider-reported usage;
- age/activity evidence;
- cleanup severity and recoverability;
- exact native command or direct-delete operation;
- expected rebuild/download/workflow consequence;
- confidence or an explicit unknown;
- refusal reason when it cannot safely enter a plan.

### Configuration, kept deliberately small

- Embedded built-in target registry.
- `~/.config/cleanrs/targets.toml` and explicit `--config PATH`.
- Never load configuration from the working directory.
- Enable/disable and include/exclude built-ins.
- Explicit CLI path targets and argv-array command targets.
- Export the resolved registry for inspection.
- Defer plugin standards, marketplaces, and complex trust stores until the
  author's use cases require them.

### Safety work before execution

- Canonicalize through the nearest existing ancestor.
- Reject empty paths, `/`, the home directory, and mount roots.
- Refuse symlinked ancestors and normalization that widens a target.
- Refuse synced-cloud and dataless candidates.
- Keep explicit-only/user data unreachable from severity selection.
- Capture filesystem identity for direct-delete candidates.
- Pin provider identity/context and exact argv in the plan.
- Put conservative count/byte limits on unattended plans.

### Practical completion criteria

- [x] Existing marker targets are detected, deduplicated, and sized.
- [x] A dataless descendant refuses an otherwise valid `node_modules` target.
- [x] Working-directory configuration is ignored.
- [x] `clean --apply` is inert in this phase.
- [ ] Homebrew, Nix, JS package managers, Python environment managers,
      Cargo/rustup, Docker/OrbStack, and Xcode appear in inventory when present.
- [x] Homebrew appears as separate cleanup, scrub, and autoremove actions with
      provider-reported/unknown estimates and pinned command plans.
- [ ] Each provider has fixture or command-output parser tests that do not
      require deleting real data.
- [ ] Severity is assigned per action; Docker build cache, images, and volumes
      cannot collapse into one generic Docker target.
- [ ] Explicit-only actions never enter a level-selected plan.
- [ ] Adversarial path tests cover traversal, symlink ancestors, target
      replacement metadata, mount roots, and cloud refusal.
- [ ] Human and JSON plans show the exact operation and consequence.

---

## Phase 3 — Cleanup execution and lightweight history

**Goal:** safely execute reviewed plans and leave enough evidence to understand
what happened.

### Already completed

- Dry-run is the default.
- `--apply` is accepted but refuses before scanning.
- Plans already capture device, inode, and file type for future revalidation.
- Fixture tests prove current dry runs do not delete candidates.

### Direct deletion

- Begin only with known regenerable directories that lack a suitable native
  cleanup command.
- Use descriptor-relative traversal that never follows symlinks.
- Revalidate device, inode, and type immediately before mutation.
- Abort an individual target on replacement or validation mismatch.
- Refuse root execution unless explicitly overridden for a justified action.
- Require `--apply`; require confirmation unless `--yes` is present.
- Keep all destructive tests inside unique temporary fixture directories.

### Provider execution

- Execute the exact executable and argv captured by the reviewed plan.
- Revalidate provider binary, identity, and context immediately before running.
- Stream provider output and record exit status.
- Use provider-native dry-run/preview modes where reliable.
- Never infer that provider-reported freed space equals host APFS recovery.
- Keep explicit-only actions named and interactive by default.

### Lightweight history

Record:

- timestamp and cleanrs version;
- selected level and explicitly named actions;
- target/provider identity, path/context, and exact argv;
- planned size or provider estimate;
- success, failure, skip, or validation abort;
- removed tree allocation or provider-reported result;
- concise stdout/stderr summary and error reason.

Call values what they are: “removed allocation,” “provider reported,” or
“estimated reclaimable.” Do not claim exact container recovery without a direct
measurement. Journal rotation and path redaction are useful but need not become
a database project.

### Practical completion criteria

- [ ] Dry-run and apply derive from the same immutable plan representation.
- [ ] Every destructive test is confined to a temporary sandbox.
- [ ] Symlink and replacement-race fixtures abort without touching the resolved
      target or anything outside it.
- [ ] Provider context changes abort instead of redirecting a command.
- [ ] Partial provider and filesystem failures are visible and journalled.
- [ ] `history` can explain what ran, what was skipped, and what failed.
- [ ] Explicit-only actions cannot run through a severity level or scheduled
      safe cleanup.

---

## Phase 4 — Project hygiene and stale-project intelligence

**Goal:** find storage that generic cache commands cannot understand because its
risk depends on project state.

### Current state

- Marker-based workspace detection exists for several common artifact names.
- Cargo workspace roots, shared target directories, Git state, Python
  environments, and project activity evidence are not implemented.

### Work

- Discover Git repositories and worktrees under explicit search roots.
- Record remote presence, upstream synchronization, dirty/untracked state,
  recent commits, and filesystem activity signals without reading arbitrary
  project contents.
- Detect project-local `node_modules`, `.venv`, Cargo `target`, Gradle output,
  Xcode build output, and other regenerated artifacts.
- Recognize shared/custom `CARGO_TARGET_DIR` and avoid duplicate accounting.
- Distinguish package-manager caches from project-local dependency copies.
- Identify large inactive projects and worktrees as recommendations.
- Never equate “old,” “clean,” or “pushed” with permission to delete a
  repository. Repository/worktree deletion remains explicit-only.
- Allow safe artifact cleanup within a protected repository without proposing
  deletion of source or unsynced data.

### Practical completion criteria

- [ ] Dirty, untracked, unpushed, remote-less, and synchronized repository
      fixtures are classified distinctly.
- [ ] Large project-local artifacts are attributed to their project and native
      ecosystem.
- [ ] Shared cache/build directories are counted once.
- [ ] No severity level selects repository or environment user data.
- [ ] Recommendations explain the evidence and exact proposed action.
- [ ] Scanning many repositories has a recorded performance baseline and avoids
      spawning an unbounded number of subprocesses.

---

## Phase 5 — Index, scheduling, and ratatui daily driver

**Goal:** remove the monthly maintenance burden and provide one fast overview of
the machine.

These are core personal-utility features, not commercial polish. They follow a
stable inventory/execution pipeline because they reuse its facts and safety
rules.

### Current state

- CLI tables, progress indicators, semantic colors, redirected output, and
  `NO_COLOR` behavior provide reusable presentation groundwork.
- `index.rs` is an inert placeholder.
- No `tui`, `index`, or `schedule` command exists.

### Incremental index

- Cache durable filesystem facts and the arena tree, not cleanup verdicts.
- Recompute target selection, severity, safety, and commands from current facts.
- Version and validate the index before access.
- Write atomically with owner-only permissions.
- Use FSEvents as a conservative dirty-root signal; fall back to a rescan when
  completeness is uncertain.
- Account for event loss, renamed roots, mount changes, and snapshot-driven
  uncertainty.
- Benchmark format choices for load time, update cost, file size, and memory
  mapping before committing to complexity.

### Scheduling

- Install and manage a macOS `launchd` agent; cron may be documented as a
  manual alternative but is not the primary integration.
- Pin the cleanrs executable, configuration path, search roots, and action set.
- Scheduled application is `safe` only and never uses Trash or explicit-only
  actions.
- A broader monthly job may refresh inventory and write a proposed plan without
  applying it.
- Keep logs and surface a concise reclaimed/failed/skipped summary.

### Ratatui interface

- Overview of container/volume state, recognized developer storage, large
  unknown paths, and recent cleanup history.
- Tree navigation with allocated/reclaimable metrics and coverage warnings.
- Provider/action inventory grouped by severity.
- Project-hygiene findings and explanations.
- Plan review and explicit execution through the same Phase 2/3 pipeline; no
  TUI-only deletion shortcuts.
- Fast startup from the index with visible freshness state and background
  refresh.

### Practical completion criteria

- [ ] Index corruption, truncation, version mismatch, and event-loss fixtures
      fall back safely.
- [ ] Indexed startup and incremental refresh have recorded latency and memory
      baselines and materially improve repeat use.
- [ ] Scheduled jobs cannot load working-directory configuration or select
      non-safe/explicit-only actions.
- [ ] The TUI exposes freshness, uncertainty, refusals, and exact planned
      operations.
- [ ] TUI actions produce the same plans and history records as CLI actions.
- [ ] Non-TTY invocation fails clearly, while ordinary CLI output continues to
      respect `NO_COLOR`.

---

## Phase 6 — Extensibility and optional portability

**Goal:** make cleanrs easier for other developers to adapt after the author's
workflow is solid.

### Deferred work

- Document a stable target/provider extension format.
- Support additional user-defined detectors and command planners without a
  shell.
- Consider modular provider packaging only when the built-in registry becomes
  difficult to maintain.
- Add import/export and compatibility guarantees when real external configs
  exist.
- Consider Linux support only in response to actual use. It requires different
  filesystem accounting, mount topology, event monitoring, scheduling, Trash,
  and provider policy—not merely compilation fixes.

The existing filesystem trait and portable fallback are useful development
seams but do not constitute supported Linux behavior.

---

## Standing engineering rules

- Dry-run is the default; mutation always requires `--apply`.
- Never delete synced cloud content. Inspect each candidate, not only roots.
- No severity level reaches user data or explicit-only actions.
- Never load configuration from the working directory.
- Commands are executable-plus-argv arrays; there is no shell.
- Prefer a provider's native cleanup interface over deleting its internal files.
- Show exact planned actions and important consequences.
- Trash means moved, not reclaimed, if Trash support is added.
- Reclaimable estimates remain intervals where uncertainty exists; provider and
  removed-allocation figures are labelled precisely.
- Direct-deletion invariants are enforced in code immediately before mutation.
- Filesystem and destructive tests use temporary fixture trees, never the real
  home directory.
- Do not run `--apply` during development without a specific sandbox path.
- Keep important safety and low-level parsing code commented for maintenance.
- Use structured logging, consistent terminal semantics, and progress reporting
  for operations long enough to need it.
- Optimize from profiles and repeatable measurements. Keep performance changes
  only when they preserve correctness and improve a recorded workload.
- Update this plan after each implemented feature, including verification and
  performance observations where relevant.

## Immediate continuation

Work should continue in this order:

1. Implement read-only Nix detection/inspection/planning; retain an explicit
   absent-provider status on machines without Nix.
2. Add the small resolved target registry and trusted configuration loading.
3. Add pnpm/npm/Yarn/Bun and uv/pip/Poetry/Conda inventory.
4. Add Cargo/rustup, Docker/OrbStack, and Xcode actions with risk separated per
   action.
5. Finish the Phase 2 path-safety and provider-context test suite.
6. Implement Phase 3 execution for a deliberately small safe-action subset.
7. In parallel with normal feature work, land the Phase 1 largest-file view,
   packed-record parser fixtures, and a checked-in performance baseline.

The provider/action model and Homebrew inspection/planning slice is complete.
The next concrete task is Nix inspection plus the resolved registry needed by
later provider and user-defined targets.
