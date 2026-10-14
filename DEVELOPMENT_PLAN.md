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
| 5A | Interactive TUI: explore, select, clean (pulled forward from Phase 5) | **M0–M5 complete** |
| 4A | Target coverage and system scope (split out of Phase 4) | **Inventory implemented; remaining cleanup tracked in 4B** |
| 4 | Project hygiene and stale-project intelligence | **Implemented** |
| 4B | Complete provider sizing and cleanup coverage | **Planned; next priority before Phase 5** |
| 5 | Index, scheduling, and remaining ratatui daily-driver work | **Terminal groundwork only** |
| 6 | Extensibility and optional portability | **Deferred** |

**Correction (2026-10-13):** Phase 4 was previously the next priority. The
interactive TUI was split out of Phase 5 as Phase 5A and moved ahead of it;
the reasons are recorded in the Phase 5A decision record. Phase numbers are
kept stable so existing references remain valid.

**Correction (2026-10-14):** the overview previously still called Phase 4A
the next priority and Phase 4 future work despite their implementation records.
Both are integrated; Phase 4B now tracks the remaining provider work explicitly.

**Correction (2026-10-14):** after 5A, Phase 4 was again listed as next. Real
TUI use showed that the limiting factor is detector and provider coverage, not
repository evidence, so the coverage half of Phase 4 was split out as Phase 4A
and moved ahead of it. See the Phase 4A decision record.

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
  skips remote contexts, and currently emits no volume-prune plan. Phase 4B
  replaces that blanket restriction with separate anonymous/named actions.
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
- **Update (2026-10-14):** Gradle, Android SDK, Metro/Expo/Watchman, Go, Maven,
  Playwright/Puppeteer, and the still-unimplemented VMware, Claude Cowork, and
  wallpaper-cache providers are scheduled in Phase 4A.

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
     **Correction (2026-10-14):** the in-list `m` toggle is superseded by a
     separate system-only scope (`S`) in Phase 4A; folder and system targets
     will no longer share one list.
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
browser opens a quit confirmation. **Correction (2026-10-14):** the earlier
design only required confirmation during cleanup; every browser quit now asks,
while a running cleanup retains its stop-after-current-action prompt.

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
directory when a file or recognized target directory is highlighted; recognized
target rows focus that target) — the same detection as
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
  `scope: folder` or `scope: folder + machine`. *(Superseded by Phase 4A's
  `S` system scope; kept here as the 5A implementation record.)*
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

**M0 implemented (2026-10-13): prerequisite refactors 1–5 complete.**

- [x] Shared non-printing `cleanrs/src/pipeline.rs`: `PlanRequest` builds an
      owned `ReviewedPlan` without mutation, with `PlanObserver` scan/phase and
      Cargo metadata counters. CLI confirmation and explicit trust-store writes
      stay in presentation; registry/trust dependencies keep the pipeline in
      the binary crate. Shared inventory helpers serve `targets` and `clean`.
- [x] `execute_reviewed_plan` preserves root/unattended boundaries, durable
      pending snapshots, direct-delete preflight, descriptor-relative deletion,
      provider identity revalidation, exact argv, bounded output summaries, and
      per-action journal snapshots. `ExecutionObserver` receives run/action,
      stdout/stderr, and result events. Its cooperative stop token is checked
      only between actions; remaining actions are journalled as skipped with
      “stopped by user”. Output events carry raw newline-preserving byte
      fragments (long lines split at 8 KiB), preserving CLI output bytes.
- [x] Core `CandidateKey`, `SelectionSource`, `SelectionMode`, and
      `plan_cleanup_with_selection` support per-candidate explicit choices.
      Manual selections override level/age/explicit-only policy, reject safety
      refusals and informational actions, reject manual exclusive conflicts,
      and supersede level-selected exclusive alternatives. Automatic safe-plan
      limits and unattended execution limits remain enforced. Totals still use
      the existing deduplicated subtree interval calculation.
- [x] Repeatable `clean --select PATH` resolves canonical candidate paths or
      full command keys after detection and uses `ManualOnly`. Unknown paths
      list up to five detected descendants. Human rows/tips mark manual choices
      and overrides, with yellow/red semantics and `NO_COLOR`. Clap-generated
      help and completions include the flag; man generation continues to work.
      Plan JSON preserves
      `selection.status`/skip reasons and adds `candidate_key` and selected
      `selection.source` (`kind`, manual `overrode`).
- [x] Journal actions add `selection_source` and `overridden_skip_reason`;
      missing fields default to level/no override for old JSONL records. Human
      history marks manual selections and their overridden reason; JSON exposes
      the fields. The journal schema remains 1.
- [x] `Scanner::scan_with_observer_cancellable` accepts an optional
      `Arc<AtomicBool>` and checks it between directories. Cancelled arenas
      contain a `scan cancelled` coverage error, keeping partial accounting
      incomplete. Existing scan entry points retain their behavior.

Fixture tests cover core manual policy, exclusive selection and mode rules,
key serde/exact argv, pipeline event order and output streams, stop-after-start
behavior and durable journal snapshots, old journal parsing, CLI dry-run/apply/
history/refusal/canonical selection, user-data unattended refusal, and scan
cancellation. All filesystem/mutation fixtures use unique temporary directories.
M0 adds no dependencies, TUI modules, or subcommands. Prerequisite item 6 (scoped
scan reuse) and M1–M5 remain unimplemented.

**Verification:** `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace --all-targets` (95 tests), and
`rustup run 1.85.0 cargo check --workspace --all-targets` all passed. CLI help,
Bash completion generation, man generation, and `git diff --check` also passed.
**Correction:** the existing `man` command generates top-level command
information, not subcommand option listings; M0 preserves that behavior.


**M1 implemented (2026-10-13): TUI skeleton and browser entry point.**

- [x] `cleanrs tui [PATH]` is a read-only clap subcommand; PATH defaults to `.`,
      is canonicalized, and must be a directory. Both stdin and stdout must be
      terminals; rejection happens before terminal modes or traversal. Generated
      help/completions/man continue to include the subcommand.
- [x] `cleanrs/src/tui/{mod,app,event,keymap,worker,theme}.rs` and
      `views/{mod,browser,help}.rs` implement the specified module layout.
      `App::handle(Action)` and `App::on_worker(WorkerEvent)` are pure state
      transitions; queued `Request` effects perform I/O on workers. A screen
      stack currently implements Browser and Help, leaving targets/review/run/
      results for M3/M4. `T` says “targets view not implemented yet”.
- [x] Raw mode/alternate screen have an RAII restoration guard, including
      partial setup errors. The panic hook restores the terminal before
      delegating to the prior hook. A dedicated input thread polls with a
      bounded shutdown interval; the main loop uses `crossbeam_channel::select!`
      for input, worker events, and 12.5 Hz busy ticks, blocking when idle.
- [x] One keymap table supplies dispatch, help and footer hints, including
      consecutive `gg`, navigation, half-pages, filter editing, match jumps,
      sorting, info, rescan, help/back, and quit. `NO_COLOR` preserves symbols
      and uses a reverse-video cursor without assigning any colors.
- [x] Ratatui **0.29.0** (declared Rust 1.74) and its compatible Crossterm
      **0.28.1** backend are pinned. Ratatui 0.30 requires Rust 1.88; Crossterm
      0.29 would introduce an incompatible second event/backend version.
      The transitive `instability` macro dependency is constrained to **0.3.10**,
      because 0.3.11+ requires Rust 1.88. Rust **1.85.0** workspace/all-targets
      verification passes with these dependencies.
- TUI logging destination: deliberately **disabled** (`io::sink`), the simple
  option permitted for M1; `-v`/`RUST_LOG` filtering is retained. Worker errors
  are traced and also shown as dismissable status messages. CLI logging stays
  unchanged. A persistent TUI log file remains a follow-up.

