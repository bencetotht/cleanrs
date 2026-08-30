//! Trusted target-registry loading. Configuration is read only from the user's
//! config directory or an explicitly named path, never from the working tree.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use cleanrs_core::fs::{NodeIndex, ScanResult};
use cleanrs_core::providers::ProviderHost;
use cleanrs_core::targets::{
    Automation, CandidateSafety, CleanupLevel, CommandSpec, DetectedTarget, PlannedOperation,
    ProviderIdentity, RebuildCost, Recoverability, TargetClass, UsageEstimate,
    detect_explicit_path_target,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

enum CommandTrust<'a> {
    ImplicitUserConfig,
    ExplicitHash(Option<&'a str>),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegistryFile {
    #[serde(default)]
    pub enable: Vec<String>,
    #[serde(default)]
    pub disable: Vec<String>,
    #[serde(default, rename = "target")]
    pub targets: Vec<ConfiguredTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfiguredTarget {
    pub id: String,
    #[serde(default)]
    pub description: String,
    pub detect: DetectConfig,
    pub class: TargetClass,
    pub reclaim: ReclaimConfig,
    #[serde(default)]
    pub stale_after: Option<String>,
    #[serde(default)]
    pub minimum_level: Option<CleanupLevel>,
    #[serde(default)]
    pub scope: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DetectConfig {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub marker: Option<String>,
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReclaimConfig {
    pub method: String,
    #[serde(default)]
    pub children_only: bool,
    #[serde(default)]
    pub argv: Vec<String>,
}

#[derive(Debug, Default)]
pub(crate) struct ResolvedRegistry {
    pub enabled: BTreeSet<String>,
    pub disabled: BTreeSet<String>,
    pub targets: BTreeMap<String, ConfiguredTarget>,
    pub sources: Vec<PathBuf>,
}

impl ResolvedRegistry {
    pub(crate) fn load(
        home: Option<&Path>,
        explicit: Option<&Path>,
        trusted_explicit_hash: Option<&str>,
    ) -> Result<Self> {
        let mut resolved = Self::default();
        if let Some(home) = home {
            let user = home.join(".config/cleanrs/targets.toml");
            if user.is_file() {
                resolved.merge_file(&user, CommandTrust::ImplicitUserConfig)?;
            }
        }
        if let Some(explicit) = explicit {
            resolved.merge_file(explicit, CommandTrust::ExplicitHash(trusted_explicit_hash))?;
        }
        Ok(resolved)
    }

    pub(crate) fn add_cli_targets(
        &mut self,
        path_specs: &[String],
        argv_specs: &[String],
    ) -> Result<()> {
        for (index, spec) in path_specs.iter().enumerate() {
            let parts: Vec<_> = spec.rsplitn(3, ':').collect();
            let (path, class, method) = match parts.as_slice() {
                [class, path] => (*path, parse_class(class)?, "delete"),
                [method, class, path] if matches!(*method, "delete" | "delete-children") => {
                    (*path, parse_class(class)?, *method)
                }
                _ => {
                    bail!("invalid --target {spec:?}; expected PATH:CLASS[:delete|delete-children]")
                }
            };
            let id = format!("cli.path.{index}");
            self.targets.insert(
                id.clone(),
                ConfiguredTarget {
                    id,
                    description: format!("ad-hoc path target {path}"),
                    detect: DetectConfig {
                        path: Some(path.into()),
                        marker: None,
                        dir: None,
                        command: None,
                    },
                    class,
                    reclaim: ReclaimConfig {
                        method: method.into(),
                        children_only: method == "delete-children",
                        argv: Vec::new(),
                    },
                    stale_after: Some("0d".into()),
                    minimum_level: None,
                    scope: Vec::new(),
                },
            );
        }
        for (index, spec) in argv_specs.iter().enumerate() {
            let argv: Vec<String> = serde_json::from_str(spec)
                .with_context(|| format!("invalid JSON argv passed to --target-argv: {spec}"))?;
            let Some(executable) = argv.first().cloned() else {
                bail!("--target-argv must contain at least an executable");
            };
            let id = format!("cli.command.{index}");
            self.targets.insert(
                id.clone(),
                ConfiguredTarget {
                    id,
                    description: "ad-hoc argv command target".into(),
                    detect: DetectConfig {
                        path: None,
                        marker: None,
                        dir: None,
                        command: Some(executable),
                    },
                    class: TargetClass::Cheap,
                    reclaim: ReclaimConfig {
                        method: "run".into(),
                        children_only: false,
                        argv,
                    },
                    stale_after: None,
                    minimum_level: None,
                    scope: Vec::new(),
                },
            );
        }
        Ok(())
    }

    fn merge_file(&mut self, path: &Path, command_trust: CommandTrust<'_>) -> Result<()> {
        let canonical = path
            .canonicalize()
            .with_context(|| format!("failed to resolve config {}", path.display()))?;
        let text = fs::read_to_string(&canonical)
            .with_context(|| format!("failed to read config {}", canonical.display()))?;
        let config: RegistryFile = toml::from_str(&text)
            .with_context(|| format!("failed to parse config {}", canonical.display()))?;
        let contains_commands = config
            .targets
            .iter()
            .any(|target| target.detect.command.is_some() || target.reclaim.method == "run");
        if contains_commands {
            if let CommandTrust::ExplicitHash(expected) = command_trust {
                let actual = hash_text(&text);
                if expected != Some(actual.as_str()) {
                    bail!(
                        "{} contains command targets and its current content is not trusted; review it and run `cleanrs trust-config {}`",
                        canonical.display(),
                        canonical.display()
                    );
                }
            }
        }
        for id in config.enable {
            self.disabled.remove(&id);
            self.enabled.insert(id);
        }
        for id in config.disable {
            self.enabled.remove(&id);
            self.disabled.insert(id);
        }
        for target in config.targets {
            validate_target(&target)?;
            self.targets.insert(target.id.clone(), target);
        }
        self.sources.push(canonical);
        Ok(())
    }

    pub(crate) fn permits(&self, id: &str) -> bool {
        self.enabled.contains(id) || !self.disabled.contains(id)
    }

    pub(crate) fn detect(
        &self,
        scan: &ScanResult,
        home: Option<&Path>,
        host: &dyn ProviderHost,
    ) -> Vec<DetectedTarget> {
        let mut items = Vec::new();
        for target in self
            .targets
            .values()
            .filter(|target| self.permits(&target.id))
        {
            if let Some(path) = &target.detect.path {
                let unresolved = expand_home(path, home);
                let symlink = symlinked_ancestor(&unresolved);
                let path = unresolved.canonicalize().unwrap_or(unresolved);
                let mut item = path_item(scan, target, path, home);
                if let Some(ancestor) = symlink {
                    item.safety = CandidateSafety::Refused(format!(
                        "configured path has symlinked ancestor: {}",
                        ancestor.display()
                    ));
                }
                items.push(item);
            } else if let (Some(marker), Some(directory)) =
                (&target.detect.marker, &target.detect.dir)
            {
                for project in (0..scan.arena.nodes.len()).map(NodeIndex::from_usize) {
                    let children: BTreeMap<_, _> = scan
                        .arena
                        .children(project)
                        .map(|child| (scan.arena.name(child), child))
                        .collect();
                    let (Some(_), Some(candidate)) = (
                        children.get(marker.as_str()),
                        children.get(directory.as_str()),
                    ) else {
                        continue;
                    };
                    let project_path = scan.arena.path(project);
                    if !target.scope.is_empty()
                        && !target
                            .scope
                            .iter()
                            .map(|scope| expand_home(scope, home))
                            .any(|scope| project_path.starts_with(scope))
                    {
                        continue;
                    }
                    items.push(path_item(scan, target, scan.arena.path(*candidate), home));
                }
            } else if let Some(command) = &target.detect.command {
                items.push(command_item(target, command, host));
            }
        }
        items
    }

    pub(crate) fn export(&self, builtin_ids: &[&str]) -> Result<String> {
        #[derive(Serialize)]
        struct Export<'a> {
            builtin: &'a [&'a str],
            sources: &'a [PathBuf],
            target: Vec<&'a ConfiguredTarget>,
        }
        toml::to_string_pretty(&Export {
            builtin: builtin_ids,
            sources: &self.sources,
            target: self.targets.values().collect(),
        })
        .context("failed to serialize resolved registry")
    }
}

fn hash_text(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    hex
}

fn validate_target(target: &ConfiguredTarget) -> Result<()> {
    if target.id.trim().is_empty() {
        bail!("target IDs cannot be empty");
    }
    if target
        .stale_after
        .as_deref()
        .is_some_and(|value| parse_days(value).is_none())
    {
        bail!(
            "target {:?} has invalid stale_after; expected whole days such as 14d",
            target.id
        );
    }
    let detector_count = [
        target.detect.path.is_some(),
        target.detect.command.is_some(),
        target.detect.marker.is_some() || target.detect.dir.is_some(),
    ]
    .into_iter()
    .filter(|value| *value)
    .count();
    if detector_count != 1 || target.detect.marker.is_some() != target.detect.dir.is_some() {
        bail!(
            "target {:?} must declare exactly one complete detector",
            target.id
        );
    }
    if target.detect.command.is_some() {
        let command = target.detect.command.as_deref().expect("command detector");
        if target.reclaim.method != "run"
            || target.reclaim.argv.first().map(String::as_str) != Some(command)
        {
            bail!(
                "target {:?} command detector requires run argv beginning with the exact detected command",
                target.id
            );
        }
    } else if !matches!(target.reclaim.method.as_str(), "delete" | "delete-children")
        || !target.reclaim.argv.is_empty()
    {
        bail!(
            "target {:?} path/marker detector requires delete or delete-children without argv",
            target.id
        );
    }
    Ok(())
}

fn path_item(
    scan: &ScanResult,
    target: &ConfiguredTarget,
    path: PathBuf,
    home: Option<&Path>,
) -> DetectedTarget {
    let stale = target
        .stale_after
        .as_deref()
        .and_then(parse_days)
        .unwrap_or(0);
    detect_explicit_path_target(
        scan,
        target.id.clone(),
        description(target),
        path,
        target.class,
        target
            .minimum_level
            .unwrap_or_else(|| default_level(target.class)),
        stale,
        target.reclaim.children_only || target.reclaim.method == "delete-children",
        home,
    )
}

fn command_item(
    target: &ConfiguredTarget,
    command: &str,
    host: &dyn ProviderHost,
) -> DetectedTarget {
    let resolved = host.resolve_executable(command);
    let (executable, safety) = match resolved {
        Ok(Some(path)) => (path, CandidateSafety::Eligible),
        Ok(None) => (
            PathBuf::from(command),
            CandidateSafety::Refused("required executable was not found on PATH".into()),
        ),
        Err(error) => (
            PathBuf::from(command),
            CandidateSafety::Refused(error.to_string()),
        ),
    };
    let mut argv = target.reclaim.argv.clone();
    if argv.first().is_some_and(|first| first == command) {
        argv.remove(0);
    }
    DetectedTarget {
        id: target.id.clone(),
        provider: ProviderIdentity {
            id: "configuration".into(),
            executable: Some(executable.clone()),
            version: None,
            context: Some("trusted configured command".into()),
        },
        description: description(target),
        project_root: None,
        path: None,
        class: target.class,
        properties: target.class.properties(),
        minimum_level: target
            .minimum_level
            .unwrap_or_else(|| default_level(target.class)),
        usage: UsageEstimate::Unknown {
            explanation: "configured command does not expose a read-only byte estimate".into(),
        },
        age_days: None,
        stale_after_days: 0,
        stale: Some(true),
        safety,
        operation: PlannedOperation::Command {
            command: CommandSpec {
                executable,
                args: argv,
                environment: BTreeMap::new(),
            },
        },
        exclusive_group: None,
        exclusive_priority: 0,
        consequence: "configured command consequence must be reviewed before execution".into(),
        evidence: vec!["loaded from a trusted registry source".into()],
    }
}

fn description(target: &ConfiguredTarget) -> String {
    if target.description.is_empty() {
        format!("configured target {}", target.id)
    } else {
        target.description.clone()
    }
}

fn expand_home(value: &str, home: Option<&Path>) -> PathBuf {
    if value == "~" {
        return home.unwrap_or_else(|| Path::new("~")).to_path_buf();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = home {
            return home.join(rest);
        }
    }
    PathBuf::from(value)
}

fn symlinked_ancestor(path: &Path) -> Option<PathBuf> {
    let mut cursor = PathBuf::new();
    for component in path.components() {
        cursor.push(component);
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Some(cursor),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    None
}

fn parse_days(value: &str) -> Option<u32> {
    value.strip_suffix('d')?.parse().ok()
}

fn parse_class(value: &str) -> Result<TargetClass> {
    match value {
        "free" => Ok(TargetClass::Free),
        "cheap" => Ok(TargetClass::Cheap),
        "network" => Ok(TargetClass::Network),
        "expensive" => Ok(TargetClass::Expensive),
        "irreplaceable" => Ok(TargetClass::Irreplaceable),
        _ => bail!("unknown target class {value:?}"),
    }
}

fn default_level(class: TargetClass) -> CleanupLevel {
    let properties = class.properties();
    if matches!(properties.rebuild_cost, Some(RebuildCost::High)) {
        CleanupLevel::Aggressive
    } else if properties.recoverability == Recoverability::Downloadable
        || matches!(properties.automation, Automation::Confirmed)
    {
        CleanupLevel::Standard
    } else {
        CleanupLevel::Safe
    }
}
