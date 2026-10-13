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
| `safe` | Cheap generated output with low surprise and no network dependency | narrowly scoped generated caches and artifacts |
| `standard` | Downloadable state and broader generated output with noticeable rebuild cost | package-manager caches, stopped runtime/build state, older project artifacts |
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
| 2 | Developer-storage inventory and cleanup plans | **Complete** |
| 3 | Cleanup execution and lightweight history | **Complete** |
| 5A | Interactive TUI: explore, select, clean (pulled forward from Phase 5) | **Next priority** |
| 4 | Project hygiene and stale-project intelligence | **Queued after 5A** |
| 5 | Index, scheduling, and remaining ratatui daily-driver work | **Terminal groundwork only** |

**Correction (2026-10-13):** Phase 4 was previously the next priority. The
interactive TUI was split out of Phase 5 as Phase 5A and moved ahead of it;
the reasons are recorded in the Phase 5A decision record. Phase numbers are
kept stable so existing references remain valid.
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
- At the Phase 2 boundary, `clean --apply` refused before scanning. Phase 3 now
  supplies the guarded mutation path without changing Phase 2's plan semantics.
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
- A resolved registry merges built-ins, the fixed user config location,
  explicitly named config, and CLI targets by stable ID. It supports
  enable/disable overrides, export, CLI-only mode, and never reads the working
  directory implicitly.
- Configured paths and marker targets are sized in the explicit scan; direct
  deletion plans capture identity and reject a scan-root target, mount/cloud
  hazards, and symlinked ancestors. Configured commands and ad-hoc commands are
  argv arrays and remain plans only. Unknown fields, invalid ages, incomplete
  detectors, and detector/operation mismatches fail closed.
- At the Phase 2 boundary, explicit config commands required a per-run review
  acknowledgement. Phase 3 replaced this with persistent canonical-path plus
  content-hash trust before configured command execution.
- Read-only provider baselines cover Nix; pnpm/npm/npx/Yarn/Bun;
  uv/pip/Poetry/Conda; Cargo/rustup; Docker/OrbStack; Xcode; and CocoaPods.
  Stateful objects and capabilities without a stable granular cleanup remain
  informational rather than receiving a speculative command.
- Docker enumerates local Unix-socket contexts separately, pins context and
  endpoint, reports build cache/stopped containers/images/volumes independently,
  skips remote contexts, and never emits a generic volume-prune plan.
- Cargo uses bounded, frozen metadata inspection to resolve workspace roots and
  target directories, collapses member crates by target directory, uses newest
  manifest/build activity with a 14-day default, and plans a workspace-pinned
  `cargo clean` only for the ordinary workspace-local target. Custom, shared,
  and out-of-scope directories are visible but refused until ownership is
  complete.
- Informational operations cannot enter a cleanup plan. Explicit-only user data
  remains unreachable by severity, and `safe` plans have conservative
  unattended limits of 100 actions or 50 GiB.
- Default `targets` and `clean` tables prioritize reclaimable size, project
  name, and the complete home-relative location (including the command's scan
  root). Full operation argv, consequences, identities, and evidence remain
  available in JSON without forcing narrow terminal columns to wrap into
  unreadable multi-line rows.

### Provider inventory

Start with tools actually present on the development Macs, then widen coverage:

- Homebrew (**implemented**): routine cleanup, scrub, and
  autoremove are distinct read-only plans. More granular formula/cask reporting
  may be added when it improves decisions without duplicating native output.
- Nix (**implemented baseline**): plain garbage collection and old-generation
  collection are separate severity levels; exact byte opportunity remains
  unknown when Nix does not expose one cheaply.
- pnpm, npm/npx, Yarn, and Bun (**implemented baseline**): shared caches/stores
  and native supported actions; npm/npx remains informational where the
  installed npm has no stable separate action.
- uv, pip, Poetry, and Conda (**implemented baseline**): download caches,
  environments, and native cleanup
  actions with environments separated from disposable caches.
- Cargo/rustup (**implemented baseline**): resolved project targets plus
  informational registry/git-cache and toolchain inventory.
- Docker Desktop and OrbStack (**implemented baseline**): local Docker-context
  build cache, stopped objects, images, and volumes
  as separate actions and contexts.
- Xcode (**implemented baseline**): DerivedData, archives, device support,
  simulator caches, and simulator
  user data as different risk classes.
- CocoaPods has a native cache plan; Gradle, Maven, and other tools are added
  when encountered locally rather than being Phase 2 exit gates.