**M2 implemented (2026-10-13): progressive read-only storage browser.**

- [x] Immediate child metadata produces file sizes and pending directory rows.
      Two bounded child jobs each use one existing scanner worker, the M0
      cancellation token, and throttled `ScanObserver` progress. Queued scans
      under the visible directory move ahead of background siblings; changing
      directories preserves outstanding work. Quit cancels every job and does
      not wait on an in-flight OS syscall before restoring the terminal.
- [x] Completed child arenas are shared through `Arc<Snapshot>` with path-keyed
      directory cache entries. Descending into completed subtrees renders
      cached children immediately. Bottom-up summaries and parent accumulation
      deduplicate hardlinks, including those across independent child jobs.
      Parent totals grow with completed results; the activity line displays
      aggregated directories/entries/bytes, completed children, and idle age.
- [x] Path/volume header uses cheap `statvfs` and, on macOS, `statfs` filesystem
      name (verified against the installed SDK's `sys/mount.h`); unsupported
      filesystem types report unknown. Rows show allocated bytes with the
      existing formatting helper, proportional bars, percentages, directory
      suffixes, and name truncation by terminal cell width at 80 columns.
- [x] `◆ N targets` is explicitly a **hint**, computed from the built-in
      workspace detector marker/directory pairs; no separate marker list or
      authoritative Cargo/provider detection is added. Subtree warnings show
      incomplete coverage, mount boundaries, firmlinks, and dataless/cloud
      entries, with a help legend for compact badges.
- [x] Info uses the existing `estimate_subtree` interval, confidence, explanation,
      and immediately-private lower bound. Large-subtree accounting runs off
      the UI thread; pending or unavailable accounting says so. Size/name/age
      sorts retain cursor identity as sizes arrive; substring filter, match
      cycling, parent-on-demand, and directory history are implemented.
      Rescan cancels/invalidates overlapping ancestors and descendants so a late
      ancestor arena cannot resurrect stale results; revision checks discard
      obsolete events.
- [x] Synthetic state tests cover movement/navigation/history, cached descent,
      sort identity, filter, help/back, placeholder, rescan/stale events, and
      quit. `TestBackend` tests cover partial/finished browser, info/help,
      symbols with/without colors, badges, and 80-column Unicode truncation.
      TempDir worker fixtures compare every child and parent total against
      `Scanner::scan`, including cross-child hardlinks and symlinks; cancellation
      tests stop queued jobs. A piped CLI test asserts the non-TTY error and no
      alternate-screen output.

**Correction to the earlier M0 note:** M1/M2 are now implemented; M3–M5 and
optional prerequisite item 6 (authoritative detection scan reuse) remain open.
No M0 scanner/pipeline/accounting/mutation APIs were changed. `main.rs` only adds
TUI dispatch before CLI terminal logging; existing CLI integration tests gain a
non-TTY TUI regression test.

**Known limits:** navigating before an ancestor job finishes can duplicate its
subtree work (still bounded to two scanner workers); active jobs are cooperative
and queue priority does not preempt an in-flight scan. The session cache has no
size cap yet. Browser roots/rows conservatively avoid dataless directories and
firmlinks, including canonical root firmlinks that a whole-root CLI scan can
traverse. Initial child paths preserve OS names; cached descendant names inherit
the scanner arena's lossy UTF-8 name representation. No live interactive TUI was
launched during verification. Target selection, review, execution and results
remain intentionally unavailable.

**Verification:** `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace --all-targets` (108 tests), and
`rustup run 1.85.0 cargo check --workspace --all-targets` pass. Help, Bash
completions, man generation, and `git diff --check` are also checked.


**M3 implemented (2026-10-13): scoped targets and exact candidate selection.**

- [x] `tui/app.rs` extends the existing screen stack and owns per-scope
      `Targets` caches, `CandidateKey` ticks, visual ranges, filter/sort state,
      freshness and request generations. `T` scopes to the highlighted
      directory, falling back to the current directory for a file. Back/re-entry
      retains cached ticks; `r` and `m` retain choices for surviving selectable
      candidates and apply the level default to newly found candidates.
      Presets `1`/`2`/`3` explicitly discard manual ticks.
- [x] `tui/worker.rs::plan_request` matches the CLI defaults (safe, 14-day Cargo
      threshold, fixed user config resolution, no ad-hoc targets). Folder-only
      inspection is the default; `m` includes machine-wide providers. A separate
      serialized cleanup worker runs `build_reviewed_plan` off the UI thread,
      forwards phase/scan/Cargo counters, and coalesces queued plans for the same
      scope. Generation checks discard obsolete results.
- [x] `views/targets.rs` shows all four checkbox/refusal states, concrete policy
      labels, exact paths/argv, operation, age/threshold, rebuild cost,
      recoverability, consequence, interval and confidence. Core `PolicyStatus`
      and `DetectedTarget::{policy_status, manually_selectable, explicit_only}`
      centralize explanations/availability; `PlannedTarget::policy_status`
      also exposes whole-plan skips. The planner uses this same structured
      policy decision and preserves existing skip-reason strings/JSON.
      The CLI usage formatter is reused instead of duplicating interval rules.
- [x] Space, visual range, Ctrl-a/Ctrl-r/u, level presets, scope toggle,
      re-detection, review, filter and sorting are in the single keymap table.
      Ctrl-a adds the current level's eligible set across the whole inventory;
      invert/range can explicitly tick policy-skipped items. Every selection
      path excludes refused/informational rows; core rejects them again.
      Help and footer derive from the table; help scrolls at narrow heights.
- [x] Footer changes debounce for 150 ms before a full pipeline rebuild, even
      after navigating back to the browser. It shows updating rather than a
      stale interval and keeps provider estimates/unknown actions separate.
      Review always performs another fresh rebuild.

**Selection-source decision:** every final tick is passed as a manual key with
`SelectionMode::ManualOnly`, including initial level-selected ticks. This
matches `clean --select` exactly and keeps unticked level-eligible items out.
Eligible manual actions have `SelectionSource::Manual { overrode: None }`;
policy overrides have `Some(reason)`. Review labels the former group “Selected
by level” because they satisfy that policy, while history truthfully records
that the final selection was manual. No `LevelPlusManual` special case is used.

**M4 implemented (2026-10-13): review, guarded run and results.**

- [x] `views/review.rs` renders only the returned `ReviewedPlan`, grouping
      policy-eligible picks, manual overrides with the specified warning, and
      explicit-only/user-data picks with a red irreversible-loss warning.
      Count, shared interval, separate provider estimates and permanent-deletion
      notice remain visible. Review scrolls; empty `x` is a status message,
      Enter is inert, `y` alone confirms, and `n`/Esc preserves ticks.
- [x] `Request::Execute` invokes the existing `execute_reviewed_plan` with
      `unattended = false`, the shared stop flag, execution observer and normal
      journal location. `enforce_apply_boundary` runs before switching screens
      and again in the pipeline. No revalidation, cloud/path protection,
      descriptor-relative deletion or journal implementation was changed.
- [x] `views/run.rs` shows completed/total gauge, pending/running/success/
      failure/validation-abort/skip evidence, and scrollable last-200-line
      stdout/stderr output, with a bounded 256-event worker channel for
      backpressure. Control characters are removed from terminal display
      only; original journal summaries remain intact. `q`/Ctrl-c asks “Stop
      after the current action? y/n”; `y` sets the cooperative flag and does not
      interrupt an in-flight action. Ctrl-c from run help follows the same flow.
- [x] `views/results.rs` preserves history's evidence/status vocabulary,
      separates removed allocation and provider reports, and shows the run ID
      plus `cleanrs history`. Returning to the browser invalidates overlapping
      ancestor/descendant arenas and targets scopes, then schedules fresh scans.
      This also refreshes parents when provider commands expose a changed path.
- [x] TUI diagnostics now append to a private `tui.log` beside history, with
      ANSI disabled. This replaces M1's deliberately disabled logging.
      Errors remain dismissable status messages and are traced.
- [x] New `tui/tests.rs` covers selection keys/presets/refusal, cache reuse,
      stale-plan rejection, grouped review, Enter versus y, retained ticks,
      stop confirmation, worker planning progress/debounce, and all four views
      on `TestBackend` with/without color, including narrow layouts. Fixture
      `.next`/`dist`/`node_modules` keys produce the same actions, operations,
      selection sources and interval as equivalent CLI `--select` requests.
      Child-process execution fixtures set HOME/XDG state under a unique TempDir:
      only the selected directory is removed, journal overrides persist, provider
      streams arrive between start/finish, and stopping skips subsequent actions
      while the running provider finishes. Existing CLI/core safety tests pass.

**M2 review corrections implemented:** the initial browser cursor follows the
top sorted row until the user first moves it; subsequent updates retain path
identity. Singular badges say `◆ 1 target`, and recognized target-directory
rows carry IDs such as `◆ node.modules`. State, render and scan-fixture tests
cover these corrections. These changes supersede M2's unconditional cursor
identity statement above.

**Correction to earlier progress notes:** M3/M4 now replace the placeholder;
selection/review/execution/results are available. M5 polish and optional
prerequisite item 6 (reuse of authoritative scan facts) remain follow-ups.
Existing scan-cache limits and possible duplicate ancestor/subtree work remain;
selection totals intentionally rebuild through the pipeline rather than adding
per-file private sizes. No new crates, dependencies, config search locations,
CLI mutation flags or journal schema were added. The only M0 edits are the
structured core accessor and making the CLI usage formatter crate-visible;
shared pipeline/execution/journal code remains unchanged.

**Verification:** all four required commands pass: `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace --all-targets` (121 tests), and
`rustup run 1.85.0 cargo check --workspace --all-targets`.
Headless tmux checks used only a temporary workspace and sandbox HOME/state:
browser, targets, manual-override review, cancel/back and 80-column targets/review
were inspected without confirming deletion. The sessions were killed afterwards.

**Orchestrator end-to-end review of M0–M4 (2026-10-13).** Re-ran all four
verification commands (121 tests pass). Drove the release binary in headless
tmux with a `mktemp` fixture and `HOME` pointed at a `mktemp` directory:
browser → `T` on a project → ticked a `needs standard` `node_modules` override
→ review showed it in the warned "Manually selected — not eligible by default"
group → `Enter` did not confirm, `y` did → exactly the three selected
directories were deleted (sibling `package.json` and the other project's Cargo
`target` untouched) → results screen and `cleanrs history` agree, including
`[manual: requires standard cleanup level]` → returning to the browser
showed rescanned sizes. Findings to address in M5:

1. **Plan rebuild cost per tick.** Each selection change (debounced 150 ms)
   rebuilds the full plan, which rescans the whole scope. On the author's
   `~/Developer` (~108 GiB) a scan took ~25 s, so the footer total and `x`
   would lag badly on large scopes. Compute the live footer from the cached
   detection result (same core interval function over the selected
   candidates), and rebuild through the pipeline only on `x`, with visible
   progress. Consider prerequisite item 6 (scan reuse) here.
2. **`T` on a row that is itself a target** (e.g. `node_modules/`) scopes
   detection to that directory and finds 0 targets. When the highlighted row
   is a recognized target directory, scope to the current directory and put
   the cursor on that target.
3. **Results screen sizes** print raw bytes ("removed 41943040 bytes
   allocation"); use the shared human size formatter while keeping
   history's vocabulary.
4. **Detail pane** ends with a dangling separator ("confidence High ·").
5. Every TUI tick is journalled as `manual` (`ManualOnly`, by design). The
   history marker `[manual]` without a reason means "policy-eligible, picked
   by hand"; document this in README/`history` help.


**M5 implemented (2026-10-13): cache totals, review freshness, and polish.**

- [x] `pipeline::SelectionAccounting` retains the detection arena and candidate
      root indices in a non-serialized `Arc`; `SelectionSummary` serves both
      CLI plan totals and immediate TUI footers via `estimate_subtrees` and the
      identical saturating provider summation/unknown counts. Tick/range/all/
      none/invert changes and cached level presets enqueue no scan/plan work.
      All three policy presets are computed through the core planner on the
      detection worker; App switches cached selections without filesystem I/O.
      Review generations still reject an older selection’s in-flight plan.
- [x] `x` shows “building reviewed plan…” plus `PlanObserver` phase/counter
      progress. `build_tui_reviewed_plan` uses the shared builder in ManualOnly
      mode, narrowing ticks that vanished, became refused, or changed captured
      filesystem identity, usage, risk policy, provider identity, or exact operation. App names
      every dropped tick in a yellow review notice (or a status if none remain).
      CLI selection still errors for unknown/refused picks; execution,
      immediate revalidation, preflight and journal behaviour are unchanged.
- [x] Browser marker badges also drive `T` scoping: recognized target rows use
      the current folder and focus the matching candidate, including cached
      re-entry. Results and human history use `human_bytes` with the existing
      “removed … allocation” / “provider reported” vocabulary; JSON stays bytes.
      Detail confidence joins present explanation parts without a trailing dot.
- [x] Browser scan and targets detection age share s/m/h freshness labels;
      data older than ten minutes is yellow with “press r to refresh”. Every
      view displays traced red errors dismissable by any key, while recognized
      keys continue navigating. PermissionDenied listing failures use coverage
      events/badges rather than error status; scanner subtree coverage stays
      non-fatal. The private M4 log location remains unchanged.
- [x] Help descriptions follow each screen’s actual review/stop/refresh actions.
      Footers use the shared keymap and remove complete low-priority hints first
      to fit the available width, retaining confirm/review/help/back. All views
      are exercised at 80×24 and with colors disabled; below 80×20 the renderer
      shows a centered “terminal too small (need 80x20)” message.
- [x] README adds a keymap and partial-selection walkthrough, `[manual]` versus
      override semantics, freshness/error/minimum-size details. VISION §13 and
      `tui`/`history` clap help match; top-level man generation remains intact.
- [x] Regression fixtures prove no tick/preset worker requests, cached interval
      equivalence with a reviewed hardlink selection, separate provider/unknown
      sums, grown-allocation refusal, marker-row focus,
      vanished/replaced/newly-refused drop notices without widening, invalid
      scope error rendering/dismissal, freshness, and complete help/no-color
      layouts across every screen. Existing sandbox execution parity tests pass.

**Correction to M3/M4 notes:** the 150 ms debounce and full pipeline rebuild
for selection totals are removed. Only detection (`T` cache miss, `r`, `m`),
review (`x`), and an uncached preset need planning. Cached presets apply the
worker-computed policy choices and discard manual ticks entirely in memory. The UI reads cached facts
for its total; it never executes that cache. M1’s indefinite idle wait is now a
one-second freshness tick so aging data warns without requiring keyboard input.
All previously ticked Phase 5A practical completion criteria remain met.

**Deferred optimization:** optional prerequisite item 6 remains a follow-up:
`T` and `x` still scan their scope rather than reusing browser arenas. Detection
arenas are retained for exact live totals; the session cache still has no size
cap. No real home/Developer workload was run for M5, and no large-tree scan-reuse
performance claim is made.


**M5 verification:** `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace --all-targets` (**128 tests**), and
`rustup run 1.85.0 cargo check --workspace --all-targets` pass. `tui --help`,
`history --help`, full `man` generation and `git diff --check` also pass.
NO_COLOR tmux walkthroughs used only mktemp workspaces and isolated HOME/state
at 110×30 and 80×24: browser → `T` on `node_modules` → one tick → review →
Enter inert → n cancel → x/y confirm → results/history both say
“removed 40.0 MiB allocation [manual: requires standard cleanup level]”
(the manual suffix is in history) → browser refreshed to 12.0 MiB. Sibling
`.next`, `dist`, `package.json` and the Cargo fixture stayed intact. Removing
only the temporary project then pressing r showed a red planning error,
appended it to the sandbox TUI log, and an unbound key dismissed it. At 60×15,
the centered minimum-size message rendered. All sessions and fixtures were
removed; no real home/Developer scan or mutation was performed.


**Spinner regression fixed (2026-10-14).** Targets detection/review and the
cleanup-run activity header had hard-coded `⠹` characters even though the
80 ms busy timer still advanced. Browser, Targets and Run now share the same
ten-frame spinner driven by that timer; running action rows use it too.
A render regression fixture advances only timer ticks, without worker events,
and verifies that detection, review and run indicators animate while their
activity/progress remains unchanged. Idle freshness timing is unchanged.
Validation: 62 binary/TUI tests passed with a canonical temporary root;
clippy with warnings denied, formatting and diff checks passed. The development
binary was rebuilt.

**Post-M5 TUI improvements (2026-10-13).**

- Targets now show a scope-relative project-path column instead of an ambiguous
  basename. Narrow layouts truncate the column; the detail pane retains exact paths.
- `T` assembles discovery arenas from completed browser child scans, including
  nested arena indices and cross-tree hardlinks. Shallow root metadata/listings
  are refreshed on the worker, and new/replaced/missing child scans fall back to
  normal discovery. Cached facts retain their original age. `r` and machine/policy
  re-detection bypass browser reuse.
- `x` performs fresh metadata enumeration on the selected candidates' ancestor
  chain and fresh scans of selected trees, then uses the existing shared planner,
  policy/identity/operation/usage comparisons, and dropped-tick notices. Unrelated
  subtrees are not walked. Review still checks live facts; execution's immediate
  revalidation, provider identity, journal and explicit `y` boundary remain intact.
- Browser cache limits: 250,000 unique arena nodes, 4,096 directory aliases and
  32 listings/250,000 inactive rows. Target cache limits: eight scopes and 250,000 arena nodes. Eviction
  removes oldest completed arenas with all aliases and inactive row references;
  active working data may exceed the limits. Obsolete events cannot replace a
  newly requested listing after eviction. Cancelled review and closed results
  release their plans rather than retaining them indefinitely.
- Review has a compact scope/level/selection summary, a titled action list,
  counted policy groups, numbered actions, exact operation paths, per-action
  confidence and consequences, and a persistent confirmation area. End-of-list
  scrolling uses rendered wrapped-line counts through ratatui’s pinned
  rendered-line-info feature, including narrow paths/warnings.

**Correction to the deferred optimization above:** unconditional `T`/`x`
full-scope scans and unbounded session caching are resolved. `T` can still scan
on cache misses, and selecting every candidate still requires scanning all
selected trees during review. This is session reuse, not Phase 5's durable index.
No real home/Developer scan or mutation was used for these changes.

Regression fixtures cover cached discovery without scanner callbacks, nested
arena rebasing and hardlink parity, new-directory fallback, preserved scan age,
explicit-refresh bypass, whole-arena alias/reference eviction, relative paths,
wrapped review scrolling, and the existing changed/refused candidate checks.
`review_walk_is_limited_to_selected_trees` creates 100 unrelated two-level
branches and one selected tree; review emits exactly one scanned directory.
An illustrative warm debug run (`cargo test -p cleanrs
review_walk_is_limited_to_selected_trees -- --nocapture`) took 47.9 ms for full
discovery and 24.5 ms for selected review. These fixture timings are not a
real-workload benchmark or a latency promise; the deterministic traversal
assertion is the performance regression gate.

**Verification:** `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace --all-targets` (**132 tests**), and
`rustup run 1.85.0 cargo check --workspace --all-targets` pass;
`git diff --check` is clean. NO_COLOR sandbox tmux walkthroughs at 110×30 and
80×24 verified repeated project paths, review groups, wrapped final-action
scrolling, Enter remaining inert, and `n` returning to selection. Only temporary
workspace/HOME/state paths were used; the sessions and fixtures were removed.


**TUI restart caching and quit confirmation (2026-10-14).**

- Browser `q`/Esc and global Ctrl-c now open a centered quit confirmation.
  `y` quits; Esc/Enter/`n` stays. Dialog keys never reach underlying selection,
  navigation or cleanup handlers. Filter/range/help/back and run-stop semantics
  retain their context; NO_COLOR and 80×24 layouts are covered.
- Fixed parent re-scanning after arena eviction: keep the completed listing,
  measured sizes, freshness and cursor, and clear its detailed Arc references
  separately. A visible row source can open its folder even after alias eviction.
  Coordinator inode maps now release immediately when all children complete.
- `tui/cache.rs` implements a fixed-user-location, bounded binary fact cache
  using pinned bincode 1.3.3. Snapshot records persist metadata, names and parent
  indices; parent order, entry sizes, filenames, version, root identity/mtime,
  timestamp, SHA-256 integrity and private ownership are validated on load. Files are 0600 in a
  0700 local directory (cloud namespaces/dataless cache content are refused),
  written through a same-directory temporary file and atomic
  rename. Limits: 1 GiB aggregate, 1,024 entries, 256 MiB per entry, oldest saved
  entries first. Complete child scans save before their completion reaches the
  UI, so exiting an idle view does not lose finished work. Orphan temporary
  writes from exited processes are removed on the next session.
- New sessions restore exact scoped entries, or compose a folder from existing
  child entries, preserving hardlink accounting and the oldest fact age. Cached age is retained as a duration offset so a reboot cannot
  reset freshness. Small headers are checked before decoding large siblings on a partial miss. Cached
  views are labelled; deeper mutations may remain stale until `r`, which always
  scans fresh and invalidates overlaps. This is static restart caching, not the
  full Phase 5 event-driven incremental index.
- Cancellation/publication share a lock so a cancelled scan cannot resurrect
  an invalidated entry. Cleanup cancels overlapping browser jobs and invalidates
  disk facts before execution. Corrupt/missing entries fall back safely; write
  failure warns in the browser and leaves session sizes available. Cached facts
  never bypass the shared live review/execution checks or persist a verdict.

**Correction to earlier post-M5 claims:** session-only caching did not satisfy
restart reuse, and evicting entire inactive listings could cause parent rescans.
Both are now addressed. The requested TUI follow-up implements the minimal
restart cache; durable CLI indexing, FSEvents and background refresh remain
Phase 5 work. Tests use only temporary workspaces, cache and HOME/state paths.

Regression coverage proves independent worker/App restart reuse, per-child
composition when going above the initial scope, parent navigation after arena
eviction, fresh explicit refresh, malformed/truncated/version-mismatched and
checksum-corrupt data fallback, root replacement, path/parent validation, private
permissions, disk limits, dead-writer cleanup, cloud-cache refusal, write-failure
warnings, 400-day age retention across reboot, and quit cancel/confirm behavior.
A small warm debug fixture took 39.2 ms fresh versus 1.3 ms on restart; these
illustrative timings are not a large-Developer workload benchmark. The regression
checks cache provenance and avoids repeat browser worker requests deterministically.

**Verification (2026-10-14):** fmt, clippy with `-D warnings`,
`cargo test --workspace --all-targets` (**143 tests**), Rust 1.85 workspace/all-target
check, and `git diff --check` pass. Sandboxed NO_COLOR tmux sessions at 110×30
and 80×24 exercised exit confirmation/cancellation, actual process restart,
restored totals and Backspace parent navigation with 16 projects/1,920 payload
files (7.6 MiB allocated). The final checksummed cache restored the idle view
in an illustrative 0.141 s; scope totals stayed identical and parent return
showed no scan. Both sessions and all workspace/HOME/cache/state fixtures were
removed. No real home/Developer scan or mutation was performed.


**PR #16 CI correction (2026-10-14).** Both Ubuntu workflow runs stopped
at Clippy because the browser header's filesystem label was mutable only on
macOS. The label now comes from immutable platform-specific bindings, retaining
the macOS lookup and unknown fallback. Rust 1.95 Clippy also required simplifying
the stale-worker-event guard; missing directories and mismatched revisions still
discard events. These corrections qualify the earlier local verification claims:
they did not establish a passing Linux CI run for the PR head.

**Verification:** Rust 1.95 formatting and workspace/all-target Clippy with
`-D warnings` pass. All 54 application and 45 core unit tests pass, along with
36 CLI integration tests. The remaining six CLI tests require conventional
`/usr/bin/touch`, `/usr/bin/false`, or `/bin/rm` paths absent on the local NixOS
host; the unchanged Ubuntu CI fixtures provide their full-suite verification.
Rust 1.85 compatibility is checked by the existing CI MSRV job.

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

- [x] `cleanrs tui [PATH]` opens on the working directory by default and never
      implicitly on the home directory; non-TTY invocation fails clearly.
- [x] Browser shows sizes progressively with visible scan activity and lets the
      user navigate before the scan finishes.
- [x] `T` lists the directory’s targets with the same IDs, states, and
      intervals as `cleanrs targets PATH --workspace-only`.
- [x] Items can be toggled individually, by range, all/none/invert, and by
      level preset; refused items cannot be selected by any key.
- [x] Manually selected ineligible items are executed, are highlighted in the
      list and in a separate warned group on the review screen, and are
      journalled as manual selections.
- [x] Execution requires an explicit `y`, runs through the shared pipeline with
      full revalidation, shows live progress, and produces history identical to
      an equivalent `clean --select … --apply` run.
- [x] Totals are intervals computed by the shared plan-summary code.
- [x] `NO_COLOR` and narrow terminals remain usable; every long operation shows
      progress.

### Follow-ups (not in this phase)

- Bare `cleanrs` (no subcommand, on a TTY) launches the TUI.
- History view inside the TUI.
- Index-backed instant startup and background refresh (Phase 5).
- Project-hygiene/Git evidence panes once Phase 4 provides the data.
- Mouse support.
- **Moved to Phase 4A (2026-10-14):** a dedicated system-wide target scan
  (`S`) replacing the in-list `m` machine-scope toggle.

---

## Phase 4A — Target coverage and system scope

**Goal:** make cleanrs recognize the generated storage that actually exists in
a developer folder and on the machine, and give folder-level and system-level
cleanup separate, first-class entry points in both the CLI and the TUI.

### Decision record (2026-10-14)

- **Observation.** Real TUI use against `~/Developer` mostly surfaced
  `node_modules`, `.next`, `dist`, and Cargo `target`. That reflects what
  cleanrs can detect, not necessarily what is on disk: `BUILTIN_TARGETS`
  contains exactly four workspace detectors (`cargo.target`, `node.modules`,
  `node.next`, `node.dist`). Python environments, tool caches, Expo/React
  Native build output, Gradle output and similar directories cannot appear.
- **Correction.** Phase 4 listed "detect project-local `node_modules`, `.venv`,
  Gradle output, Xcode build output…" as one bullet inside the Git/hygiene
  work. Detector coverage does not depend on Git evidence for unambiguous
  directory names, so it moves here. Ambiguous names that need gitignore
  evidence stay gated on Phase 4 (see tier 2 below).
- **Two scopes, named explicitly.** The codebase already separates
  *workspace targets* (marker-based, scoped to an explicit path) from
  *providers* (machine-wide: Homebrew, Nix, JS/Python package managers,
  Cargo/rustup, Docker/OrbStack, Xcode, CocoaPods). User-facing terminology
  becomes **folder targets** and **system targets**. What is missing is UX:
  - the CLI has `--workspace-only` but no system-only mode, so inspecting
    providers always costs a folder scan;
  - the TUI hides providers behind `m` inside a folder's Targets view, which
    mixes both scopes in one list and ties machine-wide actions to an
    unrelated directory.
- **Settled product decisions** (agreed with the author):
  1. The TUI gets a global `S` shortcut that runs a **system-only** target
     scan (providers only, no folder detection, no filesystem scan needed).
  2. Selection → review → execute for system targets is **identical** to the
     folder workflow: same Targets view, presets, manual-tick rules, review
     groups, run and results screens, and the same shared pipeline/journal.
  3. The `m` toggle is removed; one Targets list never mixes the two scopes.
     The header shows `scope: folder <path>` or `scope: system`.
  4. The CLI gains `--system` for `targets` and `clean` (no path; providers
     only), the counterpart of `--workspace-only`.

### Detector model extension

The current workspace detector is "one marker file → one sibling directory".
Most new targets cannot be expressed that way. Extend `TargetDefinition` (and
the config `detect` form, keeping unknown-field rejection) with:

- **Any-of markers:** e.g. `pyproject.toml | requirements.txt | setup.py |
  Pipfile` for Python projects.
- **Nested relative paths** for both marker and directory: e.g.
  `ios/Podfile` → `ios/Pods`, `android/build.gradle` → `android/.gradle`.
- **Inner markers:** a directory is recognized by a file it contains, e.g. a
  Python virtual environment by `pyvenv.cfg`, independent of its name.
- **Nested-target collapsing** must keep working: a `__pycache__` inside a
  detected `.venv` or `node_modules` is not a separate candidate.

Every new detector inherits the existing guarantees unchanged: candidate-level
cloud/dataless refusal, mount-boundary refusal, §8.2 path invariants, identity
capture, deduplication, and staleness from marker/artifact mtimes.

### Folder targets — tier 1 (unambiguous names, this phase)

| Ecosystem | Directories | Notes |
|---|---|---|
| Python | `.venv`/`venv` (via `pyvenv.cfg`), `__pycache__`, `.pytest_cache`, `.mypy_cache`, `.ruff_cache`, `.tox`, `.nox` | Virtual environments are `network` class (packages re-downloaded) and may contain manually installed packages, so not `safe`; tool caches are `free`/`cheap`. |
| JS/TS frameworks | `.turbo`, `.nuxt`, `.svelte-kit`, `.parcel-cache`, `.angular/cache`, `node_modules/.cache` | `node_modules/.cache` collapses into a selected `node_modules`. |
| Expo / React Native | `.expo`, `ios/Pods` (marker `ios/Podfile`), `android/.gradle`, `android/app/.cxx` | **Never** target `ios/` or `android/` themselves: they are generated under Expo prebuild but source code in the bare workflow, and the filesystem cannot distinguish the two. |
| Gradle / JVM | `.gradle` (marker `settings.gradle[.kts]`/`build.gradle[.kts]`) | Project-local Gradle state only; the shared `~/.gradle` cache is a system target. |
| Other | `.dart_tool` (Flutter/Dart), `.zig-cache`/`zig-cache`, `.terraform` | `.terraform` is downloadable provider state (`network`). |

### Folder targets — tier 2 (ambiguous names, gated on Phase 4)

`build/`, `out/`, `coverage/`, `ios/build`, `android/build`,
`android/app/build`, and similar generic names are frequently source or user
content. They become detectors only once Phase 4 supplies Git evidence, and
then only when the directory is **gitignored** inside a repository. Outside a
repository they remain undetected. Gitignore status is recorded as evidence on
the candidate.

### System targets — new providers

Same read-only `detect`/`inspect`/`plan` contract as Phase 2 providers, native
commands preferred over direct deletion, argv arrays only:

- **Gradle:** `~/.gradle/caches`, wrapper distributions, daemon logs (cache
  and wrapper dists are `network`).
- **Android SDK:** system images and build tools as expensive downloadable
  targets; AVDs (emulator disks) are `user_data`, informational/explicit-only.
- **Metro / Expo / Watchman:** Metro and haste-map caches under `$TMPDIR`,
  `~/.expo` cache, `watchman watch-del-all` as an informational or confirmed
  action.
- **Go:** `go clean -cache` (build cache, `cheap`) separate from
  `go clean -modcache` (`network`).
- **Maven:** `~/.m2/repository` (`network`, standard).
- **Browser automation:** Playwright (`~/Library/Caches/ms-playwright`) and
  Puppeteer downloads (`expensive`, `network`).
- **Small caches:** node-gyp headers, SwiftPM cache, TypeScript server cache.
- **§7.3 baseline leftovers (still unimplemented):** VMware `.vmwarevm`
  bundles (informational `user_data`), Claude Cowork VM bundle
  (explicit-only, informational first), and the version-validated wallpaper
  cache (`confirmed`).

### System scope UX

- **CLI:** `cleanrs targets --system` and `cleanrs clean --system [--apply]`
  take no path, skip the filesystem scan and workspace detection, and inspect
  providers only. `--system` conflicts with a path argument and with
  `--workspace-only`. `--only`/`--exclude`/levels/`--select` behave as today.
- **TUI:** `S` from any non-modal screen opens the Targets view in system
  scope. Provider inspection runs on the worker with the usual activity line
  (per-provider progress, since some commands are slow). Results are cached
  for the session with their age, `r` re-inspects, and `Esc` returns to the
  screen `S` was pressed on.
- Remove `m` and the `machine` flag from the folder Targets view; update the
  help overlay, README keymap table, and VISION/README wording together.

### Implementation and validation (2026-10-14)

- `targets.rs` now shares a metadata-only detector index with custom TOML
  detectors and browser badges: alternative markers, nested normal relative
  paths, and inner markers. Invalid or contradictory forms fail validation.
  All tier-1 targets above are implemented, with candidate-level cloud/dataless
  refusal and nested collapsing. Generic tier-2 paths now use Phase 4 Git proof.
- `system_targets.rs` inventories exact provider paths; it never recursively
  searches the home directory. Provider-owned cache trees are measured for
  interval/set accounting; system mode skips the unrelated folder scan entirely.
  Gradle caches/wrappers/daemon storage, Android SDK packages/AVDs, Metro files
  and directories, Expo cache, Watchman, Go, Maven, Playwright/Puppeteer,
  node-gyp, SwiftPM and TypeScript are covered. Native Go, SDK, Watchman and
  supported SwiftPM commands retain pinned paths/identities and are re-inspected
  before execution. Missing capabilities or unknown daemon state stay informational.
  Maven repositories containing `maven-metadata-local.xml` are preserved because
  unpublished local artifacts cannot be restored by downloading dependencies.
- VMware bundles and the exact Claude Cowork bundle are informational user data.
  Wallpaper discovery uses the user-recalled identifier
  `extension-com.apple.wallpaper.extension.image` under
  `~/Library/Containers/com.apple.wallpaper.agent/`, with bounded depth/entries,
  no symlink traversal or placeholder enumeration, and installed `sw_vers`
  evidence. **Open validation gate:** the supplied path contains `...` and no
  observed macOS version/schema was supplied. There is no verified generated-
  cache layout or quiescence/regeneration evidence, so wallpaper deletion stays
  disabled for every version. Finding this extension does not prove its contents
  are disposable. No installed macOS SDK exists on this Linux verification host.
- CLI `targets --system` and `clean --system` reject explicit paths and
  `--workspace-only`, retain filters/selections/levels, and inspect providers only.
  `system: true` plus `[system]` identify scope in JSON/history; old journal records
  default to folder scope. Existing combined CLI invocation remains compatible.
- TUI `S` opens a separate session-cached system Targets view from Browser,
  Targets or Results. Per-provider activity, `r` refresh, preserved manual ticks,
  review/run/results and origin restoration share the folder pipeline. Folder
  requests use workspace-only mode; `m` is removed. Provider scans are merged for
  union accounting and filesystem native commands receive fresh path preflight.
- Temporary fixtures cover every tier-1 detector and its cloud/dataless refusal,
  nested Expo/venv cases, Git-ignore changes, all new providers, protected state,
  missing capabilities, CLI scope conflicts, and TUI cache/modal/return behavior.
  `cargo test --workspace`: 181 tests passed (61 binary, 47 CLI, 73 core).
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all
  --check`, and `git diff --check` passed. System Go execution/history is
  verified through a stub tool against an explicit temporary sandbox. Selection-
  scoped generic-build review tests preserve ignore evidence and refuse changed
  admin symlinks; merged provider arenas preserve hardlinks and overlapping roots.
  The earlier Phase 4 fixture benchmark remains a pre-4A baseline, not a new
  performance claim for expanded system inventory.

### Practical completion criteria

- [x] Detector model supports any-of markers, nested relative paths, and inner
      markers; config parsing rejects incomplete/contradictory forms.
- [x] Each tier-1 detector has a fixture test, including nested collapsing
      (`__pycache__` in `.venv`) and a dataless/cloud refusal case.
- [x] Expo fixtures prove `ios/` and `android/` are never candidates while
      `ios/Pods` and `android/.gradle` are.
- [x] `targets --system` / `clean --system` inspect providers only, without a
      filesystem scan, and refuse a path argument.
- [x] TUI `S` opens a system-scope Targets view whose selection, review, run,
      and results behave and journal identically to the folder flow; `m` is
      gone and no list mixes scopes.
- [x] New providers have command-output/fixture parser tests and never require
      deleting real data; Android AVDs, VMware bundles, and Cowork state are
      unreachable by any level.
- [x] README, VISION (§7 provider table, §7.3), and the TUI help agree.

---

## Phase 4B — Complete provider sizing and cleanup coverage

**Status: planned (2026-10-14). Next provider milestone, before Phase 5
indexing/scheduling.** This phase completes the provider workflows exposed by
Phases 2/3/4A; it is not a requirement to add more crates.

**Correction:** “provider implemented” in earlier phases often means executable
or path detection plus a command plan, not measured storage and usable cleanup.
The system Targets view currently conflates unmeasured storage, unsupported
output parsing, unknown cleanup yield, and failed inspection as `unknown`.
Phase 4A's completed inventory boundary does not complete the work below.

### Implemented follow-up: Docker execution context matching (2026-10-14)

- **Correction:** execution preflight previously picked the first refreshed
  candidate with the same action ID. Docker repeats action IDs across contexts,
  so an OrbStack images/build-cache plan could abort against the unrelated
  `default` context even when the reviewed identity was unchanged.
- Preflight now finds the reviewed full provider identity, including executable,
  version, context and endpoint, among matching action IDs. Missing/changed
  identity still aborts; a failed fresh inspection or changed exact cleanup
  operation also aborts instead of authorizing mutation from identity alone.
- CLI regression fixtures cover both image and build-cache cleanup with an
  unavailable first context, reordered contexts, changed endpoints/versions,
  a removed reviewed context, and a newly unavailable reviewed daemon. They use
  a stub Docker executable and an explicit temporary root, never real Docker
  storage. Remaining provider accounting and UI diagnostics below stay open.
- Validation: all 184 workspace tests passed (61 binary, 48 CLI, 75 core)
  with `TMPDIR` set to a unique directory under `/private/tmp`; clippy with
  warnings denied, formatting, and diff checks passed. The default macOS
  `/var/folders/...` temporary path exposes existing canonical-path fixture
  failures (`/var` versus `/private/var`); those tests pass with the canonical
  temporary root. No real provider cleanup was performed.

### Provider accounting and inspection

- [ ] **Yarn:** resolve the installed variant's actual cache location and scope,
      measure it, and validate the native cache-clean operation. Executable-only
      detection must not imply that a cache exists; inspection must not trigger
      Corepack downloads or read policy from the working directory.
- [ ] **pnpm / npm / npx:** measure resolved stores and caches, distinguish npm
      content-cache verification/GC from npx execution-cache cleanup, and add a
      supported version-checked npx cleanup action where available. Total store
      allocation is not the amount pnpm prune or npm verify will recover.
- [ ] **Bun:** diagnose failed `bun pm cache` inspection, handle installed-version
      capabilities, resolve and measure the real cache, and retain clone-aware
      uncertainty. Preserve the actual failure detail instead of only exit code.
- [ ] **pip / Poetry / Conda:** resolve and measure package caches, implement
      provider-specific parsers (pip's structured human report and Conda's JSON
      dry-run cannot use the current bare-integer parser), and distinguish full
      cache removal from partial pruning. Environment inventory must enumerate
      individual environments and their allocation; any environment removal is
      explicitly selected and reviewed, separate from cache cleanup.
- [ ] **CocoaPods:** resolve and measure the downloaded-pod cache; `pod cache
      list` output is inventory evidence, not a cache directory or byte estimate.
- [ ] **Cargo / rustup:** locate and size registry/Git caches and individual
      toolchains. Check installed native capabilities for granular cache GC and
      toolchain removal; implement supported operations with version evidence.
      Explicit toolchain selection must disclose project dependencies. If an
      operation remains unsupported, give a specific reason and retain measured
      inventory rather than treating detection as completed cleanup coverage.
- [ ] **Homebrew autoremove / Nix GC:** use removable-object inventory and
      provider-native read-only size information where available; preserve unknown
      yield when it cannot be derived. Do not present all installed/store bytes
      as the cleanup estimate.
- [ ] **OrbStack machines / VMware / Claude Cowork:** size individually identified
      machine/bundle storage and distinguish it from Docker cache. Investigate
      supported native removal/decommission operations for explicitly selected
      machines/bundles; retain a concrete unsupported reason where none exists.
      These remain user data, outside every level preset and scheduled apply.
- [ ] **Docker contexts:** show context name and endpoint in rows/details,
      distinguish unavailable contexts from available storage, and retain useful
      inspection stderr. One failed `system df` must not look like four duplicate
      anonymous Docker failures. Keep engine bytes separate from host recovery.
- [ ] **Wallpaper:** close the outstanding Phase 4A version/schema and
      quiescence/regeneration validation gate before enabling confirmed cleanup.
      Inventory remains available with a specific blocked-capability explanation.

### Required cleanup targets

- [ ] **Unused anonymous Docker volumes:** add a separate stable target,
      proposed ID `docker.volumes-anonymous`, using native `docker volume prune`
      without `--all`. Only volumes referenced by no container qualify, including
      no references from stopped containers. Assign confirmed `standard` cleanup;
      exclude from unattended `safe` apply. Inspect anonymous-volume candidates
      and report their bytes separately from named volumes.
- [ ] **Unused named Docker volumes:** add a separate stable target, proposed ID
      `docker.volumes-named`, inventory individual eligible volumes and require
      explicit selection of those names. Named volumes remain `user_data`, outside
      every level preset. Review discloses permanent data loss and executes native
      `docker volume rm` for the selected unused names; do not use `volume prune
      --all` as a substitute because it would also sweep anonymous volumes.
- [ ] Revalidate Docker context/endpoint and volume reference state immediately
      before execution. Handle references changing after review without forced
      removal. Explain that removing stopped containers can make additional
      volumes unused; newly eligible volumes require fresh review rather than
      silently widening the reviewed volume set. Validate installed prune
      capabilities and ensure anonymous pruning cannot sweep unreviewed volumes
      if eligibility changes. Journal identities, exact argv, and provider results.
- [ ] **Gradle / gradlew caches:** complete `gradle.caches` for
      `$GRADLE_USER_HOME/caches` (default `~/.gradle/caches`), with wrapper
      distributions and daemon state separate. Existing Phase 4A implementation
      requires a global `gradle` executable to establish stopped-daemon evidence;
      support installations using only `gradlew` without assuming absence of
      global Gradle proves inactivity. Do not execute arbitrary project wrapper
      scripts without explicit trust or cause downloads during inspection.
      Provide measured, reviewed cache deletion once quiescence is established,
      with dependency-download/rebuild consequences and guarded path identity.
- [ ] **Homebrew cache:** complete explicit measured inventory of `brew --cache`
      (commonly `~/Library/Caches/Homebrew`, honoring configured locations).
      Existing `homebrew.cleanup` and `.scrub` already plan native cleanup, but
      they also cover old installed versions and are not equivalent to deleting
      the entire cache directory. Make the supported cache scope and omissions
      visible; investigate a separate full-download-cache action if native cleanup
      does not cover the requested scope. Validate it before offering it and
      avoid overlapping estimates/actions with cleanup/scrub.
- [ ] **Xcode DerivedData:** complete sizing and reviewed removal of
      `~/Library/Developer/Xcode/DerivedData` in system scope. The current
      `xcode.derived-data` action already exists but its estimate is always unknown;
      retain explicit selection and rebuild-cost warnings. Keep archives, device
      support, all simulator user data, and unavailable simulators separate.
      Measure simulator inventory and the subset removed by native
      `simctl delete unavailable`, rather than parsing device JSON as an integer.

The shell `rm -rf` examples describe desired storage coverage; implementation
uses native provider commands where supported, otherwise the shared guarded
filesystem executor. No shell commands are introduced.

### Completion criteria

- [ ] Every provider above has a recorded cache/object location, measured
      allocation where accessible, a cleanup capability or specific remaining
      blocker, and fixture coverage for installed-version differences.
- [ ] Report occupied allocation, estimated cleanup intervals/confidence,
      provider-reported bytes and observed recovery separately. Partial GC/prune
      cannot inherit whole-cache size as its reclaimable estimate.
- [ ] CLI/TUI distinguish “not measured”, “cleanup yield unknown”, “inspection
      failed” and “inventory only”; explanations include actionable evidence.
      Empty storage is distinguished from executable detection and failed sizing.
- [ ] Docker anonymous/named fixtures include running/stopped references,
      reference changes after review, context changes, and newly orphaned volumes.
      Named cleanup never sweeps anonymous or unselected volumes; anonymous
      cleanup never reaches named volumes. No destructive tests use real Docker
      storage, the real home directory, or unsandboxed `--apply`.
- [ ] Cache sizing reuses filesystem/set accounting, deduplicates overlapping
      targets, retains cloud/dataless/mount/symlink invariants, and runs through
      the existing selection/review/execution/history pipeline in both scopes.
- [ ] Update README/VISION capability statements only as features land; this
      checklist stays open until end-to-end sizing and cleanup are verified.

---

## Phase 4 — Project hygiene and stale-project intelligence

**Goal:** find storage that generic cache commands cannot understand because its
risk depends on project state.

### Implemented (2026-10-14)

- `cleanrs-core/src/projects.rs` provides a shared, read-only project inventory.
  `cleanrs projects [PATH] [--json]` exposes ranked project/worktree reports;
  `targets` and `clean` JSON also include them. `projects` skips machine-wide
  providers. Human output uses the existing TTY/NO_COLOR tables and shows
  attributed storage, activity, Git flags, intervals/confidence, exact operations,
  rebuild consequences, supporting evidence and weakening evidence.
- Discover repositories, linked worktrees and ecosystem markers only under the
  explicit root. A containing repository outside that root remains unknown.
  Nested packages inherit repository evidence; active sibling worktrees weaken
  dormancy, and out-of-scope worktrees prevent a dormant conclusion.
- Git inspection distinguishes dirty, untracked, remote-less, no-upstream,
  synchronized, unpushed, behind and diverged states. It records local commit
  time and worktree relationships without fetching. Synchronization describes
  local refs, not live remote state. Commands use argv arrays, disable fsmonitor,
  content filters, optional locks and lazy fetching, refuse external config
  includes/partial clones, and discard inherited Git redirection/config variables.
  Filtered repositories may conservatively appear dirty. Unsafe, dataless,
  symlinked, externally referenced, or incomplete admin metadata never authorizes
  cleanup.
- Repository inspection is sequential and bounded: at most 128 repositories,
  768 commands and a 15-second budget checked between commands. Each project
  command has a 2-second timeout and 4 MiB output cap. Omitted/failed inspection
  remains visible and cannot be mistaken for a clean repository.
- Project activity combines newest source/manifest/dependency-lockfile metadata,
  commit time, newest artifact activity and related worktrees. Dependency
  lockfiles are metadata signals, not proof that a process is running. No
  arbitrary source contents are parsed by cleanrs; native Git determines
  working-tree state. Repositories with observed dataless/cloud content are
  refused before invoking status. Dormancy defaults to 90 days; the existing
  14-day Cargo artifact threshold remains separate. Future timestamps, local
  work and recent output weaken inactivity; no report calls a repository
  abandoned from mtime alone.
- **Corrected to the Phase 4A plan:** the earlier implementation classified
  Python environments as user data and matched generic Gradle/Xcode `build`
  directories from ecosystem markers. Environments are now `network`, identified
  by `pyvenv.cfg` regardless of their name, excluded from `safe` with a manual-
  install warning. Generic `build`/`out`/`coverage` and mobile build paths require
  repository-local gitignore proof; tracked-source protection still vetoes them.
  The shared bounded Git inspector records ignore rules and rechecks ignored
  status before execution, refusing removed repositories or changed ignore rules.
- Artifact age now uses the newest entry in its tree rather than directory
  mtime alone. Dirty/untracked/unpushed/remote-less source does not prevent
  independently generated stale build cleanup. Tracked source inside an
  artifact path does: Git checks artifact pathspecs and retains only vetoed
  roots, not source filenames. Incomplete repository inspection also refuses
  cleanup. The same guard runs again immediately before an artifact action.
  Selection-scoped TUI review checks admin metadata and tracked paths without
  running status over unselected source; activity is explicitly uninspected.
- Cargo candidates retain structured `CargoOwnership`: target directory,
  observed workspace owners and completeness. Shared/custom output is reported
  once, with references from other owners, newest-owner manifest activity and
  conservative retention across owners. Incomplete ownership remains refused,
  including manual selection; one dormant owner cannot authorize a shared
  clean. Marker-only Cargo findings cannot substitute for unavailable metadata.
  Native cleanup revalidates frozen metadata resolution, executable version,
  target identity, cloud state, mount boundaries and coverage before running.
- Trusted user/explicit TOML accepts `[[retention]]` with absolute or `~/` paths,
  `project_only`, `artifact_days`, `dormant_days` and `keep`. Exact project rules
  override root rules; the most-specific root wins otherwise. Explicit config
  overrides identical user rules. `keep` requires explicit naming even with
  `--ignore-staleness`. Working-directory policy is never loaded. Target evidence
  is retained in the backward-compatible history journal.
- Tests use temporary repositories, worktrees, provider stubs, homes, policy
  files and cache/build trees. They cover all repository states, recent source
  and existing-file writes, unknown/limited inspection, cloud/dataless metadata,
  fsmonitor refusal, tracked-file changes after review, shared/active Cargo
  owners and changed target resolution, retention precedence/keep, Python
  network/safe-exclusion policy, and human/JSON CLI parity. Existing execution fixtures now
  resolve their sandbox tools through PATH so they also run on NixOS.
- A reproducible multi-repository benchmark and measured Linux baseline live in
  [docs/benchmarks-phase4.md](docs/benchmarks-phase4.md). The fixture is generated
  in a unique temporary directory; it never scans or cleans the real home.

**Correction:** the previous state said Git/project intelligence was absent.
It is implemented above. Shared/custom Cargo ownership is deliberately still
conservative: observing owners in one root does not prove there are none
outside it. This is a safety refusal, not permission to guess ownership.

### Work

- Discover Git repositories and worktrees under explicit search roots.
- Record remote presence, upstream synchronization, dirty/untracked state,
  recent commits, and filesystem activity signals without reading arbitrary
  project contents.
- ~~Detect project-local `node_modules`, `.venv`, Cargo `target`, Gradle output,
  Xcode build output, and other regenerated artifacts.~~ **Moved to Phase 4A
  (2026-10-14)** for unambiguous names. What remains here: record gitignore
  status as candidate evidence and use it to enable Phase 4A's tier-2
  detectors (`build/`, `out/`, `coverage/`, `ios/build`, `android/build`, …)
  only for gitignored directories inside a repository.
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

- [x] Dirty, untracked, unpushed, remote-less, and synchronized repository
      fixtures are classified distinctly.
- [x] Large project-local artifacts are attributed to their project and native
      ecosystem.
- [x] Cargo sleeper-project recommendations combine Phase 2 build age with
      repository activity and never label a project abandoned from mtime alone.
- [x] Shared Cargo build directories retain owner evidence; one dormant project
      cannot authorize cleanup that disrupts an active or out-of-scope owner.
- [x] A stale Cargo build can be proposed for cleanup without proposing
      repository deletion, including for dirty, untracked, remote-less, and
      unpushed repository fixtures.
- [x] Shared cache/build directories are counted once.
- [x] No severity level selects repository or environment user data.
- [x] Recommendations explain the evidence and exact proposed action.
- [x] Scanning many repositories has a recorded performance baseline and avoids
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
- `index.rs` remains a CLI-index placeholder. The TUI now has a bounded static
  restart cache (details above); incremental/event-driven indexing is pending.
- **Correction (Phase 5A):** `tui` now exists; `index` and `schedule` remain
  unimplemented. The original Phase 5 status listed all three as unavailable.

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
- ~~Machine-wide provider/action inventory grouped by severity.~~ **Moved to
  Phase 4A (2026-10-14)** as the `S` system-scope Targets view.
- Project-hygiene findings and explanations (data from Phase 4).
- Fast startup from the index with visible freshness state and background
  refresh, extending the TUI's static restart cache.

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

1. **Phase 5A M0–M5 complete:** shared cleanup pipeline, per-candidate
   selection, browser/targets/review/run/results, cached totals, safety parity,
   freshness, terminal polish and documentation are implemented (details above).
2. **Phase 4A coverage/system scope implemented:** expanded folder detectors,
   provider coverage, `--system`, global `S`, and removal of `m`. Wallpaper cleanup
   remains gated on real macOS version/schema and quiescence validation; discovery
   is informational until that evidence exists.
3. **Phase 4 project hygiene implemented:** repository/worktree activity,
   retention, shared Cargo ownership safeguards, and Git-ignore evidence for tier-2
   detectors. Phase 4B provider sizing/cleanup completion above comes next, before Phase 5
   indexing/scheduling and the remaining TUI work.
4. **Resolved Cargo observation (2026-10-13):** stale `cargo.target` actions
   intentionally require aggressive severity; the targets UI now says “needs
   aggressive” at lower levels. Fourteen days is an artifact threshold, not
   proof that the repository is abandoned.
5. Alongside normal feature work, finish the Phase 1 largest-file view,
   packed-record parser fixtures and a checked-in performance baseline.
6. Keep the TUI follow-ups listed above: bare `cleanrs` launch, history view,
   index-backed startup and mouse support. Browser-arena discovery reuse and
   selection-scoped fresh review are implemented in the post-M5 improvements.
7. Revisit Trash only as an explicit moved-not-reclaimed workflow, and add
   observed APFS container-free-space evidence only when its measurement
   boundary is truthful and reproducible.

**Correction:** the previous continuation listed Phases 4A and 4 as future work.
Their implementations are now integrated on `feature/phase-4a-target-coverage`.
The wallpaper deletion gate above is intentionally unresolved; no supported
macOS version is claimed without observed schema/regeneration evidence.
