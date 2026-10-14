//! Read-only developer-tool detection, inspection, and cleanup planning.
//!
//! Inspection remains read-only. Planned cleanup commands are handed to the
//! Phase 3 executor as an executable plus argv array; no shell is involved.

use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::Confidence;
use crate::accounting::estimate_subtree;
use crate::fs::{NodeIndex, NodeKind, ScanResult};
use crate::targets::{
    Automation, CandidateIdentity, CandidateSafety, CleanupLevel, CommandSpec, DetectedTarget,
    PlannedOperation, ProviderIdentity, RebuildCost, TargetClass, UsageEstimate,
    cargo_activity_age_days, subtree_sizes, validate_scanned_candidate,
};

/// Confirm that Cargo still resolves the same workspace-local build directory.
/// A config/environment change may otherwise redirect a previously reviewed clean.
pub fn revalidate_cargo_build(
    target: &DetectedTarget,
    host: &dyn ProviderHost,
) -> Result<(), String> {
    let ownership = target
        .cargo_ownership
        .as_ref()
        .ok_or("Cargo ownership evidence missing")?;
    let root = target
        .project_root
        .as_ref()
        .ok_or("Cargo workspace missing")?;
    if !ownership.complete
        || ownership.observed_owners != [root.clone()]
        || ownership.target_directory != root.join("target")
    {
        return Err("Cargo shared/custom ownership cannot authorize execution".into());
    }
    let executable = target
        .provider
        .executable
        .clone()
        .ok_or("Cargo executable missing")?;
    let command = CommandSpec {
        executable,
        args: vec![
            "metadata".into(),
            "--no-deps".into(),
            "--format-version".into(),
            "1".into(),
            "--frozen".into(),
            "--manifest-path".into(),
            root.join("Cargo.toml").display().to_string(),
        ],
        environment: BTreeMap::new(),
    };
    let output = host.run_inspection(&command).map_err(|e| e.to_string())?;
    if output.exit_code != Some(0) {
        return Err("Cargo metadata unavailable during revalidation".into());
    }
    let value: serde_json::Value =
        serde_json::from_str(&output.stdout).map_err(|e| e.to_string())?;
    if value["workspace_root"].as_str().map(Path::new) != Some(root.as_path())
        || value["target_directory"].as_str().map(Path::new)
            != Some(ownership.target_directory.as_path())
    {
        return Err("Cargo workspace or target directory changed after review".into());
    }
    Ok(())
}

/// Stable IDs exposed by the built-in Homebrew provider.
pub const HOMEBREW_ACTION_IDS: &[&str] = &[
    "homebrew.cleanup",
    "homebrew.cleanup.scrub",
    "homebrew.autoremove",
];

/// Every built-in provider action ID accepted by `--only` and `--exclude`.
pub const BUILTIN_PROVIDER_ACTION_IDS: &[&str] = &[
    "gradle.caches",
    "gradle.wrapper",
    "gradle.daemon-logs",
    "android.system-images",
    "android.build-tools",
    "android.avds",
    "metro.cache",
    "expo.global-cache",
    "watchman.watches",
    "go.build-cache",
    "go.module-cache",
    "maven.repository",
    "playwright.browsers",
    "puppeteer.browsers",
    "node-gyp.headers",
    "swiftpm.repositories",
    "typescript.cache",
    "vmware.bundles",
    "claude.cowork",
    "wallpaper.cache",
    "homebrew.cleanup",
    "homebrew.cleanup.scrub",
    "homebrew.autoremove",
    "nix.gc",
    "nix.generations",
    "pnpm.store-prune",
    "npm.cache-verify",
    "npm.npx-cache",
    "yarn.cache-clean",
    "bun.cache-clean",
    "uv.cache-prune",
    "uv.cache-clean",
    "pip.cache-purge",
    "poetry.cache-clean",
    "conda.cache-clean",
    "poetry.environments",
    "conda.environments",
    "rustup.toolchains",
    "cargo.shared-cache",
    "orbstack.machines",
    "xcode.simulators-unavailable",
    "xcode.derived-data",
    "xcode.archives",
    "xcode.device-support",
    "xcode.simulator-data",
    "cocoapods.cache-clean",
    "docker.build-cache",
    "docker.stopped-containers",
    "docker.images",
    "docker.volumes",
];

/// Result of probing one provider, including absence and inspection failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderStatus {
    pub id: String,
    pub detected: bool,
    pub version: Option<String>,
    pub context: Option<String>,
    pub message: String,
}

/// Combined provider findings and detection diagnostics.
#[derive(Debug, Default, Serialize)]
pub struct ProviderInventory {
    #[serde(skip)]
    pub scans: Vec<ScanResult>,
    pub items: Vec<DetectedTarget>,
    pub statuses: Vec<ProviderStatus>,
}

/// Captured output from a bounded read-only inspection command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Failure produced while detecting or inspecting a provider.
#[derive(Debug)]
pub struct ProviderError {
    message: String,
}

impl ProviderError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProviderError {}

/// Host operations injected into providers so parsers and plans can be tested
/// without touching a developer's real caches.
pub trait ProviderHost {
    fn resolve_executable(&self, name: &str) -> Result<Option<PathBuf>, ProviderError>;
    fn run_inspection(&self, command: &CommandSpec) -> Result<CommandOutput, ProviderError>;
}

/// Real provider host with subprocess time and output bounds.
#[derive(Debug, Clone)]
pub struct SystemProviderHost {
    timeout: Duration,
    poll_interval: Duration,
    max_output_bytes: usize,
}

impl Default for SystemProviderHost {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            poll_interval: Duration::from_millis(10),
            max_output_bytes: 4 * 1024 * 1024,
        }
    }
}

impl SystemProviderHost {
    /// Short bounded commands for bulk project inspection.
    pub fn project_inspection() -> Self {
        Self {
            timeout: Duration::from_secs(2),
            poll_interval: Duration::from_millis(2),
            max_output_bytes: 4 * 1024 * 1024,
        }
    }
}

impl ProviderHost for SystemProviderHost {
    fn resolve_executable(&self, name: &str) -> Result<Option<PathBuf>, ProviderError> {
        resolve_from_path(name)
    }

    fn run_inspection(&self, command: &CommandSpec) -> Result<CommandOutput, ProviderError> {
        run_bounded(
            command,
            self.timeout,
            self.max_output_bytes,
            self.poll_interval,
        )
    }
}

/// A built-in provider supplies read-only detection and inspection. The shared
/// executor revalidates the resulting identity immediately before execution.
pub trait Provider {
    fn id(&self) -> &'static str;
    fn action_ids(&self) -> &'static [&'static str];

    fn detect(&self, host: &dyn ProviderHost) -> Result<Option<ProviderIdentity>, ProviderError>;

    fn inspect(
        &self,
        identity: &ProviderIdentity,
        selected_ids: &HashSet<String>,
        host: &dyn ProviderHost,
    ) -> Result<Vec<DetectedTarget>, ProviderError>;
}

/// Inspect all built-in providers requested by the resolved target selection.
#[must_use]
pub fn inspect_builtin_providers(
    host: &dyn ProviderHost,
    selected_ids: &HashSet<String>,
) -> ProviderInventory {
    inspect_builtin_providers_with_progress(host, selected_ids, |_| {})
}

pub fn inspect_builtin_providers_with_progress(
    host: &dyn ProviderHost,
    selected_ids: &HashSet<String>,
    mut progress: impl FnMut(&str),
) -> ProviderInventory {
    let providers: Vec<&dyn Provider> = vec![
        &HomebrewProvider,
        &NIX_PROVIDER,
        &PNPM_PROVIDER,
        &NPM_PROVIDER,
        &YARN_PROVIDER,
        &BUN_PROVIDER,
        &UV_PROVIDER,
        &PIP_PROVIDER,
        &POETRY_PROVIDER,
        &CONDA_PROVIDER,
        &RUSTUP_PROVIDER,
        &CARGO_PROVIDER,
        &ORBSTACK_PROVIDER,
        &XcodeProvider,
        &COCOAPODS_PROVIDER,
        &DockerProvider,
    ];
    let mut inventory = ProviderInventory::default();
    for provider in providers {
        if !provider
            .action_ids()
            .iter()
            .any(|id| selected_ids.contains(*id))
        {
            continue;
        }
        progress(provider.id());
        match provider.detect(host) {
            Ok(None) => inventory.statuses.push(ProviderStatus {
                id: provider.id().into(),
                detected: false,
                version: None,
                context: None,
                message: "executable not found on PATH".into(),
            }),
            Err(error) => inventory.statuses.push(ProviderStatus {
                id: provider.id().into(),
                detected: true,
                version: None,
                context: None,
                message: format!("detection failed: {error}"),
            }),
            Ok(Some(identity)) => match provider.inspect(&identity, selected_ids, host) {
                Ok(items) => {
                    inventory.statuses.push(ProviderStatus {
                        id: provider.id().into(),
                        detected: true,
                        version: identity.version.clone(),
                        context: identity.context.clone(),
                        message: format!("{} action(s) inspected", items.len()),
                    });
                    inventory.items.extend(items);
                }
                Err(error) => inventory.statuses.push(ProviderStatus {
                    id: provider.id().into(),
                    detected: true,
                    version: identity.version.clone(),
                    context: identity.context.clone(),
                    message: format!("inspection failed: {error}"),
                }),
            },
        }
    }
    inventory
}

