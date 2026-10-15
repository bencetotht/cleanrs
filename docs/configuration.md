# Configuration

The loader in [registry.rs](../cleanrs/src/registry.rs) reads
`~/.config/cleanrs/targets.toml`, then an explicitly named `--config PATH`.
It never searches the working directory. The default config and trust store do
not follow `XDG_CONFIG_HOME`. Later definitions replace earlier targets with the
same ID; a configured ID also replaces the built-in definition with that ID.
Home-relative paths (`~/…`) expand using `HOME`, not shell evaluation.

## File reference

Unknown fields are rejected in `RegistryFile`, `ConfiguredTarget`, `DetectConfig`,
and `ReclaimConfig`. The top-level fields are:

| Field | Type / default | Meaning |
|---|---|---|
| `enable` | string array / `[]` | Re-enable target IDs; not an allow-list |
| `disable` | string array / `[]` | Disable target IDs; processed after `enable` in each file |
| `[[target]]` | table array / `[]` | Custom or replacement definitions |
| `[[retention]]` | table array / `[]` | Project retention policy |

Each `[[target]]` accepts:

| Field | Type / default | Meaning |
|---|---|---|
| `id` | required string | Nonempty stable ID |
| `description` | string / `""` | Review text; empty uses a generated description |
| `detect` | required table | Exactly one detector form below |
| `class` | required string | `free`, `cheap`, `network`, `expensive`, `irreplaceable` |
| `reclaim` | required table | Exact deletion or command operation |
| `stale_after` | optional string | Whole unsigned days with `d`, e.g. `"14d"`, `"0d"`; absent means zero |
| `minimum_level` | optional string | `safe`, `standard`, `aggressive`; absent derives from class |
| `scope` | string array / `[]` | Prefix filter on project roots for workspace marker detectors |

Classes expand into independent policy properties in
[targets.rs](../cleanrs-core/src/targets.rs):

| Class | Recoverability | Rebuild cost | Default minimum | Automation |
|---|---|---|---|---|
| `free` | generated | trivial | safe | unattended |
| `cheap` | generated | moderate | safe | unattended |
| `network` | downloadable | moderate | standard | confirmed |
| `expensive` | downloadable | high | aggressive | confirmed |
| `irreplaceable` | user data | unknown | safe* | explicit target only |

*The default minimum does not grant permission: user data still requires explicit
naming/selection at every level. Classes also encode whether downloads are needed.
`minimum_level` can raise the threshold, but cannot override recoverability or
explicit-only policy. `stale_after` applies to path targets; configured commands
have no age measurement and are considered stale immediately. `scope` does not
restrict command or explicit-path detectors.

### `detect`

| Field | Type / default | Meaning |
|---|---|---|
| `path` | optional string | Explicit path inside the scan; use absolute or home-relative paths |
| `marker` | optional string | One project-root marker file |
| `markers` | string array / `[]` | Alternative project-root marker files; any one suffices |
| `dir` | optional string | Relative candidate directory below a project root |
| `inner_marker` | optional string | Required file inside the candidate directory |
| `command` | optional string | Executable name/path to resolve for a command target |

Choose `path`, `command`, or the workspace-marker form. Do not combine `marker`
and `markers`. Project-root markers need `dir` or `inner_marker`; `dir` alone is
invalid. With only `inner_marker`, directories containing that file are found
under scanned parent directories, allowing arbitrary virtualenv names. Marker
and directory paths must use normal relative components: no `/` prefix, `..`,
`.`, empty components, or symlink traversal.

### `reclaim`

| Field | Type / default | Meaning |
|---|---|---|
| `method` | required string | `delete`, `delete-children`, or `run` |
| `children_only` | boolean / `false` | For path targets, retain the root directory |
| `argv` | string array / `[]` | For `run`, executable first, then literal arguments |

Path/marker targets require `delete` or `delete-children` with empty `argv`.
Either `method = "delete-children"` or `children_only = true` retains the root.
Command targets require `method = "run"` and `argv[0]` exactly equal to
`detect.command`; `children_only` has no effect on commands. No shell, variable
expansion, pipes, or redirections are evaluated in argv.

### Retention

Retention rules are defined in [projects.rs](../cleanrs-core/src/projects.rs):

```toml
[[retention]]
path = "~/Developer/important"
project_only = false
artifact_days = 30
dormant_days = 90
keep = true
```

`path` and `artifact_days` are required. `path` must be absolute or home-relative,
without `..`; existing paths are canonicalized. `project_only` defaults to false,
`dormant_days` to 90, and `keep` to false. `project_only = true` matches exactly
that project root; otherwise a rule covers the path prefix. The most specific
matching prefix wins; an exact project rule takes precedence. A later rule
with the same path and `project_only` replaces the earlier one. `keep` prevents
automatic recommendation, while explicit selection can override retention policy.
Unknown retention fields are rejected too.

## CLI-only targets and filters

These flags exist on `targets`, `projects`, and `clean`:

```sh
# Use a throwaway fixture; this only inventories it.
sandbox=$(mktemp -d)
mkdir -p "$sandbox/cache"
cleanrs targets "$sandbox" --targets-only-cli \
  --target "$sandbox/cache:cheap:delete-children"

# A literal command array; inventory does not execute the cleanup argv.
cleanrs targets "$sandbox" --targets-only-cli \
  --target-argv '["go", "clean", "-modcache"]'
```

- `--target PATH:CLASS[:delete|delete-children]` adds IDs `cli.path.0`, etc.
  Its default staleness is zero.
- `--target-argv JSON` adds `cli.command.0`, etc., class `cheap`, with unknown
  bytes. Classify lasting recipes in TOML rather than relying on that default.
- `--targets-only-cli` suppresses built-ins and all config; conflicts with `--config`.
- `--no-builtin-targets` suppresses embedded workspace and provider definitions
  but preserves configured and CLI targets.
- `--config PATH` adds an explicit registry after the default user config.
- `--trust-config-commands` requires `--config` and persists trust for its bytes.
- `--only ID` / `--exclude ID` filter stable IDs and can be repeated.

`clean --select PATH` (or a full command CandidateKey) selects exact candidates
and overrides age/level policy, never safety. `--only` names target IDs and also
participates in explicit-only selection. `--system` inventories command/system
providers without a folder scan; configured direct paths require a folder scope.
`projects --system` is rejected even though the shared clap options display it.

## Trust and export

Commands in the default user config are implicitly trusted. Commands in an
explicit config require trust even if that path names the default config file:

```sh
cleanrs trust-config /absolute/path/to/targets.toml
cleanrs trust-config --list
cleanrs trust-config /absolute/path/to/targets.toml --revoke
cleanrs targets . --config /absolute/path/to/targets.toml --workspace-only
cleanrs targets --export
```

Trust records bind the canonical path and SHA-256 of the whole file; editing or
moving it requires review and trust again. Path-only explicit configs need no
command trust. Trust does not weaken path checks or authorize root execution.

`targets --export` prints the resolved custom definitions, retention rules,
source paths, and a built-in ID list without scanning. **Correction:** this is
an inspection report, not an importable round-trip config: `builtin` and `sources`
are output-only fields and enable/disable policy is not exported. Remove those
fields and restore desired policy when using exported definitions as a new config.
