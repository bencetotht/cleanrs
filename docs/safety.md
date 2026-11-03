# Safety

Safety rules are checked in planning and execution. A dry run is useful for
review, but is not the mechanism enforcing path invariants.

| Invariant | Enforcement |
|---|---|
| Dry-run by default | `CleanArgs.apply` and `run_clean` in [main.rs](../cleanrs/src/main.rs); TUI review requires `y`, not Enter |
| No implicit user data | `CleanupLevel::selects`, explicit-only policy and selection checks in [targets.rs](../cleanrs-core/src/targets.rs) |
| Candidate-level cloud checks | `validate_candidate` / `validate_scanned_candidate` in [targets.rs](../cleanrs-core/src/targets.rs), known-path checks in [providers.rs](../cleanrs-core/src/providers.rs) and [system_targets.rs](../cleanrs-core/src/system_targets.rs) |
| No cwd-discovered config | `ResolvedRegistry::load` in [registry.rs](../cleanrs/src/registry.rs) |
| Explicit command-config trust | [trust.rs](../cleanrs/src/trust.rs) and `merge_file` in [registry.rs](../cleanrs/src/registry.rs): canonical path plus SHA-256 of current contents |
| Literal argv, no shell | `CommandSpec` in [targets.rs](../cleanrs-core/src/targets.rs), command execution in [execution.rs](../cleanrs-core/src/execution.rs) |
| Refuse destructive root execution | `enforce_apply_boundary` in [pipeline.rs](../cleanrs/src/pipeline.rs) |
| Revalidate immediately before action | `validate_direct_preflight`, `revalidate_provider` and execution loop in [pipeline.rs](../cleanrs/src/pipeline.rs); descriptor checks in [execution.rs](../cleanrs-core/src/execution.rs) |
| Trash is not reclamation | `run_clean` rejects reserved `--trash` in [main.rs](../cleanrs/src/main.rs); separate evidence fields in [journal.rs](../cleanrs-core/src/journal.rs) |

Workspace candidates must remain beneath their detected project or explicit
configured scan scope. Built-in system storage uses inspected provider-owned
paths, which may lie outside a folder scan. Direct-delete candidates must stay
away from protected roots and cloud storage, with safe ancestry and the reviewed
filesystem identity. Known cloud roots include `~/Library/Mobile Documents`
and `~/Library/CloudStorage`. Dataless status is checked per candidate and within
its scanned subtree; checking only the target root would miss generated
artifacts inside synced projects. Cloud deletion propagates to other devices;
Trash would not make it safe. Materialized files in known synced roots are also
refused. Arbitrary third-party sync locations cannot all be discovered reliably.

Traversal does not follow symlinks and does not cross mount boundaries by
default. Execution uses directory descriptors and relative deletion with
no-follow checks, verifies device/inode identity and refuses changed candidates.
A manual selection can override age, retention and level policy, but cannot
turn a refused or informational candidate into an executable action.

Provider identities include executable, version and context where available;
reinspection must preserve the reviewed operation. Docker actions pin the local
context/endpoint rather than following a later ambient default. Configured
commands are trusted instructions with unknown effects: trust is not a sandbox
for arbitrary executables. Global provider inspection runs from `/` to avoid
inheriting the caller repository's tool configuration. Git/Cargo inspections
carry explicit scope and additional safeguards.

`--yes` requires `--apply`; unattended execution is limited to 100 actions and
50 GiB of known planned bytes, and cannot run explicit-only actions. Interactive
confirmation does not bypass path checks. Stop requests wait for the current
action, and completed or aborted outcomes remain in history.

History records removed allocation, provider-reported bytes and (reserved) moved
bytes separately. None is labelled as observed APFS container recovery. Current
cleanup uses permanent deletion or native tool commands; Trash execution is not
implemented. Scheduling and persistent incremental indexing are also deferred.

Development and provider examples must use temp-dir fixture trees. Never run
`--apply` outside a specifically chosen disposable sandbox, and never test
against the real home directory.