/// Resolve Cargo workspace/target ownership with mutation-disabled metadata
/// calls and produce one candidate per distinct build directory.
#[must_use]
pub fn inspect_cargo_builds(
    scan: &ScanResult,
    host: &dyn ProviderHost,
    home: Option<&Path>,
    stale_after_days: u32,
) -> ProviderInventory {
    inspect_cargo_builds_with_progress(scan, host, home, stale_after_days, |_, _| {})
}

/// Publish bounded metadata inspection progress using the already discovered
/// manifest list; no extra walk or subprocess is needed for these counters.
pub fn inspect_cargo_builds_with_progress(
    scan: &ScanResult,
    host: &dyn ProviderHost,
    home: Option<&Path>,
    stale_after_days: u32,
    mut progress: impl FnMut(usize, usize),
) -> ProviderInventory {
    let mut inventory = ProviderInventory::default();
    let Ok(Some(cargo)) = host.resolve_executable("cargo") else {
        inventory.statuses.push(ProviderStatus {
            id: "cargo-builds".into(),
            detected: false,
            version: None,
            context: None,
            message: "cargo executable not found on PATH".into(),
        });
        return inventory;
    };
    let version = successful_stdout(host, &plain_inspection_command(&cargo, &["--version"]))
        .ok()
        .map(|value| value.trim().to_owned());
    let identity = ProviderIdentity {
        id: "cargo".into(),
        executable: Some(cargo.clone()),
        version: version.clone(),
        context: Some(format!("scan scope {}", scan.root.display())),
    };
    let mut manifests: Vec<_> = (0..scan.arena.nodes.len())
        .map(NodeIndex::from_usize)
        .filter(|index| scan.arena.name(*index) == "Cargo.toml")
        .filter(|index| scan.arena.nodes[index.as_usize()].kind == NodeKind::File)
        .filter(|index| {
            validate_scanned_candidate(&scan.arena, *index, home) == CandidateSafety::Eligible
        })
        .map(|index| (index, scan.arena.path(index)))
        .filter(|(_, path)| {
            !path.components().any(|component| {
                matches!(
                    component.as_os_str().to_str(),
                    Some("target" | "node_modules" | ".git")
                )
            })
        })
        .collect();
    const MAX_CARGO_METADATA_COMMANDS: usize = 256;
    let omitted_manifests = manifests.len().saturating_sub(MAX_CARGO_METADATA_COMMANDS);
    manifests.truncate(MAX_CARGO_METADATA_COMMANDS);
    let mut groups = BTreeMap::<PathBuf, (PathBuf, NodeIndex, Vec<PathBuf>)>::new();
    let mut failures = omitted_manifests;
    let total = manifests.len();
    for (position, (manifest_index, manifest)) in manifests.into_iter().enumerate() {
        progress(position, total);
        let spec = CommandSpec {
            executable: cargo.clone(),
            args: vec![
                "metadata".into(),
                "--no-deps".into(),
                "--format-version".into(),
                "1".into(),
                "--frozen".into(),
                "--manifest-path".into(),
                manifest.display().to_string(),
            ],
            environment: BTreeMap::new(),
        };
        let Ok(output) = host.run_inspection(&spec) else {
            failures += 1;
            continue;
        };
        if output.exit_code != Some(0) {
            failures += 1;
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&output.stdout) else {
            failures += 1;
            continue;
        };
        let Some(workspace_root) = value["workspace_root"].as_str().map(PathBuf::from) else {
            failures += 1;
            continue;
        };
        let Some(target_dir) = value["target_directory"].as_str().map(PathBuf::from) else {
            failures += 1;
            continue;
        };
        groups
            .entry(target_dir)
            .and_modify(|(first_root, newest_manifest, owners)| {
                if workspace_root < *first_root {
                    *first_root = workspace_root.clone();
                }
                if scan.arena.nodes[manifest_index.as_usize()].modified_unix_seconds
                    > scan.arena.nodes[newest_manifest.as_usize()].modified_unix_seconds
                {
                    *newest_manifest = manifest_index;
                }
                if !owners.contains(&workspace_root) {
                    owners.push(workspace_root.clone());
                }
            })
            .or_insert_with(|| (workspace_root.clone(), manifest_index, vec![workspace_root]));
    }

    progress(total, total);

    for (target_dir, (workspace_root, manifest_index, mut owners)) in groups {
        owners.sort();
        let target_index = (0..scan.arena.nodes.len())
            .map(NodeIndex::from_usize)
            .find(|index| scan.arena.path(*index) == target_dir);
        let local_default = target_dir == workspace_root.join("target");
        let (usage, age_days, mut safety) = if let Some(target_index) = target_index {
            let (apparent, allocated) = subtree_sizes(&scan.arena, target_index);
            (
                UsageEstimate::Filesystem {
                    apparent,
                    allocated,
                    reclaimable: estimate_subtree(
                        &scan.arena,
                        target_index,
                        scan.coverage_complete(),
                    ),
                },
                cargo_activity_age_days(&scan.arena, target_index, manifest_index, now_seconds()),
                validate_scanned_candidate(&scan.arena, target_index, home),
            )
        } else {
            (
                UsageEstimate::Unknown {
                    explanation:
                        "resolved Cargo target directory is outside the explicit scan scope".into(),
                },
                None,
                CandidateSafety::Refused(
                    "resolved Cargo target directory is outside the explicit scan scope".into(),
                ),
            )
        };
        if !local_default && matches!(safety, CandidateSafety::Eligible) {
            safety = CandidateSafety::Refused(
                "custom/shared Cargo target ownership cannot be proven complete outside this scan"
                    .into(),
            );
        }
        let mut properties = TargetClass::Cheap.properties();
        properties.rebuild_cost = Some(RebuildCost::High);
        inventory.items.push(DetectedTarget {
            gitignore: None,
            cargo_ownership: Some(crate::targets::CargoOwnership {
                target_directory: target_dir.clone(),
                observed_owners: owners.clone(),
                complete: local_default && owners.len() == 1 && scan.coverage_complete(),
            }),
            id: "cargo.target".into(),
            provider: identity.clone(),
            description: "Rust workspace build output".into(),
            project_root: Some(workspace_root.clone()),
            path: Some(target_dir.clone()),
            class: TargetClass::Cheap,
            properties,
            minimum_level: CleanupLevel::Aggressive,
            usage,
            age_days,
            stale_after_days,
            stale: age_days.map(|age| age >= u64::from(stale_after_days)),
            safety,
            operation: PlannedOperation::Command {
                command: CommandSpec {
                    executable: cargo.clone(),
                    args: vec![
                        "clean".into(),
                        "--frozen".into(),
                        "--manifest-path".into(),
                        workspace_root.join("Cargo.toml").display().to_string(),
                        "--target-dir".into(),
                        target_dir.display().to_string(),
                    ],
                    environment: BTreeMap::new(),
                },
            },
            exclusive_group: None,
            exclusive_priority: 0,
            consequence: "Rust build output will be rebuilt on the next build".into(),
            evidence: vec![
                format!("workspace root: {}", workspace_root.display()),
                format!("resolved target directory: {}", target_dir.display()),
                format!(
                    "observed owners in scope: {}",
                    owners
                        .iter()
                        .map(|owner| owner.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "activity age uses the newest manifest or build artifact".into(),
            ],
        });
    }
    let directory_label = if inventory.items.len() == 1 {
        "directory"
    } else {
        "directories"
    };
    inventory.statuses.push(ProviderStatus {
        id: "cargo-builds".into(),
        detected: true,
        version,
        context: Some(scan.root.display().to_string()),
        message: format!(
            "{} distinct build {directory_label} resolved; {failures} manifest(s) skipped because frozen metadata was unavailable",
            inventory.items.len(),
        ),
    });
    inventory
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

struct HomebrewProvider;

impl Provider for HomebrewProvider {
    fn id(&self) -> &'static str {
        "homebrew"
    }

    fn action_ids(&self) -> &'static [&'static str] {
        HOMEBREW_ACTION_IDS
    }

    fn detect(&self, host: &dyn ProviderHost) -> Result<Option<ProviderIdentity>, ProviderError> {
        let Some(executable) = host.resolve_executable("brew")? else {
            return Ok(None);
        };
        let version = successful_stdout(host, &inspection_command(&executable, &["--version"]))?;
        let version = version
            .lines()
            .next()
            .unwrap_or("Homebrew version unknown")
            .trim();
        let prefix = successful_stdout(host, &inspection_command(&executable, &["--prefix"]))?;
        Ok(Some(ProviderIdentity {
            id: self.id().into(),
            executable: Some(executable),
            version: Some(version.into()),
            context: Some(format!("prefix {}", prefix.trim())),
        }))
    }

    fn inspect(
        &self,
        identity: &ProviderIdentity,
        selected_ids: &HashSet<String>,
        host: &dyn ProviderHost,
    ) -> Result<Vec<DetectedTarget>, ProviderError> {
        let executable = identity
            .executable
            .as_ref()
            .ok_or_else(|| ProviderError::new("Homebrew executable identity is missing"))?;
        let cache = successful_stdout(host, &inspection_command(executable, &["--cache"]))?;
        let cache = cache.trim();
        let mut items = Vec::new();

        if action_enabled("homebrew.cleanup", selected_ids) {
            items.push(inspect_homebrew_cleanup(host, identity, cache, false));
        }
        if action_enabled("homebrew.cleanup.scrub", selected_ids) {
            items.push(inspect_homebrew_cleanup(host, identity, cache, true));
        }
        if action_enabled("homebrew.autoremove", selected_ids) {
            items.push(inspect_homebrew_autoremove(host, identity));
        }
        Ok(items)
    }
}

#[derive(Clone, Copy)]
struct SimpleAction {
    id: &'static str,
    description: &'static str,
    class: TargetClass,
    level: CleanupLevel,
    location_args: &'static [&'static str],
    inspect_args: &'static [&'static str],
    plan_args: &'static [&'static str],
    consequence: &'static str,
    informational: bool,
    exclusive_group: Option<&'static str>,
    priority: u8,
}

