# Extending targets and providers

Use TOML when a detector and one fixed operation describe the target. Use a Rust
provider when inspection, identity, context, measurement or revalidation requires
tool-specific logic. The [configuration reference](configuration.md) lists every
accepted field and the [safety guide](safety.md) explains execution boundaries.

## 1. TOML targets

### Explicit path

Put this in `~/.config/cleanrs/targets.toml`:

```toml
[[target]]
id = "custom.gradle-cache"
description = "Gradle dependency cache; later builds may download dependencies"
detect = { path = "~/.gradle/caches" }
class = "network"
minimum_level = "standard"
reclaim = { method = "delete-children" }
```

The root survives; its children are removed if cleanup is later confirmed.
The path must be inside the explicit folder scan. This recipe does not implement
the built-in Gradle provider's daemon/lock checks; close Gradle processes and
review the consequences before using a custom operation on actual storage.

### Workspace marker and directory

A complete alternate-registry example:

```toml
[[target]]
id = "custom.tool-output"
description = "Generated fixture-tool output"
detect = { marker = "fixture-tool.toml", dir = ".fixture-output" }
class = "cheap"
stale_after = "14d"
minimum_level = "safe"
reclaim = { method = "delete" }
```

`markers = ["build.gradle", "build.gradle.kts"]` offers alternative marker files.
`inner_marker = "pyvenv.cfg"` can detect arbitrarily named child virtualenvs;
combine it with project markers and `dir` when a narrower detector is possible.
Do not use broad names such as `build` without sufficient project evidence.

Test the example without touching real caches:

```sh
sandbox=$(mktemp -d)
mkdir -p "$sandbox/project/.fixture-output" "$sandbox/home"
printf '# fixture\n' > "$sandbox/project/fixture-tool.toml"
printf 'generated\n' > "$sandbox/project/.fixture-output/example"
# Save the marker example above as "$sandbox/targets.toml", then:
HOME="$sandbox/home" cleanrs targets "$sandbox/project" \
  --config "$sandbox/targets.toml" --no-builtin-targets --workspace-only
HOME="$sandbox/home" cleanrs clean "$sandbox/project" \
  --config "$sandbox/targets.toml" --no-builtin-targets --workspace-only
```

Both invocations are read-only. A fresh fixture will not satisfy the 14-day age
policy; inspect that skip, or add `--ignore-staleness` to the dry-run `clean`.
Any future destructive testing must stay inside this disposable sandbox. Never
use `--apply` outside a throwaway directory for provider development.

### Command target

```toml
[[target]]
id = "custom.go-modcache"
description = "Remove downloaded Go modules; future builds download them again"
detect = { command = "go" }
class = "network"
minimum_level = "standard"
reclaim = { method = "run", argv = ["go", "clean", "-modcache"] }
```

`detect.command` must match the first argv element exactly. The executable is
resolved; cleanup arguments are not executed by `targets`. No shell string is
accepted. Configured commands have unknown byte estimates and no meaningful age
threshold; use a Rust provider for trustworthy inspection and ownership checks.

The default user config implicitly trusts commands. For a separate reviewed
file, use `cleanrs trust-config /absolute/path/to/targets.toml`, then
`cleanrs targets --system --config /absolute/path/to/targets.toml`. That command
inventories the configured operation; it does not run `go clean`. Editing the
file invalidates explicit trust. Testing execution of a tool command requires
redirecting that tool's storage into the sandbox too; a sandbox scan path alone
does not constrain the effects of an arbitrary command.

## 2. Rust providers

The interface lives in [providers.rs](../cleanrs-core/src/providers.rs):

```rust,ignore
pub trait Provider {
    fn id(&self) -> &'static str;
    fn action_ids(&self) -> &'static [&'static str];
    fn detect(&self, host: &dyn ProviderHost)
        -> Result<Option<ProviderIdentity>, ProviderError>;
    fn inspect(&self, identity: &ProviderIdentity,
        selected_ids: &HashSet<String>, host: &dyn ProviderHost)
        -> Result<Vec<DetectedTarget>, ProviderError>;
}
```

`id` identifies the tool, `action_ids` supplies stable filterable operations,
`detect` resolves identity without mutation, and `inspect` returns reviewable
candidates for selected action IDs. Detection absence and inspection failure
must remain distinguishable; unavailable tools are not zero-byte caches.