- VMware/other VM disks and Claude Cowork storage as informational or
  explicit-only targets, not generic caches.

Each provider implements a read-only `detect`/`inspect`/`plan` path before any
execution exists. A planned command is stored as an executable plus argv array;
there is no shell.

### Cargo build inventory — Phase 2 scope

Cargo build storage is useful before the broader project-intelligence phase and
therefore remains a Phase 2 deliverable:

- Scan only explicit or configured project roots such as `~/Developer` and
  `~/Projects`; never make the home directory an implicit search root.
- Detect Cargo packages and workspaces, resolve the workspace root, and collapse
  member crates that share one build directory into one candidate.
- Resolve the effective build directory, including `CARGO_TARGET_DIR` and
  supported Cargo configuration, rather than assuming every project uses a
  sibling `target/`.
- A shared target directory is eligible for a directory-wide action only when
  every discovered owning workspace is inside inspection scope and satisfies
  the policy. Incomplete ownership or an active owner refuses the broad action
  instead of deleting artifacts needed by another project.
- Record the newest meaningful build-artifact timestamp together with
  `Cargo.toml`/workspace-marker activity. A target becomes a stale-build
  candidate after approximately 14 days by default, configurable per target.
  Unknown or contradictory activity evidence must be shown, not treated as
  stale.
- The Cargo-aware implementation uses the newest manifest or build artifact and
  a 14-day default. If frozen metadata is unavailable, the older marker
  detector remains a conservative fallback instead of mutating a lockfile or
  downloading dependencies during inspection.
- Plan one exact cleanup operation per resolved build directory. Prefer a
  workspace-pinned `cargo clean` argv when it addresses exactly the inspected
  directory; otherwise use the guarded direct-delete operation reserved for
  proven workspace-local generated artifacts.
- Treat Cargo build output as generated but potentially expensive to rebuild.
  Staleness makes it eligible for review; it does not silently lower rebuild
  cost or authorize deletion of the repository.
- Report binaries, incremental state, dependencies, examples, and other
  build-output categories when they can be attributed without double counting,
  while keeping the cleanup action at the resolved build-directory boundary.
- Phase 2 plans cleanup only. Actual removal remains behind Phase 3's
  `--apply`, identity revalidation, path invariants, and sandboxed execution
  tests.

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
- [x] Homebrew, Nix, JS package managers, Python environment managers,
      Cargo/rustup, Docker/OrbStack, and Xcode appear in inventory when present.
- [x] Homebrew appears as separate cleanup, scrub, and autoremove actions with
      provider-reported/unknown estimates and pinned command plans.
- [x] Explicit-root Cargo inventory resolves packages, workspace members, and
      custom/shared target directories without duplicate candidates.
- [x] Cargo build candidates use configurable approximately 14-day artifact and
      marker activity evidence, with unknown evidence reported conservatively.
- [x] Human Cargo plans lead with project, resolved build directory, interval,
      age, and selection status; JSON additionally carries all evidence, the
      rebuild consequence, and exact cleanup operation.
- [x] Cargo parser/resolution fixtures cover a standalone crate, a multi-member
      workspace, custom `CARGO_TARGET_DIR`, a shared target directory, fresh
      output, stale output, and out-of-scope refusal. Cargo candidates reuse the
      common cloud/dataless and mount-boundary validators and their fixtures.
- [x] Each provider has fixture or command-output parser tests that do not
      require deleting real data.
- [x] Severity is assigned per action; Docker build cache, images, and volumes
      cannot collapse into one generic Docker target.
- [x] Explicit-only actions never enter a level-selected plan.
- [x] Adversarial path tests cover normalization widening, symlink ancestors, target
      replacement metadata, mount roots, and cloud refusal.
- [x] Human plans show the decision-oriented project/location summary; JSON
      shows the exact operation and consequence for detailed review or tooling.

**Status: complete for the read-only Phase 2 boundary.** The implementation can
inspect and construct fully reviewable plan data, but deliberately has no
deletion or provider-command execution API. Provider-specific execution support,
identity revalidation, persistent command-config trust, and journalling are
Phase 3 work rather than hidden Phase 2 mutations.

This paragraph records the Phase 2 exit boundary. Phase 3 has since implemented
those execution, revalidation, trust, and journal capabilities on top of the
unchanged planning model.

---

## Phase 3 — Cleanup execution and lightweight history

**Goal:** safely execute reviewed plans and leave enough evidence to understand
what happened.