struct SimpleProvider {
    id: &'static str,
    executable: &'static str,
    version_args: &'static [&'static str],
    actions: &'static [SimpleAction],
}

impl Provider for SimpleProvider {
    fn id(&self) -> &'static str {
        self.id
    }

    fn action_ids(&self) -> &'static [&'static str] {
        // All simple-provider declarations below use a stable adjacent ID
        // slice, avoiding allocation during provider filtering.
        match self.id {
            "nix" => &NIX_IDS,
            "pnpm" => &PNPM_IDS,
            "npm" => &NPM_IDS,
            "yarn" => &YARN_IDS,
            "bun" => &BUN_IDS,
            "uv" => &UV_IDS,
            "pip" => &PIP_IDS,
            "poetry" => &POETRY_IDS,
            "conda" => &CONDA_IDS,
            "rustup" => &RUSTUP_IDS,
            "cargo" => &CARGO_IDS,
            "orbstack" => &ORBSTACK_IDS,
            "xcode" => &XCODE_IDS,
            "cocoapods" => &COCOAPODS_IDS,
            _ => &[],
        }
    }

    fn detect(&self, host: &dyn ProviderHost) -> Result<Option<ProviderIdentity>, ProviderError> {
        let Some(executable) = host.resolve_executable(self.executable)? else {
            return Ok(None);
        };
        let version = if self.version_args.is_empty() {
            None
        } else {
            let command = plain_inspection_command(&executable, self.version_args);
            successful_stdout(host, &command)
                .ok()
                .and_then(|output| output.lines().next().map(str::trim).map(str::to_owned))
        };
        Ok(Some(ProviderIdentity {
            id: self.id.into(),
            executable: Some(executable),
            version,
            context: None,
        }))
    }

    fn inspect(
        &self,
        identity: &ProviderIdentity,
        selected_ids: &HashSet<String>,
        host: &dyn ProviderHost,
    ) -> Result<Vec<DetectedTarget>, ProviderError> {
        let executable = identity
            .executable
            .as_ref()
            .ok_or_else(|| ProviderError::new("provider executable identity is missing"))?;
        Ok(self
            .actions
            .iter()
            .filter(|action| selected_ids.contains(action.id))
            .map(|action| inspect_simple_action(host, identity, executable, action))
            .collect())
    }
}

fn inspect_simple_action(
    host: &dyn ProviderHost,
    identity: &ProviderIdentity,
    executable: &Path,
    action: &SimpleAction,
) -> DetectedTarget {
    let mut evidence = Vec::new();
    let mut safety = CandidateSafety::Eligible;
    let mut reported_bytes = None;
    let mut item_identity = identity.clone();
    if !action.location_args.is_empty() {
        match host.run_inspection(&plain_inspection_command(executable, action.location_args)) {
            Ok(output) if output.exit_code == Some(0) => {
                let location = concise_output(&output.stdout);
                if !location.is_empty() {
                    item_identity.context = Some(location.clone());
                    evidence.push(format!("configured location/context: {location}"));
                }
            }
            Ok(output) => safety = CandidateSafety::Refused(command_failure(&output)),
            Err(error) => safety = CandidateSafety::Refused(error.to_string()),
        }
    }
    if matches!(safety, CandidateSafety::Eligible) && !action.inspect_args.is_empty() {
        match host.run_inspection(&plain_inspection_command(executable, action.inspect_args)) {
            Ok(output) if output.exit_code == Some(0) => {
                let summary = concise_output(&output.stdout);
                reported_bytes = output.stdout.trim().parse::<u64>().ok();
                if !summary.is_empty() {
                    evidence.push(format!("provider inspection: {summary}"));
                }
            }
            Ok(output) => safety = CandidateSafety::Refused(command_failure(&output)),
            Err(error) => safety = CandidateSafety::Refused(error.to_string()),
        }
    }

    let operation = if action.informational {
        PlannedOperation::Informational
    } else {
        PlannedOperation::Command {
            command: CommandSpec {
                executable: executable.to_path_buf(),
                args: action.plan_args.iter().map(|arg| (*arg).into()).collect(),
                environment: BTreeMap::new(),
            },
        }
    };
    let usage = reported_bytes.map_or_else(
        || UsageEstimate::Unknown {
            explanation:
                "provider exposes the storage/action but no trustworthy read-only byte estimate"
                    .into(),
        },
        |bytes| UsageEstimate::ProviderReported {
            bytes: Some(bytes),
            confidence: Confidence::Low,
            explanation: "bytes reported by the provider inspection command".into(),
        },
    );
    provider_item(
        action.id,
        &item_identity,
        action.description,
        action.class,
        action.level,
        usage,
        safety,
        operation,
        action.exclusive_group,
        action.priority,
        action.consequence,
        evidence,
    )
}

fn plain_inspection_command(executable: &Path, args: &[&str]) -> CommandSpec {
    CommandSpec {
        executable: executable.to_path_buf(),
        args: args.iter().map(|arg| (*arg).into()).collect(),
        environment: BTreeMap::new(),
    }
}

fn concise_output(output: &str) -> String {
    const MAX: usize = 320;
    let joined = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(4)
        .collect::<Vec<_>>()
        .join("; ");
    if joined.chars().count() <= MAX {
        joined
    } else {
        format!("{}…", joined.chars().take(MAX).collect::<String>())
    }
}

const NIX_IDS: [&str; 2] = ["nix.gc", "nix.generations"];
const PNPM_IDS: [&str; 1] = ["pnpm.store-prune"];
const NPM_IDS: [&str; 2] = ["npm.cache-verify", "npm.npx-cache"];
const YARN_IDS: [&str; 1] = ["yarn.cache-clean"];
const BUN_IDS: [&str; 1] = ["bun.cache-clean"];
const UV_IDS: [&str; 2] = ["uv.cache-prune", "uv.cache-clean"];
const PIP_IDS: [&str; 1] = ["pip.cache-purge"];
const POETRY_IDS: [&str; 2] = ["poetry.cache-clean", "poetry.environments"];
const CONDA_IDS: [&str; 2] = ["conda.cache-clean", "conda.environments"];
const RUSTUP_IDS: [&str; 1] = ["rustup.toolchains"];
const CARGO_IDS: [&str; 1] = ["cargo.shared-cache"];
const ORBSTACK_IDS: [&str; 1] = ["orbstack.machines"];
const XCODE_IDS: [&str; 5] = [
    "xcode.derived-data",
    "xcode.archives",
    "xcode.device-support",
    "xcode.simulator-data",
    "xcode.simulators-unavailable",
];
const COCOAPODS_IDS: [&str; 1] = ["cocoapods.cache-clean"];