All tool resolution and inspection goes through `ProviderHost`:
`resolve_executable`, `run_inspection`, `home`, and `variable`. Stub hosts can
supply exact outputs and opt into fixture storage discovery without visiting the
real home directory. `SystemProviderHost` bounds subprocess time and output.
The comment in `run_bounded` explains why global inspection sets cwd to `/`:
repository-local configuration must not change a global cache inspection.
Cargo/Git exceptions use explicit manifests or `-C` scope and extra safeguards.
Do not spawn subprocesses directly from provider code.

A `ProviderIdentity` records `id`, resolved executable, version and context.
Carry it unchanged into candidates; contexts such as Docker endpoints are part
of the reviewed identity. The shared pipeline reinspects before execution and
compares identity and exact operation. New special identity rules may also need
an explicit branch in `revalidate_provider` in
[pipeline.rs](../cleanrs/src/pipeline.rs); adding discovery alone is insufficient.

Register a provider in `inspect_builtin_providers_with_progress` and add its
stable action IDs to `BUILTIN_PROVIDER_ACTION_IDS`. Ensure filtering, status
reporting, registry export, and revalidation all agree on those IDs. Known-path
storage discovery may belong in [system_targets.rs](../cleanrs-core/src/system_targets.rs)
or the existing storage helpers instead of a new abstraction.

### Example: pnpm

`PNPM_PROVIDER` uses `SimpleProvider`, executable `pnpm`, and `PNPM_ACTIONS`:

1. Detection resolves pnpm and obtains its version through the host.
2. The action `pnpm.store-prune` inspects `pnpm store path` for location.
3. `inspect_simple_action` delegates to storage helpers for bounded measurement
   and safety evidence; failures stay explicit.
4. The planned command is `pnpm store prune`, with class `network` and minimum
   level `standard`: later installs may download removed packages again.
5. The pipeline refreshes the provider and operation before executing that argv.

Follow this pattern for simple tools instead of duplicating parsing and command
construction. For contextual or stateful tools, follow Docker or storage-provider
implementations and their tests.

Each candidate needs a class and expanded `TargetProperties` (recoverability,
rebuild cost, network requirement, automation), minimum cleanup level, safety
status, exact operation, usage evidence, consequence and identity. `user_data`
means `Recoverability::UserData`, not a valid TOML class: its class shorthand is
`irreplaceable`. Never assign a lower-risk class merely to make automatic
selection convenient. Unknown byte counts must remain unknown.

### Tests

Use the `FixtureHost` and parser/inspection tests in
[providers.rs](../cleanrs-core/src/providers.rs), plus fixtures in
[storage_tests.rs](../cleanrs-core/src/providers/storage_tests.rs) and
[system_targets.rs](../cleanrs-core/src/system_targets.rs). Use temp directories
for filesystem identity and refusal cases. Project-hygiene tests in
[projects.rs](../cleanrs-core/src/projects.rs) demonstrate temp Git repositories.

Test missing executables, nonzero exits, malformed/partial output, time/output
bounds, changed identity/context, cloud/dataless descendants, symlink ancestry,
protected user data, stale policy and exact argv. Verify detection and planning
never execute cleanup. Keep live tool/macOS validation separate from stub evidence.

## 3. Contribution checklist

- Explain what owns the data, recovery cost, and whether downloads are needed.
- Prefer supported tool cleanup interfaces to deleting managed internal files.
- Keep read-only inspection distinct from cleanup; bound time and output.
- Pin scope/context, document side effects, and revalidate the exact operation.
- Preserve per-candidate cloud, dataless, symlink and explicit-user-data checks.
- Use labelled intervals, provider reports or unknowns rather than invented sizes.
- Add stub-host/temp-dir tests and record platform-specific validation gaps.
- Update the design reference and development plan when behavior changes.

System package managers requiring root (`apt`, `dnf`/`yum`, `pacman`) are not
supported: the shared apply boundary refuses root. A future privileged-provider
design is **not implemented**; it would need a narrowly scoped privilege boundary,
validated operations/identity and auditable authorization rather than running
the entire scanner and arbitrary configured commands as root.