### Already completed

- Dry-run is the default.
- Dry-run and apply consume the same owned reviewed-plan representation.
- `--apply` requires confirmation unless `--yes` is present; JSON apply requires
  `--yes`, and destructive execution as root is refused.
- Plans capture device, inode, and file type and revalidate them after a final
  metadata-only cloud/mount/coverage preflight.
- Direct deletion uses descriptor-relative `openat`/`fstatat`/`unlinkat`
  traversal, never follows symlinks, and supports guarded whole-directory and
  children-only operations.
- Provider commands execute as the exact captured executable plus argv and
  permitted environment, with executable and provider identity/context
  revalidation immediately before spawn. Output is streamed interactively and
  retained as bounded history summaries.
- Explicit command configs use persistent canonical-path plus SHA-256 content
  trust. Editing or replacing the file invalidates trust; trust can be listed
  and revoked.
- The private JSONL journal is append-only, records a durable pending snapshot
  before mutation and a snapshot after each action, collapses snapshots by run
  ID for history, and rotates at a bounded size.
- `history` reports successes, failures, skipped planning decisions, validation
  aborts, exact argv, estimates, removed allocation, provider-reported output,
  exit status, and concise output/error evidence.
- Human dry-run output ends with a context-aware tip: selected plans suggest
  `--apply`, while severity-skipped plans suggest the required level before
  reminding the user to review and apply.
- Platform-specific `libc::stat` conversions and allocation-free digest
  formatting keep strict workspace Clippy checks clean on macOS and Linux,
  including the Rust 1.85 CI toolchain.
- Cargo-provider execution uses a deterministic provider stub in tests that
  records the exact reviewed argv and refuses deletion outside its temporary
  target, avoiding host Cargo/rustup coupling on Linux CI runners.
- Fixture tests prove dry-run is inert and every destructive test operates only
  inside a unique temporary sandbox.

### Direct deletion

- Begin only with known regenerable directories that lack a suitable native
  cleanup command.
- Use descriptor-relative traversal that never follows symlinks.
- Revalidate device, inode, and type immediately before mutation.
- Abort an individual target on replacement or validation mismatch.
- Refuse root execution unless explicitly overridden for a justified action.
- Require `--apply`; require confirmation unless `--yes` is present.
- Keep all destructive tests inside unique temporary fixture directories.
- **`--trash` is deliberately deferred.** Same-volume Trash moves do not
  reclaim storage and require separate moved-byte/cross-volume semantics; the
  current flag refuses explicitly instead of pretending to recover space.

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

- [x] Dry-run and apply derive from the same immutable plan representation.
- [x] Every destructive test is confined to a temporary sandbox.
- [x] Symlink and replacement-race fixtures abort without touching the resolved
      target or anything outside it.
- [x] Provider context changes abort instead of redirecting a command.
- [x] Partial provider and filesystem failures are visible and journalled.
- [x] `history` can explain what ran, what was skipped, and what failed.
- [x] Explicit-only actions cannot run through a severity level or scheduled
      safe cleanup.

**Status: complete.** Phase 3 enables guarded permanent cleanup for eligible
direct-delete and native provider actions. It does not yet implement Trash or
claim observed APFS container recovery. The journal labels current evidence as
removed allocation or provider reported; container-delta calibration remains a
separate accounting enhancement.

---

## Phase 5A — Interactive TUI: explore, select, clean (pulled forward)

**Goal:** one keyboard-driven terminal window in which the user can find where
storage goes, inspect the cleanup targets inside a chosen directory, select
exactly the items to remove, and run the cleanup — through the same reviewed
plan, revalidation, and journal pipeline the CLI uses.

### Decision record (2026-10-13)

- **Why pulled forward.** Using `scan`, `targets`, and `clean` against a real
  `~/Developer` tree (≈108 GiB, 178 detected targets) showed that the CLI
  answers each question correctly but is painful to navigate: the user has to
  re-run commands with different paths and depths, and cannot pick individual
  items from a plan. `--only`/`--exclude` filter by *target ID* (for example
  `node.modules`), not by candidate path, so “clean only 3 of the top 5” is
  currently impossible from any interface.
- **Correction to the phase order.** Earlier versions of this plan placed the
  ratatui interface inside Phase 5, after Phase 4 project hygiene and after the
  incremental index. The TUI itself does not depend on either: it consumes the
  Phase 1 scanner and the Phase 2/3 plan/execution pipeline, both complete.
  VISION.md §12 already ordered the TUI directly after `clean --apply` (step 7)
  and before the index (step 8), so this change realigns the plan with the
  vision rather than departing from it. Index-backed startup, the machine
  overview, and project-hygiene panes remain Phase 5 work (see below).