const NIX_ACTIONS: [SimpleAction; 2] = [
    SimpleAction {
        id: "nix.gc",
        description: "collect unreachable Nix store paths",
        class: TargetClass::Network,
        level: CleanupLevel::Standard,
        location_args: &[],
        inspect_args: &[],
        plan_args: &[],
        consequence: "unreachable store paths are removed and may need to be downloaded or rebuilt",
        informational: false,
        exclusive_group: Some("nix.gc.mode"),
        priority: 10,
    },
    SimpleAction {
        id: "nix.generations",
        description: "delete old Nix generations and collect garbage",
        class: TargetClass::Expensive,
        level: CleanupLevel::Aggressive,
        location_args: &[],
        inspect_args: &[],
        plan_args: &["-d"],
        consequence: "old generations and rollback capability are removed before garbage collection",
        informational: false,
        exclusive_group: Some("nix.gc.mode"),
        priority: 20,
    },
];
const PNPM_ACTIONS: [SimpleAction; 1] = [SimpleAction {
    id: "pnpm.store-prune",
    description: "prune unreferenced packages from the pnpm store",
    class: TargetClass::Network,
    level: CleanupLevel::Standard,
    location_args: &["store", "path"],
    inspect_args: &[],
    plan_args: &["store", "prune"],
    consequence: "unreferenced packages are removed and later installs may download them again",
    informational: false,
    exclusive_group: None,
    priority: 0,
}];
const NPM_ACTIONS: [SimpleAction; 2] = [
    SimpleAction {
        id: "npm.cache-verify",
        description: "verify and garbage-collect npm's content-addressed cache",
        class: TargetClass::Network,
        level: CleanupLevel::Standard,
        location_args: &["config", "get", "cache"],
        inspect_args: &[],
        plan_args: &["cache", "verify"],
        consequence: "unneeded cache entries are removed and corrupt entries may be redownloaded",
        informational: false,
        exclusive_group: None,
        priority: 0,
    },
    SimpleAction {
        id: "npm.npx-cache",
        description: "inventory npm/npx execution cache",
        class: TargetClass::Network,
        level: CleanupLevel::Standard,
        location_args: &["config", "get", "cache"],
        inspect_args: &[],
        plan_args: &[],
        consequence: "the installed npm exposes no stable separate npx cleanup capability; entries remain informational",
        informational: true,
        exclusive_group: None,
        priority: 0,
    },
];
const YARN_ACTIONS: [SimpleAction; 1] = [SimpleAction {
    id: "yarn.cache-clean",
    description: "clear Yarn's shared package cache",
    class: TargetClass::Network,
    level: CleanupLevel::Standard,
    location_args: &[],
    inspect_args: &[],
    plan_args: &["cache", "clean"],
    consequence: "cached packages are removed and must be downloaded by later installs",
    informational: false,
    exclusive_group: None,
    priority: 0,
}];
const BUN_ACTIONS: [SimpleAction; 1] = [SimpleAction {
    id: "bun.cache-clean",
    description: "clear Bun's package cache",
    class: TargetClass::Network,
    level: CleanupLevel::Standard,
    location_args: &["pm", "cache"],
    inspect_args: &[],
    plan_args: &["pm", "cache", "rm"],
    consequence: "cached packages are removed; clone sharing can make host reclamation differ",
    informational: false,
    exclusive_group: None,
    priority: 0,
}];
const UV_ACTIONS: [SimpleAction; 2] = [
    SimpleAction {
        id: "uv.cache-prune",
        description: "prune dangling uv cache entries and cached environments",
        class: TargetClass::Network,
        level: CleanupLevel::Standard,
        location_args: &["cache", "dir", "--no-config"],
        inspect_args: &["cache", "size", "--no-config"],
        plan_args: &["cache", "prune", "--no-config"],
        consequence: "dangling cache entries are removed and may be downloaded again",
        informational: false,
        exclusive_group: Some("uv.cache.mode"),
        priority: 10,
    },
    SimpleAction {
        id: "uv.cache-clean",
        description: "clear the complete uv cache",
        class: TargetClass::Expensive,
        level: CleanupLevel::Aggressive,
        location_args: &["cache", "dir", "--no-config"],
        inspect_args: &["cache", "size", "--no-config"],
        plan_args: &["cache", "clean", "--no-config"],
        consequence: "all cached packages and environments are removed and must be restored",
        informational: false,
        exclusive_group: Some("uv.cache.mode"),
        priority: 20,
    },
];
const PIP_ACTIONS: [SimpleAction; 1] = [SimpleAction {
    id: "pip.cache-purge",
    description: "purge pip's wheel and HTTP caches",
    class: TargetClass::Network,
    level: CleanupLevel::Standard,
    location_args: &["cache", "dir", "--isolated"],
    inspect_args: &["cache", "info", "--isolated"],
    plan_args: &["cache", "purge", "--isolated", "--no-input"],
    consequence: "cached Python distributions are removed and later installs require downloads",
    informational: false,
    exclusive_group: None,
    priority: 0,
}];
const POETRY_ACTIONS: [SimpleAction; 2] = [
    SimpleAction {
        id: "poetry.cache-clean",
        description: "clear Poetry's PyPI package cache",
        class: TargetClass::Network,
        level: CleanupLevel::Standard,
        location_args: &["config", "cache-dir"],
        inspect_args: &[],
        plan_args: &["cache", "clear", "pypi", "--all"],
        consequence: "cached Python packages are removed and later installs require downloads",
        informational: false,
        exclusive_group: None,
        priority: 0,
    },
    SimpleAction {
        id: "poetry.environments",
        description: "inventory Poetry virtual environments",
        class: TargetClass::Irreplaceable,
        level: CleanupLevel::Aggressive,
        location_args: &["config", "virtualenvs.path"],
        inspect_args: &["env", "list", "--full-path"],
        plan_args: &[],
        consequence: "virtual environments may contain workflow state and remain informational until individually named",
        informational: true,
        exclusive_group: None,
        priority: 0,
    },
];
const CONDA_ACTIONS: [SimpleAction; 2] = [
    SimpleAction {
        id: "conda.cache-clean",
        description: "remove Conda index, tarball, log, and unused package caches",
        class: TargetClass::Network,
        level: CleanupLevel::Standard,
        location_args: &["info", "--base"],
        inspect_args: &["clean", "--all", "--dry-run", "--json"],
        plan_args: &["clean", "--all", "--yes"],
        consequence: "unused cached packages and metadata are removed; environments remain intact",
        informational: false,
        exclusive_group: None,
        priority: 0,
    },
    SimpleAction {
        id: "conda.environments",
        description: "inventory Conda environments",
        class: TargetClass::Irreplaceable,
        level: CleanupLevel::Aggressive,
        location_args: &["info", "--base"],
        inspect_args: &["env", "list", "--json"],
        plan_args: &[],
        consequence: "Conda environments are protected state and remain informational until an individual environment is named",
        informational: true,
        exclusive_group: None,
        priority: 0,
    },
];
const RUSTUP_ACTIONS: [SimpleAction; 1] = [SimpleAction {
    id: "rustup.toolchains",
    description: "inventory installed Rust toolchains",
    class: TargetClass::Irreplaceable,
    level: CleanupLevel::Aggressive,
    location_args: &[],
    inspect_args: &["toolchain", "list"],
    plan_args: &[],
    consequence: "toolchains are only reported until an individual unused toolchain is explicitly selected",
    informational: true,
    exclusive_group: None,
    priority: 0,
}];
const CARGO_ACTIONS: [SimpleAction; 1] = [SimpleAction {
    id: "cargo.shared-cache",
    description: "inventory Cargo registry and Git caches",
    class: TargetClass::Network,
    level: CleanupLevel::Standard,
    location_args: &[],
    inspect_args: &[],
    plan_args: &[],
    consequence: "Cargo has no stable built-in whole-cache prune operation; storage is informational",
    informational: true,
    exclusive_group: None,
    priority: 0,
}];
const ORBSTACK_ACTIONS: [SimpleAction; 1] = [SimpleAction {
    id: "orbstack.machines",
    description: "inventory OrbStack Linux machines",
    class: TargetClass::Irreplaceable,
    level: CleanupLevel::Aggressive,
    location_args: &[],
    inspect_args: &["list"],
    plan_args: &[],
    consequence: "machine disks contain user data and remain explicit-only informational findings",
    informational: true,
    exclusive_group: None,
    priority: 0,
}];
const XCODE_ACTIONS: [SimpleAction; 1] = [SimpleAction {
    id: "xcode.simulators-unavailable",
    description: "delete simulator devices unavailable to the active Xcode SDK",
    class: TargetClass::Cheap,
    level: CleanupLevel::Standard,
    location_args: &[],
    inspect_args: &["simctl", "list", "devices", "unavailable", "--json"],
    plan_args: &["simctl", "delete", "unavailable"],
    consequence: "unavailable simulator device data is permanently removed",
    informational: false,
    exclusive_group: None,
    priority: 0,
}];
const COCOAPODS_ACTIONS: [SimpleAction; 1] = [SimpleAction {
    id: "cocoapods.cache-clean",
    description: "clear CocoaPods' downloaded pod cache",
    class: TargetClass::Network,
    level: CleanupLevel::Standard,
    location_args: &["cache", "list"],
    inspect_args: &[],
    plan_args: &["cache", "clean", "--all"],
    consequence: "cached pods are removed and later installs require downloads",
    informational: false,
    exclusive_group: None,
    priority: 0,
}];

static NIX_PROVIDER: SimpleProvider = SimpleProvider {
    id: "nix",
    executable: "nix-collect-garbage",
    version_args: &[],
    actions: &NIX_ACTIONS,
};
static PNPM_PROVIDER: SimpleProvider = SimpleProvider {
    id: "pnpm",
    executable: "pnpm",
    version_args: &["--version"],
    actions: &PNPM_ACTIONS,
};
static NPM_PROVIDER: SimpleProvider = SimpleProvider {
    id: "npm",
    executable: "npm",
    version_args: &["--version"],
    actions: &NPM_ACTIONS,
};
// Corepack shims may download Yarn merely to answer --version, so detection is
// intentionally executable-only and never invokes Yarn until an action is run.
static YARN_PROVIDER: SimpleProvider = SimpleProvider {
    id: "yarn",
    executable: "yarn",
    version_args: &[],
    actions: &YARN_ACTIONS,
};
static BUN_PROVIDER: SimpleProvider = SimpleProvider {
    id: "bun",
    executable: "bun",
    version_args: &["--version"],
    actions: &BUN_ACTIONS,
};
static UV_PROVIDER: SimpleProvider = SimpleProvider {
    id: "uv",
    executable: "uv",
    version_args: &["--version"],
    actions: &UV_ACTIONS,
};
static PIP_PROVIDER: SimpleProvider = SimpleProvider {
    id: "pip",
    executable: "pip3",
    version_args: &["--version"],
    actions: &PIP_ACTIONS,
};
static POETRY_PROVIDER: SimpleProvider = SimpleProvider {
    id: "poetry",
    executable: "poetry",
    version_args: &["--version"],
    actions: &POETRY_ACTIONS,
};
static CONDA_PROVIDER: SimpleProvider = SimpleProvider {
    id: "conda",
    executable: "conda",
    version_args: &["--version"],
    actions: &CONDA_ACTIONS,
};
static RUSTUP_PROVIDER: SimpleProvider = SimpleProvider {
    id: "rustup",
    executable: "rustup",
    version_args: &["--version"],
    actions: &RUSTUP_ACTIONS,
};
static CARGO_PROVIDER: SimpleProvider = SimpleProvider {
    id: "cargo",
    executable: "cargo",
    version_args: &["--version"],
    actions: &CARGO_ACTIONS,
};
static ORBSTACK_PROVIDER: SimpleProvider = SimpleProvider {
    id: "orbstack",
    executable: "orbctl",
    version_args: &["version"],
    actions: &ORBSTACK_ACTIONS,
};
static COCOAPODS_PROVIDER: SimpleProvider = SimpleProvider {
    id: "cocoapods",
    executable: "pod",
    version_args: &["--version"],
    actions: &COCOAPODS_ACTIONS,
};

