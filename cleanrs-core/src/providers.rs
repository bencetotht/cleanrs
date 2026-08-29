//! Read-only developer-tool detection, inspection, and cleanup planning.
//!
//! Phase 2 may execute tightly scoped inspection commands, but it never runs a
//! planned cleanup command. External commands are always an executable plus an
//! argv array; no shell is involved.

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
use crate::targets::{
    CandidateSafety, CleanupLevel, CommandSpec, DetectedTarget, PlannedOperation, ProviderIdentity,
    TargetClass, UsageEstimate,
};

/// Stable IDs exposed by the built-in Homebrew provider.
pub const HOMEBREW_ACTION_IDS: &[&str] = &[
    "homebrew.cleanup",
    "homebrew.cleanup.scrub",
    "homebrew.autoremove",
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
    max_output_bytes: usize,
}

impl Default for SystemProviderHost {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_output_bytes: 4 * 1024 * 1024,
        }
    }
}

impl ProviderHost for SystemProviderHost {
    fn resolve_executable(&self, name: &str) -> Result<Option<PathBuf>, ProviderError> {
        resolve_from_path(name)
    }

    fn run_inspection(&self, command: &CommandSpec) -> Result<CommandOutput, ProviderError> {
        run_bounded(command, self.timeout, self.max_output_bytes)
    }
}

/// A built-in provider supplies read-only detection and inspection. Execution
/// and verification deliberately remain Phase 3 capabilities.
pub trait Provider {
    fn id(&self) -> &'static str;

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
    let providers: [&dyn Provider; 1] = [&HomebrewProvider];
    let mut inventory = ProviderInventory::default();
    for provider in providers {
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

struct HomebrewProvider;

impl Provider for HomebrewProvider {
    fn id(&self) -> &'static str {
        "homebrew"
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
        return candidate
            .is_file()
            .then(|| candidate.canonicalize().map_err(provider_io_error))
            .transpose();
    }
    let Some(path) = env::var_os("PATH") else {
        return Ok(None);
    };
    for directory in env::split_paths(&path) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return candidate
                .canonicalize()
                .map(Some)
                .map_err(provider_io_error);
        }
    }
    Ok(None)
}

fn run_bounded(
    specification: &CommandSpec,
    timeout: Duration,
    max_output_bytes: usize,
) -> Result<CommandOutput, ProviderError> {
    let mut command = Command::new(&specification.executable);
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
        thread::sleep(Duration::from_millis(10));
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
}