- **Settled product decisions** (agreed with the author):
  1. **Manual ticking is explicit naming.** If the user ticks an item, it is
     planned and executed even when the current severity level or staleness
     policy would skip it. Such items are highlighted in the targets list and
     listed in a separate, warned group on the review screen (“not eligible by
     default — manually selected, will be deleted”). Safety refusals
     (`CandidateSafety::Refused`, cloud/dataless content, out-of-scope
     targets, path-invariant failures) can **never** be ticked.
  2. **No arbitrary deletion from the browser.** There is no gdu-style `d` key.
     cleanrs deletes recognized, explained cleanup targets only; the browser is
     for locating storage.
  3. **Targets are scoped to the chosen directory by default.** Machine-wide
     providers (Cargo registry, npm/pnpm/Bun caches, Homebrew, Docker, …) are
     hidden unless toggled on with a key.
  4. **Entry point is `cleanrs tui [PATH]`**, defaulting to the working
     directory like `scan` (never implicitly the home directory). Launching the
     TUI from a bare `cleanrs` invocation is a desirable follow-up once the TUI
     is stable; it is out of scope for this phase.

### Non-goals for this phase

- No TUI-only mutation path or deletion shortcut. Every mutation goes through
  the shared plan → preflight/revalidate → execute → journal pipeline.
- No arbitrary file/directory deletion and no Trash mode (`--trash` stays
  deferred; see Phase 3).
- No persistent index or FSEvents refresh; every launch scans. Freshness is
  shown instead (Phase 5 adds index-backed startup).
- No scheduling, no project-hygiene/Git panes (Phase 4 adds the data; Phase 5
  adds the pane), no container overview dashboard.
- No mouse support in the first version (keyboard first; mouse may follow).

### Interaction model

Two primary views over one shared scan, linked by `T`, followed by a linear
cleanup flow. `Esc`/`q` always steps back exactly one screen; `q` on the
browser quits (with a confirmation only if a cleanup is running).

```
 Browser ──T──▶ Targets ──x──▶ Review ──y──▶ Run ──▶ Results
 where is the     what can go      exactly what     live       what was
 space going?     in this folder?  will happen      progress   removed
    ◀──────────────── Esc / q steps back one screen ────────────────
```

#### 1. Browser view (gdu/yazi-style)

```
 cleanrs  ~/Developer/Projects                          Free 41.2 GiB · APFS
 ⠹ scanning… 18,204 dirs · 412k entries · 61.3 GiB
──────────────────────────────────────────────────────────────────────────────
  25.5 GiB ██████████████████  33%  matekerettsegi/           ◆ 3 targets
▶ 12.7 GiB █████████           16%  finance/                  ◆ 5 targets
   8.4 GiB ██████              11%  datinf/                   ◆ 2 targets
   5.5 GiB ████                 7%  erettsegi-site/           ◆ 4 targets
   ⠹ …                              hosthing/                 scanning
     312 MiB ▏                       notes.sqlite
──────────────────────────────────────────────────────────────────────────────
 j/k move  l open  h up  T targets  i info  s sort  / filter  ? help  q quit
```

- **Header:** current path, free space and filesystem of the containing
  volume (from existing `doctor` facts where cheap; `unknown` otherwise), and
  the activity/status line (see “Always show what is happening”).
- **Rows:** allocated size (default metric, same as `scan`), a proportional
  bar, percentage of the current directory, name (directories end in `/`),
  and badges:
  - `◆ N targets` — recognized cleanup-target markers somewhere below
    (cheap name/marker match against the registry’s workspace detectors during
    the scan; the authoritative list comes from `T`);
  - dim warning badges for incomplete coverage (unreadable subtree), mount
    boundaries, and dataless/cloud entries. Cloud content is shown but marked,
    never offered for cleanup.
- **Progressive sizing.** A directory’s size requires its whole subtree, so
  sizes cannot be computed lazily “on expand”. Instead the root’s immediate
  children are scanned as independent background jobs (bounded concurrency,
  reusing `Scanner::scan_with_observer` per child). Rows appear immediately
  with a spinner and fill in as each child job completes; the parent total
  grows live. Because each job returns the child’s full arena subtree,
  descending into a scanned directory is instant. Leaving a directory does
  **not** discard results; cancelling is only for an explicit rescan or quit.
  A later optimization may stream partial arena results out of a single scan
  (see Phase 1 performance work); per-child jobs are the first implementation.