const DOCKER_IDS: [&str; 4] = [
    "docker.build-cache",
    "docker.stopped-containers",
    "docker.images",
    "docker.volumes",
];

struct DockerProvider;

impl Provider for DockerProvider {
    fn id(&self) -> &'static str {
        "docker"
    }

    fn action_ids(&self) -> &'static [&'static str] {
        &DOCKER_IDS
    }

    fn detect(&self, host: &dyn ProviderHost) -> Result<Option<ProviderIdentity>, ProviderError> {
        let Some(executable) = host.resolve_executable("docker")? else {
            return Ok(None);
        };
        let version = successful_stdout(
            host,
            &plain_inspection_command(&executable, &["version", "--format", "{{.Client.Version}}"]),
        )
        .ok()
        .map(|value| value.trim().to_owned());
        Ok(Some(ProviderIdentity {
            id: "docker".into(),
            executable: Some(executable),
            version,
            context: None,
        }))
    }

    fn inspect(
        &self,
        identity: &ProviderIdentity,
        selected_ids: &HashSet<String>,
        host: &dyn ProviderHost,
    ) -> Result<Vec<DetectedTarget>, ProviderError> {
        let executable = identity
            .executable
            .as_ref()
            .ok_or_else(|| ProviderError::new("Docker executable identity is missing"))?;
        let contexts = successful_stdout(
            host,
            &plain_inspection_command(executable, &["context", "ls", "--format", "{{json .}}"]),
        )?;
        let mut items = Vec::new();
        for line in contexts.lines().filter(|line| !line.trim().is_empty()) {
            let value: serde_json::Value = serde_json::from_str(line).map_err(|error| {
                ProviderError::new(format!("invalid Docker context JSON: {error}"))
            })?;
            let name = value["Name"].as_str().unwrap_or_default();
            let endpoint = value["DockerEndpoint"].as_str().unwrap_or_default();
            let context_error = value["Error"].as_str().unwrap_or_default();
            if name.is_empty() || !context_error.is_empty() || !endpoint.starts_with("unix://") {
                continue;
            }
            let inspection = plain_inspection_command(
                executable,
                &["--context", name, "system", "df", "--format", "json"],
            );
            let output = host.run_inspection(&inspection);
            let usage = match &output {
                Ok(output) if output.exit_code == Some(0) => {
                    parse_docker_usage(&output.stdout).unwrap_or_default()
                }
                _ => BTreeMap::new(),
            };
            for action in docker_actions() {
                if !selected_ids.contains(action.id) {
                    continue;
                }
                let mut context_identity = identity.clone();
                context_identity.context = Some(format!("{name} ({endpoint})"));
                let safety = match &output {
                    Ok(output) if output.exit_code == Some(0) => CandidateSafety::Eligible,
                    Ok(output) => CandidateSafety::Refused(command_failure(output)),
                    Err(error) => CandidateSafety::Refused(error.to_string()),
                };
                let bytes = usage.get(action.usage_type).copied();
                let estimate = if bytes.is_some() {
                    UsageEstimate::ProviderReported {
                        bytes,
                        confidence: Confidence::Low,
                        explanation: "Docker engine reclaimable bytes; host free-space change can differ on VM-backed runtimes".into(),
                    }
                } else {
                    UsageEstimate::Unknown {
                        explanation: "Docker context did not report a trustworthy reclaimable size"
                            .into(),
                    }
                };
                let args = std::iter::once("--context".to_owned())
                    .chain(std::iter::once(name.to_owned()))
                    .chain(action.args.iter().map(|arg| (*arg).to_owned()))
                    .collect();
                let operation = if action.informational {
                    PlannedOperation::Informational
                } else {
                    PlannedOperation::Command {
                        command: CommandSpec {
                            executable: executable.clone(),
                            args,
                            environment: BTreeMap::new(),
                        },
                    }
                };
                items.push(provider_item(
                    action.id,
                    &context_identity,
                    action.description,
                    action.class,
                    action.level,
                    estimate,
                    safety,
                    operation,
                    None,
                    0,
                    action.consequence,
                    vec![format!(
                        "pinned Docker context and endpoint: {name} ({endpoint})"
                    )],
                ));
            }
        }
        Ok(items)
    }
}

struct DockerAction {
    id: &'static str,
    usage_type: &'static str,
    description: &'static str,
    class: TargetClass,
    level: CleanupLevel,
    args: &'static [&'static str],
    consequence: &'static str,
    informational: bool,
}

fn docker_actions() -> [DockerAction; 4] {
    [
        DockerAction {
            id: "docker.build-cache",
            usage_type: "Build Cache",
            description: "prune Docker build cache unused for 14 days",
            class: TargetClass::Network,
            level: CleanupLevel::Standard,
            args: &["builder", "prune", "--force", "--filter", "until=336h"],
            consequence: "old build cache is removed and later builds may rebuild or download layers",
            informational: false,
        },
        DockerAction {
            id: "docker.stopped-containers",
            usage_type: "Containers",
            description: "remove stopped Docker containers",
            class: TargetClass::Cheap,
            level: CleanupLevel::Standard,
            args: &["container", "prune", "--force", "--filter", "until=336h"],
            consequence: "stopped containers and their writable layers are permanently removed",
            informational: false,
        },
        DockerAction {
            id: "docker.images",
            usage_type: "Images",
            description: "remove unused Docker images older than 14 days",
            class: TargetClass::Expensive,
            level: CleanupLevel::Aggressive,
            args: &[
                "image",
                "prune",
                "--all",
                "--force",
                "--filter",
                "until=336h",
            ],
            consequence: "unused images are removed and may require large future downloads",
            informational: false,
        },
        DockerAction {
            id: "docker.volumes",
            usage_type: "Local Volumes",
            description: "inventory Docker volumes",
            class: TargetClass::Irreplaceable,
            level: CleanupLevel::Aggressive,
            args: &[],
            consequence: "named volume data is user data; generic volume pruning is never planned",
            informational: true,
        },
    ]
}

fn parse_docker_usage(output: &str) -> Option<BTreeMap<String, u64>> {
    let mut usage = BTreeMap::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        let kind = value["Type"].as_str()?;
        let reclaimable = value["Reclaimable"].as_str()?.split_whitespace().next()?;
        if let Some(bytes) = parse_human_bytes(reclaimable) {
            usage.insert(kind.to_owned(), bytes);
        }
    }
    Some(usage)
}

struct XcodeProvider;

impl Provider for XcodeProvider {
    fn id(&self) -> &'static str {
        "xcode"
    }

    fn action_ids(&self) -> &'static [&'static str] {
        &XCODE_IDS
    }

    fn detect(&self, host: &dyn ProviderHost) -> Result<Option<ProviderIdentity>, ProviderError> {
        let Some(executable) = host.resolve_executable("xcrun")? else {
            return Ok(None);
        };
        let xcode_select = host
            .resolve_executable("xcode-select")?
            .ok_or_else(|| ProviderError::new("xcode-select executable was not found"))?;
        let developer_directory =
            successful_stdout(host, &plain_inspection_command(&xcode_select, &["-p"]))?;
        Ok(Some(ProviderIdentity {
            id: "xcode".into(),
            executable: Some(executable),
            version: None,
            context: Some(format!(
                "developer directory {}",
                developer_directory.trim()
            )),
        }))
    }

    fn inspect(
        &self,
        identity: &ProviderIdentity,
        selected_ids: &HashSet<String>,
        host: &dyn ProviderHost,
    ) -> Result<Vec<DetectedTarget>, ProviderError> {
        let executable = identity
            .executable
            .as_ref()
            .ok_or_else(|| ProviderError::new("xcrun executable identity is missing"))?;
        let mut items = Vec::new();
        if selected_ids.contains("xcode.simulators-unavailable") {
            let action = &XCODE_ACTIONS[0];
            items.push(inspect_simple_action(host, identity, executable, action));
        }
        let Some(home) = env::var_os("HOME").map(PathBuf::from) else {
            return Ok(items);
        };
        let paths = [
            (
                "xcode.derived-data",
                home.join("Library/Developer/Xcode/DerivedData"),
                TargetClass::Cheap,
                CleanupLevel::Aggressive,
                true,
                "Xcode build products and indexes will be regenerated, potentially at high cost",
                false,
            ),
            (
                "xcode.archives",
                home.join("Library/Developer/Xcode/Archives"),
                TargetClass::Irreplaceable,
                CleanupLevel::Aggressive,
                false,
                "archives may be the only retained distribution artifacts and remain informational user data",
                true,
            ),
            (
                "xcode.device-support",
                home.join("Library/Developer/Xcode/iOS DeviceSupport"),
                TargetClass::Expensive,
                CleanupLevel::Aggressive,
                false,
                "device-support symbols may need to be downloaded or regenerated for debugging",
                true,
            ),
            (
                "xcode.simulator-data",
                home.join("Library/Developer/CoreSimulator/Devices"),
                TargetClass::Irreplaceable,
                CleanupLevel::Aggressive,
                false,
                "simulator applications and documents are user data and remain informational",
                true,
            ),
        ];
        for (id, path, class, level, children_only, consequence, informational) in paths {
            if selected_ids.contains(id) && path.exists() {
                let mut item = known_path_item(
                    id,
                    identity,
                    path,
                    class,
                    level,
                    children_only,
                    consequence,
                    informational,
                    &home,
                );
                if id == "xcode.derived-data" {
                    // The whole DerivedData root can be very costly to rebuild.
                    // It is plan-able only when its stable ID is explicitly named.
                    item.properties.rebuild_cost = Some(RebuildCost::High);
                    item.properties.automation = Automation::ExplicitTargetOnly;
                }
                items.push(item);
            }
        }
        Ok(items)
    }
}

