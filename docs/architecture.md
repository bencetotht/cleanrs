# Architecture

The workspace has two crates. Paths below refer to source modules, not extra crates.

| `cleanrs-core` module | Responsibility |
|---|---|
| [`fs`](../cleanrs-core/src/fs/mod.rs) | Metadata backends, arena, bounded traversal, coverage, doctor |
| [`accounting`](../cleanrs-core/src/accounting.rs) | Selected-set intervals, hardlinks and clone-family bounds |
| [`targets`](../cleanrs-core/src/targets.rs) | Classification, candidate identity, detectors and policy |
| [`providers`](../cleanrs-core/src/providers.rs) | Tool identities, bounded read-only inspection and built-ins |
| [`system_targets`](../cleanrs-core/src/system_targets.rs) | Known tool paths and system storage; platform gates |
| [`projects`](../cleanrs-core/src/projects.rs) | Project activity, Git evidence, retention and ownership |
| [`execution`](../cleanrs-core/src/execution.rs) | Descriptor-relative deletion and bounded argv execution |
| [`journal`](../cleanrs-core/src/journal.rs) | Append-only action history and rotation |
| [`index`](../cleanrs-core/src/index.rs) | Index groundwork; persistent incremental indexing is deferred |

| `cleanrs` module | Responsibility |
|---|---|
| [`main`](../cleanrs/src/main.rs) | Clap interface, command dispatch, JSON and human reports |
| [`pipeline`](../cleanrs/src/pipeline.rs) | Shared CLI/TUI planning, selection, execution boundaries and journal |
| [`registry`](../cleanrs/src/registry.rs) | Config loading, TOML/CLI targets and export |
| [`trust`](../cleanrs/src/trust.rs) | Canonical-path/content-hash command trust |
| [`terminal`](../cleanrs/src/terminal.rs) | Progress, logging and terminal-aware CLI output |
| [`tui`](../cleanrs/src/tui/mod.rs) | Terminal lifecycle and event loop |
| [`tui/app`](../cleanrs/src/tui/app.rs) | Browser/targets/review/run/results state and transitions |
| [`tui/worker`](../cleanrs/src/tui/worker.rs) | Bounded scans, cancellation, planning/execution workers |
| [`tui/cache`](../cleanrs/src/tui/cache.rs) | Validated restart cache and in-memory scan reuse |
| [`tui/keymap`](../cleanrs/src/tui/keymap.rs), [`event`](../cleanrs/src/tui/event.rs), [`theme`](../cleanrs/src/tui/theme.rs) | Shared bindings/help, messages and appearance |
| [`tui/views`](../cleanrs/src/tui/views/mod.rs) | Browser, targets, review, run, results and help rendering |

## Cleanup pipeline

```text
metadata scan → detect targets / inspect providers → immutable reviewed plan
                                                        ↓
                                                  review / confirm
                                                        ↓
                     revalidate candidate identity, provider and exact operation
                                                        ↓
                     descriptor-relative deletion OR executable + argv (no shell)
                                                        ↓
                                        journal → results / history
```

Folder and system scopes share the pipeline; system scope omits a folder walk.
Direct deletion checks ancestry, cloud/dataless flags and device/inode identity.
Provider commands use the inspected executable and context. Execution failures
and validation aborts remain action-level outcomes in history. Allocation removed
and provider reports are recorded as evidence, not observed container recovery.

The TUI's event loop owns UI state. An input thread sends keyboard/resize events;
worker threads perform scans, inspection, fresh review, and execution. Request
identities prevent late worker results from replacing newer selections. Cached
browser sizes speed navigation and restart; cached data never authorizes cleanup.
`x` rebuilds review from fresh selected evidence. Stop requests take effect after
the current action, preserving completed outcomes.

## State on disk

| Data | Location | Overrides |
|---|---|---|
| Target registry | `~/.config/cleanrs/targets.toml` | Additional file through `--config`; no `XDG_CONFIG_HOME` support |
| Command trust | `~/.config/cleanrs/trusted-configs.json` | Uses `HOME`; no XDG override |
| History | `~/.local/state/cleanrs/history.jsonl` | `XDG_STATE_HOME/cleanrs/history.jsonl` |
| Rotated history | Same path with `.1` suffix | Current journal rotates at 8 MiB by default |
| TUI restart cache | `~/.cache/cleanrs/tui-v1/` | Absolute `XDG_CACHE_HOME/cleanrs/tui-v1/`; relative overrides are ignored |

These paths apply on macOS too; the app does not move its state into
`~/Library`. The journal resolver accepts `XDG_STATE_HOME` as supplied (including
a relative value); use an absolute path. TUI cache files are bounded, checksummed,
owner/permission-validated and disposable. The current cache is not an FSEvents
index. Scheduling and persistent incremental indexing remain design work, with
no corresponding CLI commands exposed today.