- **Info panel (`i`):** allocated, apparent, immediately-private lower bound,
  reclaimability interval and confidence, and the concrete reason for any
  uncertainty (clone, hardlink, snapshot, fallback) — the TUI form of
  `scan --explain`.
- **Sort (`s`):** cycles size → name → modification age. Filter (`/`) narrows
  the visible rows by substring; `n`/`N` jump between matches.

#### 2. Targets view (`T`)

`T` runs target detection scoped to the highlighted directory (or the current
directory when a file is highlighted) — the same detection as
`cleanrs targets PATH --workspace-only`, plus machine-wide providers only when
toggled on.

```
 Targets in ~/Developer/Projects/finance               level: safe   12 found
 ✓ detected workspace targets  ⠹ cargo metadata 3/5 manifests
──────────────────────────────────────────────────────────────────────────────
  [x]  5.0–5.1 GiB  polygon-market-maker  cargo.target  71d   eligible
  [x]  1.4 GiB      dashboard             node.next     35d   eligible
  [ ]  2.1 GiB      web                   node.next      6d   not stale (14d)
  [!]  0.1–2.1 GiB  simplemailer          node.modules  25d   needs standard
   ✗   unknown      bridge                cargo.target   —    refused: outside scope
──────────────────────────────────────────────────────────────────────────────
 ~/Developer/Projects/finance/polygon-bots/polygon-market-maker/target
 delete directory · built 71d ago · removes allocated tree · rebuild: cargo build
──────────────────────────────────────────────────────────────────────────────
 Selected 2 · 6.4–6.5 GiB            space toggle  v range  1/2/3 level  x clean
```

- **Initial selection** equals what the active level (default `safe`) would
  plan — i.e. the CLI dry-run result. The user edits from there.
- **Row states:** `[x]` selected by level, `[ ]` not selected, `[!]` manually
  selected although the level/staleness policy would skip it (yellow), `✗`
  refused and untickable (red, reason shown). Refused rows stay visible so the
  user understands why something large is not offered.
- **Detail pane:** full path or exact executable + argv, operation kind
  (delete directory / delete children / provider command), age vs. threshold,
  selection reason or skip reason, rebuild cost, recoverability, and the
  reclaimable interval with confidence.
- **Footer total** is the selected plan’s reclaimable interval computed by the
  same code as the CLI plan summary — never a sum of per-file private sizes,
  never a bare point value when uncertainty exists. Provider-reported
  estimates are shown separately, as in the CLI.
- **Level presets** `1`/`2`/`3` reset the selection to `safe`/`standard`/
  `aggressive`. Manual ticks are discarded by a preset (the header says so).
- **Scope toggle** `m` adds/removes machine-wide providers; the header shows
  `scope: folder` or `scope: folder + machine`.
- Re-entering `T` on the same directory reuses the result and shows its age;
  `r` re-detects.

#### 3. Review screen (`x`)

A modal listing exactly what will happen, grouped:

1. **Selected by level** — normal rows.
2. **Manually selected, not eligible by default** — yellow header and warning:
   “These items would be skipped at level `safe` (reason shown per row). You
   selected them explicitly; they will be deleted.”
3. **Explicit-only / user data manually selected** — red header with the
   strongest warning. Allowed because ticking is explicit naming, never
   reachable through a level or preset.

Footer: item count, reclaimable interval, provider estimates, “Permanent
deletion — not moved to Trash”. Confirmation key is `y`; `Enter` does **not**
confirm, and `n`/`Esc` returns to the targets view with the selection intact.
Root execution is refused exactly as in the CLI.

#### 4. Run screen

- Overall progress gauge (actions completed / total) plus per-action status
  lines: pending, running (spinner), done (removed allocation or provider
  reported), failed (reason), aborted (revalidation mismatch — “changed since
  scan”), skipped.
- Scrollable output pane streaming provider stdout/stderr (bounded, the same
  summaries the journal keeps).
- `q` during a run asks for confirmation and stops **between** actions; an
  action in progress is never interrupted mid-deletion.

#### 5. Results screen

Removed allocation, provider-reported figures, failures, aborts, and skips —
labelled exactly as in `history`. Affected browser directories are marked
stale and rescanned automatically so the sizes the user returns to are true.