#[allow(clippy::too_many_arguments)]
fn known_path_item(
    id: &str,
    identity: &ProviderIdentity,
    path: PathBuf,
    class: TargetClass,
    level: CleanupLevel,
    children_only: bool,
    consequence: &str,
    informational: bool,
    home: &Path,
) -> DetectedTarget {
    let metadata = std::fs::symlink_metadata(&path);
    let safety = validate_known_path(&path, home);
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    let operation = if informational {
        PlannedOperation::Informational
    } else if let Ok(metadata) = &metadata {
        PlannedOperation::DirectDelete {
            path: path.clone(),
            children_only,
            identity: CandidateIdentity {
                #[cfg(unix)]
                device: metadata.dev(),
                #[cfg(not(unix))]
                device: 0,
                #[cfg(unix)]
                inode: metadata.ino(),
                #[cfg(not(unix))]
                inode: 0,
                kind: if metadata.is_dir() {
                    NodeKind::Directory
                } else {
                    NodeKind::Other
                },
            },
        }
    } else {
        PlannedOperation::Informational
    };
    provider_item(
        id,
        identity,
        "known Xcode developer storage",
        class,
        level,
        UsageEstimate::Unknown {
            explanation: "known provider path; APFS-aware sizing requires inclusion in an explicit scan scope".into(),
        },
        safety,
        operation,
        None,
        0,
        consequence,
        vec![format!("detected known Xcode path: {}", path.display())],
    )
}

