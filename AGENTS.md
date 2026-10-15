# AGENTS.md

## What this is

`cleanrs` — a macOS-first storage analyzer and developer-cache cleaner in Rust. It answers "how much space will deleting this actually return," which on APFS is not the same as "how big is this."

## Structure

```
VISION.md             The design reference. Read it before proposing anything.
DEVELOPMENT_PLAN.md   Phased build order with exit criteria. Follow it; don't skip ahead.
README.md             Short public summary. Must stay consistent with VISION.md.
AGENTS.md             This file.
docs/                 installation, configuration, providers, accounting, architecture,
                      safety, releasing (all .md).
scripts/              Versioning, packaging, and fixture verification.
packaging/            Homebrew formula template.
flake.nix             Nix packages and development shell (flake.lock pins inputs).
```

Code layout: `cleanrs-core` (library) + `cleanrs` (binary), with `fs`, `accounting`, `targets`, `providers`, `index`, `journal` as modules. Resist splitting into more crates early, only if you really deem it necessary.

## Working on the docs

- **Verify macOS claims against the installed SDK**, not blog posts. Man pages and headers live under `/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/`. Several earlier assertions in this document were wrong because they came from secondary sources.
- **Keep README.md and VISION.md in sync.** The README makes promises; the VISION defines them.
- If a correction invalidates an earlier claim, state the correction in the doc rather than silently deleting the old text.

## Coding Guidelines

These come from VISION.md and are not open for convenience trade-offs:

- Release via the dispatch-only Release workflow; never tag or bump versions by hand.
- **Dry-run is the default.** `--apply` is always explicit.
- **Never delete synced cloud content** (§2, §8.2). Deletion propagates to every device. Check dataless status per candidate path, not just target roots.
- **Never delete `user_data`** without explicit naming — no level reaches it.
- **Never load config from the working directory** (§9.1). Commands are argv arrays; there is no shell.
- **Trash is not reclamation.** Report moved and reclaimed separately.
- **Estimates are intervals with confidence** (§3.1), never bare point values. Do not sum per-file `PRIVATESIZE` across a set.
- **Path invariants are enforced in code** (§8.2), not implied by dry-run.
- Do not run the --apply command without using a specific sandbox directory. 
- Filesystem tests run against fixture trees in temp dirs, never the real home directory.
- After implementing a feature, always update the DEVELOPMENT_PLAN.md document with the implementation details and changes. Use the document as a progress tracker as well.

When coding, try to follow best practices regarding code quality. Make sure you use appropriate logging libraries, try to use conventional structure and colors to make the logs easy to understand for the users (for progresses, also use progress bars, etc.)

During development, make sure to always comment the most important parts of your code. Try to comment as much as it would make sense to help improve maintainability and understandability of the code.