### Keymap (vim/yazi conventions)

| Context | Key | Action |
|---|---|---|
| Global | `?` | Help overlay listing every binding for the current view |
| Global | `q` / `Esc` | Back one screen; quit from the browser |
| Global | `Ctrl-c` | Quit (confirmation while a cleanup is running) |
| Move | `j`/`k`, `↓`/`↑` | Next/previous row |
| Move | `gg` / `G` | First/last row |
| Move | `Ctrl-d` / `Ctrl-u` | Half page down/up |
| Browser | `l` / `Enter` / `→` | Open directory |
| Browser | `h` / `←` / `Backspace` | Parent directory |
| Browser | `-` | Previous directory (history jump) |
| Browser | `T` | Targets for highlighted directory (or current) |
| Browser | `i` | Info/explain panel |
| Browser | `r` | Rescan current directory |
| Lists | `/`, `n`, `N` | Filter, next/previous match |
| Lists | `s` | Cycle sort |
| Targets | `Space` | Toggle highlighted item and move down |
| Targets | `v` | Visual range selection (yazi) |
| Targets | `Ctrl-a` | Select all *eligible* items (never refused ones) |
| Targets | `Ctrl-r` | Invert selection (refused items unaffected) |
| Targets | `u` / `Esc` in visual | Clear selection / leave visual mode |
| Targets | `1` / `2` / `3` | Reset selection to safe / standard / aggressive |
| Targets | `m` | Toggle machine-wide providers |
| Targets | `x` | Review and clean |
| Review | `y` | Confirm and run; `n`/`Esc` cancels |

Bindings live in one keymap table so the help overlay, footer hints, and
tests derive from the same source.

### Always show what is happening

- Every view has a one-line activity bar with a spinner and real counters,
  mirroring the CLI progress bars: directories/entries/bytes while scanning,
  detection phase and `cargo metadata N/M manifests` while finding targets,
  `action i/N` while cleaning. Idle state says `idle` plus data freshness
  (`scanned 4m ago`).
- Long operations never block input: the UI thread only renders and handles
  keys; all filesystem, provider, and execution work runs on worker threads.
- Errors surface as a dismissable status message (red) and are also logged via
  `tracing` to a file (never to the TUI’s own terminal).

### Color semantics

Shared with the CLI `terminal.rs` vocabulary and disabled by `NO_COLOR`
(symbols such as `[x]`, `[!]`, `✗`, `◆` keep meaning without color):

| Color | Meaning |
|---|---|
| Green | selected / eligible / succeeded |
| Yellow | blocked by level or staleness, manual override, warnings |
| Red | refused, failed, aborted, explicit-only warnings |
| Cyan | paths, headers, focused elements |
| Dim | unknown sizes, `0 B`, incomplete coverage hints, help hints |

The palette stays minimal: one accent color, no backgrounds except the focused
row highlight.

### Architecture

**Crates/dependencies.** `ratatui` + `crossterm` (VISION.md §14). Choose the
newest releases compatible with the workspace’s Rust 1.85 MSRV and pin with a
comment if necessary, as already done for `globset` and `comfy-table`. Worker
communication uses the existing `crossbeam-channel` dependency; no async
runtime, consistent with the scanner.

**Module layout** (inside the `cleanrs` binary crate, no new crate):

```
cleanrs/src/tui/
  mod.rs        entry point: TTY check, terminal setup/restore, panic hook
  app.rs        App state machine (screens, selection, focus) — no I/O
  event.rs      unified Event enum: Key, Tick, Worker(WorkerEvent)
  keymap.rs     binding table → Action enum; feeds help + footer
  worker.rs     background jobs: child scans, target detection, execution
  theme.rs      color/symbol semantics, NO_COLOR handling
  views/        browser.rs, targets.rs, review.rs, run.rs, results.rs, help.rs
```

**Event loop.** `crossbeam_channel::select!` over terminal input (read on a
dedicated thread), worker events, and a render tick (~10–15 Hz while busy,
idle otherwise). `App::update(Action | WorkerEvent)` is a pure state
transition; rendering reads state only. This keeps the logic testable without a
terminal.

**Terminal hygiene.** Alternate screen + raw mode, restored on normal exit,
error, and panic (panic hook). Non-TTY invocation fails with a clear error
(Phase 5 criterion).

### Prerequisite refactors (land first, CLI-visible where useful)