fn validate_known_path(path: &Path, home: &Path) -> CandidateSafety {
    if path == Path::new("/") || path == home || !path.starts_with(home) {
        return CandidateSafety::Refused(
            "known path escaped the protected home-relative scope".into(),
        );
    }
    let mut cursor = home.to_path_buf();
    let Ok(relative) = path.strip_prefix(home) else {
        return CandidateSafety::Refused("known path is outside the home directory".into());
    };
    for component in relative.components() {
        cursor.push(component);
        match std::fs::symlink_metadata(&cursor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return CandidateSafety::Refused(format!(
                    "symlinked ancestor refused: {}",
                    cursor.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return CandidateSafety::Refused(error.to_string()),
        }
    }
    CandidateSafety::Eligible
}

fn inspect_homebrew_cleanup(
    host: &dyn ProviderHost,
    identity: &ProviderIdentity,
    cache: &str,
    scrub: bool,
) -> DetectedTarget {
    let id = if scrub {
        "homebrew.cleanup.scrub"
    } else {
        "homebrew.cleanup"
    };
    let mut inspection_args = vec!["cleanup", "--dry-run"];
    if scrub {
        inspection_args.push("--scrub");
    }
    let executable = identity.executable.as_ref().expect("detected executable");
    let mut inspection = inspection_command(executable, &inspection_args);
    inspection
        .environment
        .insert("HOMEBREW_NO_AUTOREMOVE".into(), "1".into());
    let output = host.run_inspection(&inspection);
    let (usage, safety, mut evidence) = inspection_usage(output);
    evidence.push(format!("configured cache: {cache}"));

    let mut apply_args = vec!["cleanup".into()];
    if scrub {
        apply_args.push("--scrub".into());
    }
    let mut environment = provider_environment();
    // Keep autoremove a distinct reviewed action instead of letting cleanup
    // silently expand its scope at execution time.
    environment.insert("HOMEBREW_NO_AUTOREMOVE".into(), "1".into());
    provider_item(
        id,
        identity,
        if scrub {
            "scrub Homebrew caches and old installed versions"
        } else {
            "remove Homebrew's stale downloads and old installed versions"
        },
        if scrub {
            TargetClass::Expensive
        } else {
            TargetClass::Network
        },
        if scrub {
            CleanupLevel::Aggressive
        } else {
            CleanupLevel::Standard
        },
        usage,
        safety,
        PlannedOperation::Command {
            command: CommandSpec {
                executable: executable.clone(),
                args: apply_args,
                environment,
            },
        },
        Some("homebrew.cleanup.mode"),
        if scrub { 20 } else { 10 },
        if scrub {
            "removes a broader set of cached downloads; later installs may require substantial downloads"
        } else {
            "removes stale downloads and old installed versions; rollback artifacts and later downloads may be lost"
        },
        evidence,
    )
}

fn inspect_homebrew_autoremove(
    host: &dyn ProviderHost,
    identity: &ProviderIdentity,
) -> DetectedTarget {
    let executable = identity.executable.as_ref().expect("detected executable");
    let inspection = inspection_command(executable, &["autoremove", "--dry-run"]);
    let output = host.run_inspection(&inspection);
    let (usage, safety, evidence) = match output {
        Ok(output) if output.exit_code == Some(0) => {
            let count = parse_autoremove_count(&output.stdout).unwrap_or(0);
            (
                UsageEstimate::Unknown {
                    explanation: "Homebrew lists removable formulae but does not report their reclaimable bytes".into(),
                },
                CandidateSafety::Eligible,
                vec![format!("Homebrew reported {count} unneeded formulae")],
            )
        }
        Ok(output) => (
            UsageEstimate::Unknown {
                explanation: "Homebrew autoremove inspection failed".into(),
            },
            CandidateSafety::Refused(command_failure(&output)),
            output_evidence(&output),
        ),
        Err(error) => (
            UsageEstimate::Unknown {
                explanation: "Homebrew autoremove inspection failed".into(),
            },
            CandidateSafety::Refused(error.to_string()),
            Vec::new(),
        ),
    };
    provider_item(
        "homebrew.autoremove",
        identity,
        "uninstall formulae Homebrew considers unneeded dependencies",
        TargetClass::Network,
        CleanupLevel::Standard,
        usage,
        safety,
        PlannedOperation::Command {
            command: CommandSpec {
                executable: executable.clone(),
                args: vec!["autoremove".into()],
                environment: provider_environment(),
            },
        },
        None,
        0,
        "unneeded installed formulae are uninstalled and may need to be downloaded again",
        evidence,
    )
}

#[allow(clippy::too_many_arguments)]
fn provider_item(
    id: &str,
    identity: &ProviderIdentity,
    description: &str,
    class: TargetClass,
    minimum_level: CleanupLevel,
    usage: UsageEstimate,
    safety: CandidateSafety,
    operation: PlannedOperation,
    exclusive_group: Option<&str>,
    exclusive_priority: u8,
    consequence: &str,
    evidence: Vec<String>,
) -> DetectedTarget {
    DetectedTarget {
        cargo_ownership: None,
        gitignore: None,
        id: id.into(),
        provider: identity.clone(),
        description: description.into(),
        project_root: None,
        path: None,
        class,
        properties: class.properties(),
        minimum_level,
        usage,
        age_days: None,
        stale_after_days: 0,
        stale: Some(true),
        safety,
        operation,
        exclusive_group: exclusive_group.map(str::to_owned),
        exclusive_priority,
        consequence: consequence.into(),
        evidence,
    }
}

fn inspection_usage(
    output: Result<CommandOutput, ProviderError>,
) -> (UsageEstimate, CandidateSafety, Vec<String>) {
    match output {
        Ok(output) if output.exit_code == Some(0) => {
            let bytes = parse_homebrew_cleanup_bytes(&output.stdout).unwrap_or(0);
            let candidates = output
                .stdout
                .lines()
                .filter(|line| line.starts_with("Would remove:"))
                .count();
            (
                UsageEstimate::ProviderReported {
                    bytes: Some(bytes),
                    confidence: Confidence::Medium,
                    explanation: "approximate bytes reported by Homebrew's dry run; host free-space change may differ".into(),
                },
                CandidateSafety::Eligible,
                vec![format!("Homebrew dry run listed {candidates} removable paths")],
            )
        }
        Ok(output) => (
            UsageEstimate::Unknown {
                explanation: "Homebrew cleanup inspection failed".into(),
            },
            CandidateSafety::Refused(command_failure(&output)),
            output_evidence(&output),
        ),
        Err(error) => (
            UsageEstimate::Unknown {
                explanation: "Homebrew cleanup inspection failed".into(),
            },
            CandidateSafety::Refused(error.to_string()),
            Vec::new(),
        ),
    }
}

fn inspection_command(executable: &Path, args: &[&str]) -> CommandSpec {
    CommandSpec {
        executable: executable.to_path_buf(),
        args: args.iter().map(|value| (*value).into()).collect(),
        environment: provider_environment(),
    }
}

fn provider_environment() -> BTreeMap<String, String> {
    BTreeMap::from([("HOMEBREW_NO_AUTO_UPDATE".into(), "1".into())])
}

fn successful_stdout(
    host: &dyn ProviderHost,
    command: &CommandSpec,
) -> Result<String, ProviderError> {
    let output = host.run_inspection(command)?;
    if output.exit_code != Some(0) {
        return Err(ProviderError::new(command_failure(&output)));
    }
    Ok(output.stdout)
}

fn command_failure(output: &CommandOutput) -> String {
    let detail = output.stderr.trim();
    if detail.is_empty() {
        format!("inspection command exited with {:?}", output.exit_code)
    } else {
        format!(
            "inspection command exited with {:?}: {detail}",
            output.exit_code
        )
    }
}

fn output_evidence(output: &CommandOutput) -> Vec<String> {
    let stderr = output.stderr.trim();
    if stderr.is_empty() {
        Vec::new()
    } else {
        vec![format!("provider stderr: {stderr}")]
    }
}

fn action_enabled(id: &str, selected_ids: &HashSet<String>) -> bool {
    selected_ids.contains(id)
}

fn parse_homebrew_cleanup_bytes(output: &str) -> Option<u64> {
    let marker = "would free approximately ";
    let line = output
        .lines()
        .find(|line| line.to_ascii_lowercase().contains(marker))?;
    let lowercase = line.to_ascii_lowercase();
    let start = lowercase.find(marker)? + marker.len();
    parse_human_bytes(
        line[start..]
            .split_whitespace()
            .next()?
            .trim_end_matches('.'),
    )
}

fn parse_human_bytes(value: &str) -> Option<u64> {
    let split = value
        .find(|character: char| !(character.is_ascii_digit() || character == '.'))
        .unwrap_or(value.len());
    let number: f64 = value[..split].parse().ok()?;
    let unit = value[split..].trim().to_ascii_uppercase();
    let multiplier = match unit.as_str() {
        "" | "B" => 1_f64,
        "KB" => 1_000_f64,
        "MB" => 1_000_000_f64,
        "GB" => 1_000_000_000_f64,
        "TB" => 1_000_000_000_000_f64,
        "KIB" => 1024_f64,
        "MIB" => 1024_f64.powi(2),
        "GIB" => 1024_f64.powi(3),
        "TIB" => 1024_f64.powi(4),
        _ => return None,
    };
    (number.is_finite() && number >= 0.0).then_some((number * multiplier).round() as u64)
}

fn parse_autoremove_count(output: &str) -> Option<usize> {
    let line = output
        .lines()
        .find(|line| line.contains("Would autoremove"))?;
    line.split_whitespace().find_map(|word| word.parse().ok())
}

fn resolve_from_path(name: &str) -> Result<Option<PathBuf>, ProviderError> {
    let candidate = Path::new(name);
    if candidate.components().count() > 1 {
        return Ok(candidate.is_file().then(|| candidate.to_path_buf()));
    }
    let Some(path) = env::var_os("PATH") else {
        return Ok(None);
    };
    for directory in env::split_paths(&path) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            // Preserve the executable entry point rather than canonicalizing
            // its final symlink. Multi-call tools such as Docker Desktop select
            // behavior from argv[0] and break if invoked by the backing name.
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn run_bounded(
    specification: &CommandSpec,
    timeout: Duration,
    max_output_bytes: usize,
    poll_interval: Duration,
) -> Result<CommandOutput, ProviderError> {
    let mut command = Command::new(&specification.executable);
    if specification.environment.contains_key("GIT_NO_LAZY_FETCH") {
        // Ambient Git variables must not redirect inspection to another repo,
        // inject config, or cause writes through an alternate index.
        for (name, _) in env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
    }
    command
        .args(&specification.args)
        .envs(&specification.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(provider_io_error)?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let stdout_reader = thread::spawn(move || read_limited(stdout, max_output_bytes));
    let stderr_reader = thread::spawn(move || read_limited(stderr, max_output_bytes));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(provider_io_error)? {
            break status;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ProviderError::new(format!(
                "inspection command timed out after {} seconds",
                timeout.as_secs()
            )));
        }
        thread::sleep(poll_interval);
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| ProviderError::new("stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| ProviderError::new("stderr reader panicked"))??;
    Ok(CommandOutput {
        exit_code: status.code(),
        stdout,
        stderr,
    })
}

fn read_limited(mut file: impl Read, limit: usize) -> Result<String, ProviderError> {
    let mut bytes = Vec::new();
    file.by_ref()
        .take(u64::try_from(limit).unwrap_or(u64::MAX) + 1)
        .read_to_end(&mut bytes)
        .map_err(provider_io_error)?;
    if bytes.len() > limit {
        return Err(ProviderError::new(format!(
            "inspection output exceeded {limit} bytes"
        )));
    }
    String::from_utf8(bytes).map_err(|_| ProviderError::new("inspection output was not UTF-8"))
}

fn provider_io_error(error: io::Error) -> ProviderError {
    ProviderError::new(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{Arena, Node, ScanResult};
    use std::sync::Mutex;

    struct FixtureHost {
        commands: Mutex<Vec<Vec<String>>>,
    }

    impl FixtureHost {
        fn new() -> Self {
            Self {
                commands: Mutex::new(Vec::new()),
            }
        }
    }

    impl ProviderHost for FixtureHost {
        fn resolve_executable(&self, name: &str) -> Result<Option<PathBuf>, ProviderError> {
            Ok((name == "brew").then(|| PathBuf::from("/fixture/bin/brew")))
        }

        fn run_inspection(&self, command: &CommandSpec) -> Result<CommandOutput, ProviderError> {
            self.commands.lock().unwrap().push(command.args.clone());
            let stdout = match command
                .args
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .as_slice()
            {
                ["--version"] => "Homebrew 6.0.18\n",
                ["--prefix"] => "/opt/homebrew\n",
                ["--cache"] => "/Users/test/Library/Caches/Homebrew\n",
                ["cleanup", "--dry-run"] => {
                    "Would remove: /cache/a (1.2GB)\n==> This operation would free approximately 1.2GB of disk space.\n"
                }
                ["cleanup", "--dry-run", "--scrub"] => {
                    "Would remove: /cache/a (1.2GB)\nWould remove: /cache/b (800MB)\n==> This operation would free approximately 2GB of disk space.\n"
                }
                ["autoremove", "--dry-run"] => "Would autoremove 2 unneeded formulae:\na\nb\n",
                other => panic!("unexpected command: {other:?}"),
            };
            Ok(CommandOutput {
                exit_code: Some(0),
                stdout: stdout.into(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn homebrew_actions_are_separate_and_exact() {
        let host = FixtureHost::new();
        let selected = HOMEBREW_ACTION_IDS
            .iter()
            .map(|id| (*id).to_owned())
            .collect();
        let inventory = inspect_builtin_providers(&host, &selected);
        assert_eq!(inventory.items.len(), 3);
        let cleanup = inventory
            .items
            .iter()
            .find(|item| item.id == "homebrew.cleanup")
            .unwrap();
        assert_eq!(cleanup.minimum_level, CleanupLevel::Standard);
        assert!(matches!(
            cleanup.usage,
            UsageEstimate::ProviderReported {
                bytes: Some(1_200_000_000),
                ..
            }
        ));
        let PlannedOperation::Command { command } = &cleanup.operation else {
            panic!("cleanup must be a command")
        };
        assert_eq!(command.args, ["cleanup"]);
        assert_eq!(command.environment["HOMEBREW_NO_AUTOREMOVE"], "1");
    }

    #[test]
    fn action_filter_avoids_unrequested_inspections() {
        let host = FixtureHost::new();
        let selected = HashSet::from(["homebrew.autoremove".into()]);
        let inventory = inspect_builtin_providers(&host, &selected);
        assert_eq!(inventory.items.len(), 1);
        assert_eq!(inventory.items[0].id, "homebrew.autoremove");
        let commands = host.commands.lock().unwrap();
        assert!(
            !commands
                .iter()
                .any(|args| args.first().is_some_and(|arg| arg == "cleanup"))
        );
    }

    #[test]
    fn parser_accepts_homebrew_decimal_units() {
        assert_eq!(
            parse_homebrew_cleanup_bytes(
                "==> This operation would free approximately 3.9GB of disk space."
            ),
            Some(3_900_000_000)
        );
    }

    #[test]
    fn docker_usage_parser_keeps_risk_classes_separate() {
        let usage = parse_docker_usage(
            r#"{"Type":"Images","Reclaimable":"1.2GB (20%)"}
{"Type":"Local Volumes","Reclaimable":"3.5GB (90%)"}
{"Type":"Build Cache","Reclaimable":"800MB"}"#,
        )
        .unwrap();
        assert_eq!(usage["Images"], 1_200_000_000);
        assert_eq!(usage["Local Volumes"], 3_500_000_000);
        assert_eq!(usage["Build Cache"], 800_000_000);
        let volumes = docker_actions()
            .into_iter()
            .find(|action| action.id == "docker.volumes")
            .unwrap();
        assert_eq!(volumes.class, TargetClass::Irreplaceable);
    }

    struct SharedCargoFixtureHost {
        target: PathBuf,
    }
    impl ProviderHost for SharedCargoFixtureHost {
        fn resolve_executable(&self, name: &str) -> Result<Option<PathBuf>, ProviderError> {
            Ok((name == "cargo").then(|| PathBuf::from("/fixture/cargo")))
        }
        fn run_inspection(&self, command: &CommandSpec) -> Result<CommandOutput, ProviderError> {
            let stdout = if command.args == ["--version"] {
                "cargo fixture".into()
            } else {
                let manifest = Path::new(command.args.last().unwrap());
                serde_json::json!({"workspace_root": manifest.parent().unwrap(), "target_directory": self.target}).to_string()
            };
            Ok(CommandOutput {
                exit_code: Some(0),
                stdout,
                stderr: String::new(),
            })
        }
    }
    #[test]
    fn shared_cargo_ownership_is_structured_counted_once_and_cannot_disrupt_active_owner() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        for name in ["a", "b"] {
            std::fs::create_dir(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("Cargo.toml"), "fixture").unwrap();
        }
        let target = root.join("a/target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("output"), "fixture").unwrap();
        let mut scan = crate::Scanner::new(Default::default()).scan(&root).unwrap();
        for node in &mut scan.arena.nodes {
            node.modified_unix_seconds = 1;
        }
        let active_manifest = (0..scan.arena.nodes.len())
            .map(NodeIndex::from_usize)
            .find(|i| scan.arena.path(*i) == root.join("b/Cargo.toml"))
            .unwrap();
        scan.arena.nodes[active_manifest.as_usize()].modified_unix_seconds = now_seconds() as i64;
        let host = SharedCargoFixtureHost {
            target: target.clone(),
        };
        let mut inventory = inspect_cargo_builds(&scan, &host, None, 14);
        assert_eq!(inventory.items.len(), 1);
        let ownership = inventory.items[0].cargo_ownership.as_ref().unwrap();
        assert_eq!(ownership.observed_owners, [root.join("a"), root.join("b")]);
        assert!(!ownership.complete);
        assert_eq!(
            inventory.items[0].stale,
            Some(false),
            "the newest owner manifest must count"
        );
        let projects = crate::projects::inspect_projects(
            &scan,
            &mut inventory.items,
            &[],
            &host,
            Default::default(),
        );
        assert!(!inventory.items[0].manually_selectable());
        assert_eq!(
            projects
                .projects
                .iter()
                .map(|p| p.artifacts.len())
                .sum::<usize>(),
            1
        );
        assert!(
            projects
                .projects
                .iter()
                .any(|p| p.root == root.join("b") && p.shared_artifacts == [target.clone()])
        );
        assert!(revalidate_cargo_build(&inventory.items[0], &host).is_err());
    }

    #[test]
    fn cargo_revalidation_refuses_changed_target_resolution() {
        let root = PathBuf::from("/fixture/workspace");
        let mut arena = Arena::new(&root);
        arena.push_child(
            arena.root(),
            "Cargo.toml",
            Node {
                kind: NodeKind::File,
                modified_unix_seconds: 1,
                ..Default::default()
            },
        );
        arena.push_child(
            arena.root(),
            "target",
            Node {
                kind: NodeKind::Directory,
                modified_unix_seconds: 1,
                ..Default::default()
            },
        );
        let scan = ScanResult {
            root: root.clone(),
            arena,
            coverage_errors: vec![],
            backend: "fixture".into(),
            dataless_entries: 0,
            mount_boundaries: 0,
        };
        let host = CargoFixtureHost {
            root: root.clone(),
            target: root.join("target"),
        };
        let inventory = inspect_cargo_builds(&scan, &host, None, 14);
        assert!(revalidate_cargo_build(&inventory.items[0], &host).is_ok());
        let changed = CargoFixtureHost {
            root: root.clone(),
            target: root.join("custom-target"),
        };
        assert!(revalidate_cargo_build(&inventory.items[0], &changed).is_err());
    }

    struct CargoFixtureHost {
        root: PathBuf,
        target: PathBuf,
    }

    impl ProviderHost for CargoFixtureHost {
        fn resolve_executable(&self, name: &str) -> Result<Option<PathBuf>, ProviderError> {
            Ok((name == "cargo").then(|| PathBuf::from("/fixture/bin/cargo")))
        }

        fn run_inspection(&self, command: &CommandSpec) -> Result<CommandOutput, ProviderError> {
            let stdout = if command.args == ["--version"] {
                "cargo fixture\n".into()
            } else {
                serde_json::json!({
                    "workspace_root": self.root,
                    "target_directory": self.target
                })
                .to_string()
            };
            Ok(CommandOutput {
                exit_code: Some(0),
                stdout,
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn cargo_workspace_members_collapse_to_one_resolved_target() {
        let root = PathBuf::from("/fixture/workspace");
        let mut arena = Arena::new(&root);
        let arena_root = arena.root();
        arena.push_child(
            arena_root,
            "Cargo.toml",
            Node {
                kind: NodeKind::File,
                modified_unix_seconds: 1,
                ..Node::default()
            },
        );
        let member = arena.push_child(
            arena_root,
            "member",
            Node {
                kind: NodeKind::Directory,
                ..Node::default()
            },
        );
        arena.push_child(
            member,
            "Cargo.toml",
            Node {
                kind: NodeKind::File,
                modified_unix_seconds: 1,
                ..Node::default()
            },
        );
        let target = arena.push_child(
            arena_root,
            "target",
            Node {
                kind: NodeKind::Directory,
                device: 1,
                inode: 20,
                modified_unix_seconds: 1,
                ..Node::default()
            },
        );
        arena.push_child(
            target,
            "artifact",
            Node {
                kind: NodeKind::File,
                device: 1,
                inode: 21,
                allocated_size: 4096,
                private_size: Some(4096),
                modified_unix_seconds: 1,
                ..Node::default()
            },
        );
        let scan = ScanResult {
            root: root.clone(),
            arena,
            coverage_errors: Vec::new(),
            dataless_entries: 0,
            mount_boundaries: 0,
            backend: "fixture".into(),
        };
        let inventory = inspect_cargo_builds(
            &scan,
            &CargoFixtureHost {
                root: root.clone(),
                target: root.join("target"),
            },
            None,
            9,
        );
        assert_eq!(inventory.items.len(), 1);
        assert_eq!(inventory.items[0].stale_after_days, 9);
        assert_eq!(
            inventory.items[0].path.as_deref(),
            Some(root.join("target").as_path())
        );
        let PlannedOperation::Command { command } = &inventory.items[0].operation else {
            panic!("Cargo cleanup must use its native command")
        };
        assert!(command.args.contains(&"--target-dir".into()));
    }

    #[test]
    fn cargo_custom_target_is_resolved_but_refused_without_complete_ownership() {
        let root = PathBuf::from("/fixture/workspace");
        let custom = root.join("shared-build");
        let mut arena = Arena::new(&root);
        let arena_root = arena.root();
        let now = i64::try_from(now_seconds()).unwrap();
        arena.push_child(
            arena_root,
            "Cargo.toml",
            Node {
                kind: NodeKind::File,
                modified_unix_seconds: now,
                ..Node::default()
            },
        );
        arena.push_child(
            arena_root,
            "shared-build",
            Node {
                kind: NodeKind::Directory,
                device: 1,
                inode: 30,
                modified_unix_seconds: now,
                ..Node::default()
            },
        );
        let scan = ScanResult {
            root: root.clone(),
            arena,
            coverage_errors: Vec::new(),
            dataless_entries: 0,
            mount_boundaries: 0,
            backend: "fixture".into(),
        };
        let inventory = inspect_cargo_builds(
            &scan,
            &CargoFixtureHost {
                root,
                target: custom,
            },
            None,
            14,
        );
        assert_eq!(inventory.items.len(), 1);
        assert_eq!(inventory.items[0].stale, Some(false));
        assert!(matches!(
            &inventory.items[0].safety,
            CandidateSafety::Refused(reason) if reason.contains("ownership")
        ));
    }

    #[test]
    fn cargo_target_outside_the_explicit_scan_is_visible_but_refused() {
        let root = PathBuf::from("/fixture/workspace");
        let mut arena = Arena::new(&root);
        let arena_root = arena.root();
        arena.push_child(
            arena_root,
            "Cargo.toml",
            Node {
                kind: NodeKind::File,
                modified_unix_seconds: 1,
                ..Node::default()
            },
        );
        let scan = ScanResult {
            root: root.clone(),
            arena,
            coverage_errors: Vec::new(),
            dataless_entries: 0,
            mount_boundaries: 0,
            backend: "fixture".into(),
        };
        let inventory = inspect_cargo_builds(
            &scan,
            &CargoFixtureHost {
                root,
                target: PathBuf::from("/fixture/shared-target"),
            },
            None,
            14,
        );

        assert!(matches!(
            &inventory.items[0].safety,
            CandidateSafety::Refused(reason) if reason.contains("outside the explicit scan scope")
        ));
        assert!(matches!(
            inventory.items[0].usage,
            UsageEstimate::Unknown { .. }
        ));
    }

    struct PermissiveProviderHost;

    impl ProviderHost for PermissiveProviderHost {
        fn resolve_executable(&self, name: &str) -> Result<Option<PathBuf>, ProviderError> {
            Ok(Some(PathBuf::from(format!("/fixture/bin/{name}"))))
        }

        fn run_inspection(&self, command: &CommandSpec) -> Result<CommandOutput, ProviderError> {
            let stdout = if command.args.iter().any(|arg| arg == "--version")
                || command.args.iter().any(|arg| arg == "version")
            {
                "fixture 1.0\n"
            } else {
                "/fixture/provider-storage\n"
            };
            Ok(CommandOutput {
                exit_code: Some(0),
                stdout: stdout.into(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn every_declarative_provider_has_a_fixture_inspection_and_exact_plan() {
        let selected: HashSet<String> = [
            &NIX_IDS[..],
            &PNPM_IDS,
            &NPM_IDS,
            &YARN_IDS,
            &BUN_IDS,
            &UV_IDS,
            &PIP_IDS,
            &POETRY_IDS,
            &CONDA_IDS,
            &RUSTUP_IDS,
            &CARGO_IDS,
            &ORBSTACK_IDS,
            &XCODE_IDS[4..],
            &COCOAPODS_IDS,
        ]
        .into_iter()
        .flatten()
        .map(|id| (*id).to_owned())
        .collect();
        let inventory = inspect_builtin_providers(&PermissiveProviderHost, &selected);
        let found: HashSet<_> = inventory
            .items
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        for id in &selected {
            assert!(found.contains(id.as_str()), "missing fixture action {id}");
        }
        for item in inventory.items {
            if let PlannedOperation::Command { command } = item.operation {
                assert!(
                    !command.executable.as_os_str().is_empty(),
                    "{} has no executable",
                    item.id
                );
            }
        }
    }
}