1. **Extract the shared cleanup pipeline from `main.rs`.** Today
   `run_clean`, `ReviewedPlan`, `enforce_apply_boundary`,
   `apply_reviewed_plan`, `validate_direct_preflight`, `revalidate_provider`,
   and journal helpers live in the binary and print directly. Move the
   non-presentational parts into a reusable layer (a `pipeline`/`plan` module
   in `cleanrs-core`, or a shared binary module if registry/trust coupling makes
   core inappropriate) that:
   - builds a `ReviewedPlan` from scope + registry + level + selection;
   - executes it, emitting `ExecutionEvent`s (action started, output line,
     action finished with `ActionResult`, run finished) to an observer;
   - writes the identical journal records.
   The CLI becomes one observer (indicatif + streamed output), the TUI another.
   Existing CLI tests must pass unchanged.
2. **Per-candidate selection in plans.** Extend planning so a selection can be
   expressed per candidate (stable candidate identity: canonical path +
   device/inode for paths, provider identity + argv for commands), not only per
   target ID. Represent the outcome explicitly, e.g.
   `PlanSelection::Selected { source: Level | Manual }` and a manual-override
   record carrying the skip reason it overrode. Refusals remain
   non-overridable in code, not just in the UI.
3. **Journal the selection source.** History records whether each action was
   selected by level or manually (and what it overrode), so `history` can
   explain why an ineligible-by-default item ran.
4. **CLI parity:** add `clean --select PATH` (repeatable) with the same
   semantics as a manual tick, and show manual overrides in the dry-run table.
   This also delivers per-item selection to non-TUI users.
5. **Scanner cancellation.** A cancellation token checked between directories
   so rescans and quit stop work promptly; cancelled scans report incomplete
   coverage rather than partial totals presented as complete.
6. **Scoped detection reuse (optional, measured).** Let target detection reuse
   arena facts already gathered by the browser scan instead of rescanning the
   subtree. Only land if it measurably reduces `T` latency without changing
   results.

### Milestones

| # | Delivers | Exit criteria |
|---|---|---|
| M0 | Refactors 1–5 above | CLI behaviour and tests unchanged; `clean --select` works in fixtures; manual-override recorded in journal |
| M1 | `cleanrs tui [PATH]` skeleton | Alternate screen, keymap, help overlay, clean restore on panic, non-TTY error |
| M2 | Browser | Progressive per-child scanning with live totals, navigation, sort, filter, info panel, coverage/cloud badges |
| M3 | Targets view | Scoped detection with progress, row states, detail pane, selection keys, presets, scope toggle, interval totals |
| M4 | Review + run + results | Grouped review with override warnings, `y`-only confirmation, streamed execution via shared pipeline, results, affected-dir rescan |
| M5 | Polish | Freshness labels, error messages, `NO_COLOR`, narrow-terminal layout (≥80 cols), docs (README/VISION/man page) |

### Testing strategy

- `App` state transitions tested directly with synthetic key/worker events
  (selection rules, refused rows untickable, presets, visual mode, back-stack).
- Rendering tested with ratatui’s `TestBackend` buffer assertions for each
  view, with and without color.
- Pipeline equivalence: the same fixture + selection produces the same
  `ReviewedPlan` and the same journal records from the CLI and TUI paths.
- Execution tests only in unique temporary sandboxes with the existing
  provider stub; never the real home directory, never `--apply` outside a
  sandbox.

### Practical completion criteria

- [ ] `cleanrs tui [PATH]` opens on the working directory by default and never
      implicitly on the home directory; non-TTY invocation fails clearly.
- [ ] Browser shows sizes progressively with visible scan activity and lets the
      user navigate before the scan finishes.
- [ ] `T` lists the directory’s targets with the same IDs, states, and
      intervals as `cleanrs targets PATH --workspace-only`.
- [ ] Items can be toggled individually, by range, all/none/invert, and by
      level preset; refused items cannot be selected by any key.
- [ ] Manually selected ineligible items are executed, are highlighted in the
      list and in a separate warned group on the review screen, and are
      journalled as manual selections.
- [ ] Execution requires an explicit `y`, runs through the shared pipeline with
      full revalidation, shows live progress, and produces history identical to
      an equivalent `clean --select … --apply` run.
- [ ] Totals are intervals computed by the shared plan-summary code.
- [ ] `NO_COLOR` and narrow terminals remain usable; every long operation shows
      progress.

### Follow-ups (not in this phase)

- Bare `cleanrs` (no subcommand, on a TTY) launches the TUI.
- History view inside the TUI.
- Index-backed instant startup and background refresh (Phase 5).
- Project-hygiene/Git evidence panes once Phase 4 provides the data.
- Mouse support.

---

## Phase 4 — Project hygiene and stale-project intelligence

**Goal:** find storage that generic cache commands cannot understand because its
risk depends on project state.

### Current state

- Marker-based workspace detection exists for several common artifact names.
- The Phase 2 Cargo scanner resolves workspace roots and target directories,
  collapses members, and uses a 14-day newest-manifest/build-artifact threshold.
- Custom/shared target directories retain observed owner evidence but are
  conservatively refused because a single explicit scan cannot yet prove
  complete ownership.
- Git state, repository-level sleeper-project evidence, and individually safe
  Python-environment recommendations are not implemented.

### Work

- Discover Git repositories and worktrees under explicit search roots.
- Record remote presence, upstream synchronization, dirty/untracked state,
  recent commits, and filesystem activity signals without reading arbitrary
  project contents.
- Detect project-local `node_modules`, `.venv`, Cargo `target`, Gradle output,
  Xcode build output, and other regenerated artifacts.
- Consume and revalidate Phase 2's shared/custom `CARGO_TARGET_DIR` ownership
  map instead of rediscovering or double-counting the same build directory.
- Build on Phase 2's approximately 14-day Cargo build candidates with stronger
  repository-level activity evidence: recent commits, dirty/untracked work,
  marker changes, newest build output, worktree activity, and optional
  ecosystem-specific signals.
- Distinguish “stale build output” from “dormant project.” Fourteen days is an
  artifact-cleanup threshold, not proof that a repository was abandoned.
- Rank sleeper-project build cleanup by reclaimable size and inactivity
  evidence, while showing which evidence caused the recommendation and which
  evidence weakened it.
- Allow per-root and per-project retention overrides for long-lived or
  expensive-to-rebuild Rust projects.
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
- [ ] Cargo sleeper-project recommendations combine Phase 2 build age with
      repository activity and never label a project abandoned from mtime alone.
- [ ] Shared Cargo build directories retain owner evidence; one dormant project
      cannot authorize cleanup that disrupts an active or out-of-scope owner.
- [ ] A stale Cargo build can be proposed for cleanup without proposing
      repository deletion, including for dirty, untracked, remote-less, and
      unpushed repository fixtures.
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

The browser, targets view, per-item selection, review, and execution screens
moved to Phase 5A. Phase 5 extends that TUI with:

- Overview of container/volume state, recognized developer storage, large
  unknown paths, and recent cleanup history.
- Machine-wide provider/action inventory grouped by severity.
- Project-hygiene findings and explanations (data from Phase 4).
- Fast startup from the index with visible freshness state and background
  refresh, replacing Phase 5A's scan-on-launch.

All execution continues through the shared pipeline extracted in Phase 5A; no
TUI-only deletion shortcuts.

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

1. **Phase 5A M0:** extract the shared plan/execution/journal pipeline from
   `main.rs`, add per-candidate selection with a recorded selection source,
   ship `clean --select PATH`, and add scanner cancellation. CLI behaviour and
   tests must remain unchanged.
2. **Phase 5A M1–M5:** `cleanrs tui [PATH]` skeleton, browser with progressive
   per-child scanning, scoped targets view with selection, review/run/results
   through the shared pipeline, then polish and documentation.
3. Investigate an observation from the 2026-10-13 real-tree dry run of
   `~/Developer`: several `cargo.target` candidates `targets` reported as
   `eligible` (71–125 days old) did not appear among the selected actions at
   the `safe` or `standard` level. Confirm whether this is the intended
   severity gate or a planning gap before the TUI presents the same selection.
4. Begin Phase 4 project hygiene using the shared pipeline rather than adding a
   second mutation path: Cargo shared-target ownership and project-activity
   evidence first, then repository state, worktree, lockfile, and
   process-activity evidence.
5. In parallel with normal feature work, land the Phase 1 largest-file view,
   packed-record parser fixtures, and a checked-in performance baseline.
6. Revisit Trash only as an explicit moved-not-reclaimed workflow, and add
   observed APFS container-free-space evidence only when its measurement
   boundary is truthful and reproducible.

Phases 0–3 now provide accounting, read-only inventory, immutable cleanup plans,
guarded execution, persistent config trust, and lightweight history. Phase 5A
(the interactive TUI) is the next concrete feature boundary, followed by
Phase 4.
