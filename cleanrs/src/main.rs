use std::cmp::Reverse;
use std::collections::HashSet;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand};
use cleanrs_core::accounting::{estimate_subtree, estimate_subtrees};
use cleanrs_core::execution::{
    ActionResult, ActionStatus, ExecutableIdentity, command_argv, executable_identity,
    execute_command, execute_direct_delete,
};
use cleanrs_core::fs::{Arena, NodeIndex, ScanResult, doctor};
use cleanrs_core::journal::{Journal, JournalAction, JournalRun, default_journal_path};
use cleanrs_core::providers::{
    BUILTIN_PROVIDER_ACTION_IDS, ProviderHost, ProviderStatus, SystemProviderHost,
    inspect_builtin_providers, inspect_cargo_builds,
};
use cleanrs_core::targets::{
    Automation, BUILTIN_TARGETS, CandidateIdentity, CleanupLevel, CommandSpec, DetectedTarget,
    PlanSelection, PlannedOperation, PlannedTarget, UsageEstimate, detect_workspace_targets,
    plan_cleanup,
};
use cleanrs_core::{ReclaimEstimate, ScanOptions, Scanner};
use serde::Serialize;
use tracing_subscriber::EnvFilter;

mod registry;
mod terminal;
mod trust;

use registry::ResolvedRegistry;
use terminal::{ActionProgressIndicator, ScanProgressIndicator, TerminalUi, stderr_is_terminal};
use trust::TrustStore;

#[derive(Debug, Parser)]
#[command(name = "cleanrs", version, about, propagate_version = true)]
struct Cli {
    /// Increase diagnostic logging (-v or -vv).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Scan an explicit directory without reading file contents.
    Scan(ScanArgs),
    /// Inspect container, volume, snapshot, swap, and access state.
    Doctor(DoctorArgs),
    /// Detect and size cleanup targets without executing anything.
    Targets(TargetArgs),
    /// Plan cleanup, or execute the same reviewed plan with --apply.
    Clean(CleanArgs),
    /// Show cleanup runs and their action-level outcomes.
    History(HistoryArgs),
    /// Persist trust for an explicitly named command configuration.
    TrustConfig(TrustConfigArgs),
    /// Generate shell completions on standard output.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Generate the cleanrs(1) manual page on standard output.
    Man,
}

#[derive(Debug, clap::Args)]
struct TargetArgs {
    /// Explicit workspace scope to inspect. Defaults to the working directory.
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Emit stable machine-readable output.
    #[arg(long)]
    json: bool,

    /// Include only these stable target IDs; may be supplied more than once.
    #[arg(long, value_name = "ID")]
    only: Vec<String>,

    /// Exclude these stable target IDs; may be supplied more than once.
    #[arg(long, value_name = "ID")]
    exclude: Vec<String>,

    /// Show at most N largest targets. Human output defaults to 25; JSON is unlimited.
    #[arg(long, value_name = "N")]
    top: Option<usize>,

    /// Hide targets whose reclaimable ceiling is below this size (for example 10MiB).
    #[arg(long, value_parser = parse_size, default_value = "0")]
    min_size: u64,

    /// Directory worker count (blocking workers; no async runtime).
    #[arg(long)]
    workers: Option<usize>,

    /// Inspect only workspace targets and skip machine-wide providers.
    #[arg(long)]
    workspace_only: bool,

    /// Cargo build inactivity threshold in whole days.
    #[arg(long, value_name = "DAYS", default_value_t = 14)]
    cargo_stale_after: u32,

    /// Load an explicitly named target registry (never discovered from cwd).
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Suppress all embedded workspace and provider targets.
    #[arg(long)]
    no_builtin_targets: bool,

    /// Persist trust for the explicit config's canonical path and current content hash.
    #[arg(long, requires = "config")]
    trust_config_commands: bool,

    /// Export the resolved registry as TOML without scanning.
    #[arg(long)]
    export: bool,

    /// Add an ad-hoc path target as PATH:CLASS[:delete|delete-children].
    #[arg(long, value_name = "SPEC")]
    target: Vec<String>,

    /// Add an ad-hoc command target as a JSON argv array (never a shell string).
    #[arg(long, value_name = "JSON")]
    target_argv: Vec<String>,

    /// Use only ad-hoc CLI targets; suppress built-ins and all config files.
    #[arg(long, conflicts_with = "config")]
    targets_only_cli: bool,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum CleanupLevelArg {
    Safe,
    Standard,
    Aggressive,
}

impl From<CleanupLevelArg> for CleanupLevel {
    fn from(value: CleanupLevelArg) -> Self {
        match value {
            CleanupLevelArg::Safe => Self::Safe,
            CleanupLevelArg::Standard => Self::Standard,
            CleanupLevelArg::Aggressive => Self::Aggressive,
        }
    }
}

#[derive(Debug, clap::Args)]
struct CleanArgs {
    /// Explicit workspace scope to inspect. Defaults to the working directory.
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Cleanup intensity. No level can implicitly select user data.
    #[arg(long, value_enum, default_value_t = CleanupLevelArg::Safe)]
    level: CleanupLevelArg,

    /// Execute the reviewed plan. Without this flag clean is always a dry run.
    #[arg(long)]
    apply: bool,

    /// Confirm unattended execution (only meaningful with --apply).
    #[arg(long, requires = "apply")]
    yes: bool,

    /// Reserved recovery mode; Trash execution is intentionally deferred.
    #[arg(long, requires = "apply")]
    trash: bool,

    /// Include only these stable target IDs; may be supplied more than once.
    #[arg(long, value_name = "ID")]
    only: Vec<String>,

    /// Exclude these stable target IDs; may be supplied more than once.
    #[arg(long, value_name = "ID")]
    exclude: Vec<String>,

    /// Plan eligible targets even when their age is below the staleness policy.
    #[arg(long)]
    ignore_staleness: bool,

    /// Emit stable machine-readable output.
    #[arg(long)]
    json: bool,

    /// Directory worker count (blocking workers; no async runtime).
    #[arg(long)]
    workers: Option<usize>,

    /// Plan only workspace targets and skip machine-wide providers.
    #[arg(long)]
    workspace_only: bool,

    /// Cargo build inactivity threshold in whole days.
    #[arg(long, value_name = "DAYS", default_value_t = 14)]
    cargo_stale_after: u32,

    /// Load an explicitly named target registry (never discovered from cwd).
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Suppress all embedded workspace and provider targets.
    #[arg(long)]
    no_builtin_targets: bool,

    /// Persist trust for the explicit config's canonical path and current content hash.
    #[arg(long, requires = "config")]
    trust_config_commands: bool,

    /// Add an ad-hoc path target as PATH:CLASS[:delete|delete-children].
    #[arg(long, value_name = "SPEC")]
    target: Vec<String>,

    /// Add an ad-hoc command target as a JSON argv array (never a shell string).
    #[arg(long, value_name = "JSON")]
    target_argv: Vec<String>,

    /// Use only ad-hoc CLI targets; suppress built-ins and all config files.
    #[arg(long, conflicts_with = "config")]
    targets_only_cli: bool,
}

#[derive(Debug, clap::Args)]
struct HistoryArgs {
    /// Emit stable machine-readable output.
    #[arg(long)]
    json: bool,

    /// Show at most this many newest runs.
    #[arg(long, default_value_t = 20)]
    limit: usize,
}

#[derive(Debug, clap::Args)]
struct TrustConfigArgs {
    /// Configuration file to trust or revoke.
    path: Option<PathBuf>,

    /// List persistent trust records.
    #[arg(long, conflicts_with = "revoke")]
    list: bool,

    /// Revoke trust for PATH.
    #[arg(long, requires = "path")]
    revoke: bool,

    /// Emit stable machine-readable output.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
struct DoctorArgs {
    /// Emit stable machine-readable output.
    #[arg(long)]
    json: bool,

    /// Reconcile one explicit scan scope against the Data volume.
    #[arg(long, value_name = "PATH")]
    scan: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
struct ScanArgs {
    /// Directory to scan. Defaults to the working directory, never the home directory.
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Emit stable machine-readable output.
    #[arg(long)]
    json: bool,

    /// Rank and display logical file length.
    #[arg(long, conflicts_with_all = ["allocated", "private", "reclaimable"])]
    apparent: bool,

    /// Rank and display filesystem allocated bytes (the default).
    #[arg(long, conflicts_with_all = ["apparent", "private", "reclaimable"])]
    allocated: bool,

    /// Display the immediately-private lower bound.
    #[arg(long, conflicts_with_all = ["apparent", "allocated", "reclaimable"])]
    private: bool,

    /// Display the reclaimability interval and derived confidence.
    #[arg(long, conflicts_with_all = ["apparent", "allocated", "private"])]
    reclaimable: bool,

    /// Maximum displayed tree depth. The complete subtree is still scanned.
    #[arg(long, default_value_t = 1)]
    depth: usize,

    /// Hide entries smaller than this value (for example 10MiB or 2GB).
    #[arg(long, value_parser = parse_size, default_value = "0")]
    min_size: u64,

    /// Exclude a glob; may be supplied more than once.
    #[arg(long)]
    exclude: Vec<String>,

    /// Explain the estimate for one path within the scan.
    #[arg(long)]
    explain: Option<PathBuf>,

    /// Traverse mount boundaries. Disabled by default.
    #[arg(long)]
    cross_mounts: bool,

    /// Directory worker count (blocking workers; no async runtime).
    #[arg(long)]
    workers: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Metric {
    Apparent,
    Allocated,
    Private,
    Reclaimable,
}

#[derive(Debug, Serialize)]
pub(crate) struct ScanOutput {
    root: PathBuf,
    metric: &'static str,
    backend: String,
    coverage_complete: bool,
    coverage_errors: Vec<cleanrs_core::fs::CoverageError>,
    dataless_entries: usize,
    mount_boundaries: usize,
    entries: Vec<OutputEntry>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OutputEntry {
    path: PathBuf,
    depth: usize,
    apparent: u64,
    allocated: u64,
    estimate: ReclaimEstimate,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    match cli.command {
        Commands::Scan(args) => run_scan(args),
        Commands::Doctor(args) => run_doctor(args),
        Commands::Targets(args) => run_targets(args),
        Commands::Clean(args) => run_clean(args),
        Commands::History(args) => run_history(args),
        Commands::TrustConfig(args) => run_trust_config(args),
        Commands::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "cleanrs", &mut io::stdout());
            Ok(())
        }
        Commands::Man => {
            clap_mangen::Man::new(Cli::command()).render(&mut io::stdout())?;
            Ok(())
        }
    }
}

#[derive(Debug, Serialize)]
struct TargetsOutput {
    scope: PathBuf,
    coverage_complete: bool,
    total_detected: usize,
    hidden: usize,
    default_limit_applied: bool,
    providers: Vec<ProviderStatus>,
    targets: Vec<DetectedTarget>,
}

#[derive(Debug, Serialize)]
pub(crate) struct CleanOutput {
    pub(crate) scope: PathBuf,
    pub(crate) level: CleanupLevel,
    pub(crate) coverage_complete: bool,
    pub(crate) execution: String,
    pub(crate) selected_targets: usize,
    pub(crate) selected_estimate: ReclaimEstimate,
    pub(crate) selected_provider_reported: u64,
    pub(crate) selected_provider_actions: usize,
    pub(crate) selected_unknown_actions: usize,
    pub(crate) providers: Vec<ProviderStatus>,
    pub(crate) targets: Vec<PlannedTarget>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) results: Vec<AppliedActionOutput>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AppliedActionOutput {
    pub(crate) target_id: String,
    pub(crate) result: ActionResult,
}

/// The owned handoff shared by rendering, confirmation, execution, and history.
/// No action is rediscovered or widened after this value is constructed.
#[derive(Debug, Clone, Serialize)]
struct ReviewedPlan {
    run_id: String,
    created_unix_seconds: u64,
    scope: PathBuf,
    level: CleanupLevel,
    explicitly_named_ids: Vec<String>,
    targets: Vec<PlannedTarget>,
    selected_estimate: ReclaimEstimate,
    selected_provider_reported: u64,
    actions: Vec<ReviewedAction>,
}

#[derive(Debug, Clone, Serialize)]
struct ReviewedAction {
    target: DetectedTarget,
    executable_identity: Option<ExecutableIdentity>,
    executable_identity_error: Option<String>,
}

fn run_targets(args: TargetArgs) -> Result<()> {
    if args.top == Some(0) {
        bail!("--top must be at least 1");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let trusted_hash = resolve_explicit_config_trust(
        home.as_deref(),
        args.config.as_deref(),
        args.trust_config_commands,
    )?;
    let mut registry = if args.targets_only_cli {
        ResolvedRegistry::default()
    } else {
        ResolvedRegistry::load(
            home.as_deref(),
            args.config.as_deref(),
            trusted_hash.as_deref(),
        )?
    };
    registry.add_cli_targets(&args.target, &args.target_argv)?;
    if args.export {
        print!(
            "{}",
            registry.export(
                &BUILTIN_TARGETS
                    .iter()
                    .map(|definition| definition.id)
                    .chain(BUILTIN_PROVIDER_ACTION_IDS.iter().copied())
                    .collect::<Vec<_>>()
            )?
        );
        return Ok(());
    }
    let definitions: Vec<_> = BUILTIN_TARGETS
        .iter()
        .copied()
        .filter(|_| !args.no_builtin_targets && !args.targets_only_cli)
        .filter(|definition| registry.permits(definition.id))
        .filter(|definition| !registry.targets.contains_key(definition.id))
        .filter(|definition| args.only.is_empty() || args.only.iter().any(|id| id == definition.id))
        .filter(|definition| !args.exclude.iter().any(|id| id == definition.id))
        .map(|mut definition| {
            if definition.id == "cargo.target" {
                definition.stale_after_days = args.cargo_stale_after;
            }
            definition
        })
        .collect();
    validate_target_ids(&args.only, "--only", &registry)?;
    validate_target_ids(&args.exclude, "--exclude", &registry)?;

    let mut options = ScanOptions::default();
    if let Some(workers) = args.workers {
        if workers == 0 {
            bail!("--workers must be at least 1");
        }
        options.workers = workers;
    }
    let scanner = Scanner::new(options);
    let progress = ScanProgressIndicator::create(
        &format!("targets in {}", args.path.display()),
        !args.json && stderr_is_terminal(),
    );
    let scan = if let Some(indicator) = &progress {
        scanner.scan_with_observer(&args.path, indicator.clone())
    } else {
        scanner.scan(&args.path)
    };
    if let Some(indicator) = &progress {
        indicator.finish();
    }
    let scan =
        scan.with_context(|| format!("failed to inspect targets in {}", args.path.display()))?;
    let mut targets = detect_workspace_targets(&scan, &definitions, home.as_deref());
    targets.extend(
        registry
            .detect(&scan, home.as_deref(), &SystemProviderHost::default())
            .into_iter()
            .filter(|item| args.only.is_empty() || args.only.contains(&item.id))
            .filter(|item| !args.exclude.contains(&item.id)),
    );
    let provider_ids = if args.no_builtin_targets || args.targets_only_cli {
        HashSet::new()
    } else {
        selected_provider_ids(&args.only, &args.exclude, &registry)
    };
    let mut provider_inventory = if args.workspace_only || provider_ids.is_empty() {
        Default::default()
    } else {
        inspect_builtin_providers(&SystemProviderHost::default(), &provider_ids)
    };
    if !args.no_builtin_targets
        && !args.targets_only_cli
        && registry.permits("cargo.target")
        && !registry.targets.contains_key("cargo.target")
        && (args.only.is_empty() || args.only.iter().any(|id| id == "cargo.target"))
    {
        let cargo_inventory = inspect_cargo_builds(
            &scan,
            &SystemProviderHost::default(),
            home.as_deref(),
            args.cargo_stale_after,
        );
        let resolved_paths: HashSet<_> = cargo_inventory
            .items
            .iter()
            .filter_map(|item| item.path.clone())
            .collect();
        let resolved_projects: HashSet<_> = cargo_inventory
            .items
            .iter()
            .filter_map(|item| item.project_root.clone())
            .collect();
        targets.retain(|item| {
            item.id != "cargo.target"
                || !resolved_paths.contains(item.path.as_ref().expect("workspace target path"))
                    && !resolved_projects.contains(
                        item.project_root
                            .as_ref()
                            .expect("workspace target project root"),
                    )
        });
        targets.extend(cargo_inventory.items);
        provider_inventory.statuses.extend(cargo_inventory.statuses);
    }
    targets.extend(provider_inventory.items);
    targets = dedupe_inventory_paths(targets);
    targets.sort_by(|left, right| {
        std::cmp::Reverse(target_sort_size(left))
            .cmp(&std::cmp::Reverse(target_sort_size(right)))
            .then_with(|| left.path.cmp(&right.path))
    });
    let total_detected = targets.len();
    targets.retain(|target| target_sort_size(target) >= args.min_size);
    let default_limit = !args.json && args.top.is_none();
    let default_limit_applied = default_limit && targets.len() > 25;
    let limit = args.top.or(default_limit.then_some(25));
    if let Some(limit) = limit {
        targets.truncate(limit);
    }
    let output = TargetsOutput {
        scope: scan.root.clone(),
        coverage_complete: scan.coverage_complete(),
        total_detected,
        hidden: total_detected.saturating_sub(targets.len()),
        default_limit_applied,
        providers: provider_inventory.statuses,
        targets,
    };
    if args.json {
        serde_json::to_writer_pretty(io::stdout().lock(), &output)?;
        println!();
    } else {
        TerminalUi::detect().print_targets(&output);
    }
    Ok(())
}

fn run_clean(args: CleanArgs) -> Result<()> {
    if args.trash {
        bail!(
            "--trash is intentionally deferred: moving data does not reclaim space; omit --trash to use guarded permanent cleanup"
        );
    }
    if args.apply && args.json && !args.yes {
        bail!(
            "--json --apply requires --yes because interactive prompts would corrupt JSON output"
        );
    }
    if args.workers == Some(0) {
        bail!("--workers must be at least 1");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let trusted_hash = resolve_explicit_config_trust(
        home.as_deref(),
        args.config.as_deref(),
        args.trust_config_commands,
    )?;
    let mut registry = if args.targets_only_cli {
        ResolvedRegistry::default()
    } else {
        ResolvedRegistry::load(
            home.as_deref(),
            args.config.as_deref(),
            trusted_hash.as_deref(),
        )?
    };
    registry.add_cli_targets(&args.target, &args.target_argv)?;
    validate_target_ids(&args.only, "--only", &registry)?;
    validate_target_ids(&args.exclude, "--exclude", &registry)?;
    let definitions: Vec<_> = BUILTIN_TARGETS
        .iter()
        .copied()
        .filter(|_| !args.no_builtin_targets && !args.targets_only_cli)
        .filter(|definition| registry.permits(definition.id))
        .filter(|definition| !registry.targets.contains_key(definition.id))
        .filter(|definition| args.only.is_empty() || args.only.iter().any(|id| id == definition.id))
        .filter(|definition| !args.exclude.iter().any(|id| id == definition.id))
        .map(|mut definition| {
            if definition.id == "cargo.target" {
                definition.stale_after_days = args.cargo_stale_after;
            }
            definition
        })
        .collect();

    let mut options = ScanOptions::default();
    if let Some(workers) = args.workers {
        options.workers = workers;
    }
    let scanner = Scanner::new(options);
    let progress = ScanProgressIndicator::create(
        &format!("cleanup candidates in {}", args.path.display()),
        !args.json && stderr_is_terminal(),
    );
    let scan = if let Some(indicator) = &progress {
        scanner.scan_with_observer(&args.path, indicator.clone())
    } else {
        scanner.scan(&args.path)
    };
    if let Some(indicator) = &progress {
        indicator.finish();
    }
    let scan =
        scan.with_context(|| format!("failed to build cleanup plan in {}", args.path.display()))?;
    let mut detected = detect_workspace_targets(&scan, &definitions, home.as_deref());
    detected.extend(
        registry
            .detect(&scan, home.as_deref(), &SystemProviderHost::default())
            .into_iter()
            .filter(|item| args.only.is_empty() || args.only.contains(&item.id))
            .filter(|item| !args.exclude.contains(&item.id)),
    );
    let provider_ids = if args.no_builtin_targets || args.targets_only_cli {
        HashSet::new()
    } else {
        selected_provider_ids(&args.only, &args.exclude, &registry)
    };
    let mut provider_inventory = if args.workspace_only || provider_ids.is_empty() {
        Default::default()
    } else {
        inspect_builtin_providers(&SystemProviderHost::default(), &provider_ids)
    };
    if !args.no_builtin_targets
        && !args.targets_only_cli
        && registry.permits("cargo.target")
        && !registry.targets.contains_key("cargo.target")
        && (args.only.is_empty() || args.only.iter().any(|id| id == "cargo.target"))
    {
        let cargo_inventory = inspect_cargo_builds(
            &scan,
            &SystemProviderHost::default(),
            home.as_deref(),
            args.cargo_stale_after,
        );
        let resolved_paths: HashSet<_> = cargo_inventory
            .items
            .iter()
            .filter_map(|item| item.path.clone())
            .collect();
        let resolved_projects: HashSet<_> = cargo_inventory
            .items
            .iter()
            .filter_map(|item| item.project_root.clone())
            .collect();
        detected.retain(|item| {
            item.id != "cargo.target"
                || !resolved_paths.contains(item.path.as_ref().expect("workspace target path"))
                    && !resolved_projects.contains(
                        item.project_root
                            .as_ref()
                            .expect("workspace target project root"),
                    )
        });
        detected.extend(cargo_inventory.items);
        provider_inventory.statuses.extend(cargo_inventory.statuses);
    }
    detected.extend(provider_inventory.items);
    detected = dedupe_inventory_paths(detected);
    let level = CleanupLevel::from(args.level);
    let mut targets = plan_cleanup(detected, level, &args.only, args.ignore_staleness);
    targets.sort_by(|left, right| {
        let left_selected = matches!(left.selection, PlanSelection::Selected);
        let right_selected = matches!(right.selection, PlanSelection::Selected);
        Reverse((left_selected, target_sort_size(&left.target)))
            .cmp(&Reverse((right_selected, target_sort_size(&right.target))))
            .then_with(|| left.target.path.cmp(&right.target.path))
    });

    let selected: Vec<_> = targets
        .iter()
        .filter(|target| matches!(target.selection, PlanSelection::Selected))
        .collect();
    let selected_roots: Vec<_> = selected
        .iter()
        .filter_map(|planned| {
            if !matches!(planned.target.usage, UsageEstimate::Filesystem { .. }) {
                return None;
            }
            let index = (0..scan.arena.nodes.len())
                .map(NodeIndex::from_usize)
                .find(|index| {
                    planned
                        .target
                        .path
                        .as_ref()
                        .is_some_and(|path| scan.arena.path(*index) == *path)
                })
                .expect("direct-delete target path must remain in its source arena");
            Some(index)
        })
        .collect();
    let selected_estimate =
        estimate_subtrees(&scan.arena, &selected_roots, scan.coverage_complete());
    let selected_provider_reported = selected
        .iter()
        .filter_map(|planned| match planned.target.usage {
            UsageEstimate::ProviderReported { bytes, .. } => bytes,
            _ => None,
        })
        .fold(0_u64, u64::saturating_add);
    let selected_provider_actions = selected
        .iter()
        .filter(|planned| matches!(planned.target.usage, UsageEstimate::ProviderReported { .. }))
        .count();
    let selected_unknown_actions = selected
        .iter()
        .filter(|planned| matches!(planned.target.usage, UsageEstimate::Unknown { .. }))
        .count();
    let actions = selected
        .iter()
        .map(|planned| {
            let (identity, error) = match &planned.target.operation {
                PlannedOperation::Command { command } => {
                    match executable_identity(&command.executable) {
                        Ok(identity) => (Some(identity), None),
                        Err(error) => (None, Some(error.to_string())),
                    }
                }
                _ => (None, None),
            };
            ReviewedAction {
                target: planned.target.clone(),
                executable_identity: identity,
                executable_identity_error: error,
            }
        })
        .collect();
    let selected_count = selected.len();
    let plan = ReviewedPlan {
        run_id: new_run_id(),
        created_unix_seconds: now_seconds(),
        scope: scan.root.clone(),
        level,
        explicitly_named_ids: args.only.clone(),
        targets,
        selected_estimate: selected_estimate.clone(),
        selected_provider_reported,
        actions,
    };
    let mut output = CleanOutput {
        scope: scan.root.clone(),
        level,
        coverage_complete: scan.coverage_complete(),
        execution: "dry-run; no action was run".into(),
        selected_targets: selected_count,
        selected_estimate,
        selected_provider_reported,
        selected_provider_actions,
        selected_unknown_actions,
        providers: provider_inventory.statuses,
        targets: plan.targets.clone(),
        results: Vec::new(),
    };
    if !args.apply {
        if args.json {
            serde_json::to_writer_pretty(io::stdout().lock(), &output)?;
            println!();
        } else {
            TerminalUi::detect().print_clean(&output);
        }
        return Ok(());
    }

    enforce_apply_boundary(&plan, args.yes)?;
    if !args.json {
        output.execution = "awaiting confirmation; no action has run".into();
        TerminalUi::detect().print_clean(&output);
    }
    if !args.yes && !confirm_apply(&plan)? {
        bail!("cleanup cancelled; no action was run");
    }

    let journal = journal_for_environment(home.as_deref())?;
    let (results, run) = apply_reviewed_plan(&plan, home.as_deref(), args.json, &journal)?;
    output.execution = run.outcome.clone();
    output.results = results;
    if args.json {
        serde_json::to_writer_pretty(io::stdout().lock(), &output)?;
        println!();
    } else {
        TerminalUi::detect().print_apply_results(&output);
    }
    if run.outcome == "success" {
        Ok(())
    } else {
        bail!("cleanup completed with outcome: {}", run.outcome)
    }
}

fn enforce_apply_boundary(plan: &ReviewedPlan, unattended: bool) -> Result<()> {
    #[cfg(unix)]
    if unsafe { libc::geteuid() } == 0 {
        bail!("cleanrs refuses destructive execution as root");
    }
    if plan.actions.is_empty() {
        bail!("cleanup plan selects no executable actions");
    }
    if unattended {
        if plan.actions.len() > 100 {
            bail!("unattended cleanup is limited to 100 actions; review interactively");
        }
        let known_bytes = plan
            .selected_estimate
            .ceiling
            .unwrap_or(u64::MAX)
            .saturating_add(plan.selected_provider_reported);
        if known_bytes > 50 * 1024 * 1024 * 1024 {
            bail!("unattended filesystem cleanup is limited to 50 GiB; review interactively");
        }
        if plan.actions.iter().any(|action| {
            matches!(
                action.target.properties.automation,
                Automation::ExplicitTargetOnly
            )
        }) {
            bail!("explicit-only actions require interactive confirmation and cannot use --yes");
        }
    }
    Ok(())
}

fn confirm_apply(plan: &ReviewedPlan) -> Result<bool> {
    if !io::stdin().is_terminal() {
        bail!("interactive confirmation requires a terminal; repeat with --yes after review");
    }
    eprintln!();
    eprintln!("Exact operations:");
    for action in &plan.actions {
        match &action.target.operation {
            PlannedOperation::DirectDelete {
                path,
                children_only,
                ..
            } => {
                let mode = if *children_only {
                    "delete children of"
                } else {
                    "delete"
                };
                eprintln!("  {}: {mode} {}", action.target.id, path.display());
            }
            PlannedOperation::Command { command } => {
                eprintln!("  {}: argv = {:?}", action.target.id, command_argv(command));
            }
            PlannedOperation::Informational => {}
        }
    }
    eprint!("Apply this permanent cleanup plan? [y/N] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn apply_reviewed_plan(
    plan: &ReviewedPlan,
    home: Option<&Path>,
    json: bool,
    journal: &Journal,
) -> Result<(Vec<AppliedActionOutput>, JournalRun)> {
    let mut results = Vec::with_capacity(plan.actions.len());
    let mut journal_actions = Vec::with_capacity(plan.targets.len());

    // Preserve skipped planning decisions in history as well as attempted work.
    for planned in &plan.targets {
        if let PlanSelection::Skipped(reason) = &planned.selection {
            let result = ActionResult {
                status: ActionStatus::Skipped,
                removed_allocation: None,
                provider_reported: None,
                exit_code: None,
                stdout_summary: None,
                stderr_summary: None,
                error: Some(reason.clone()),
            };
            journal_actions.push(journal_action(&planned.target).finish(&result));
        }
    }

    let selected_offset = journal_actions.len();
    for action in &plan.actions {
        journal_actions.push(journal_action(&action.target));
    }
    let mut run = JournalRun {
        schema: 1,
        run_id: plan.run_id.clone(),
        timestamp_unix_seconds: plan.created_unix_seconds,
        cleanrs_version: env!("CARGO_PKG_VERSION").into(),
        scope: plan.scope.clone(),
        level: format!("{:?}", plan.level).to_lowercase(),
        explicitly_named_ids: plan.explicitly_named_ids.clone(),
        outcome: "running".into(),
        actions: journal_actions,
    };
    // The initial durable record means an interruption is visible as a run
    // with pending actions. Each completed action appends a newer snapshot.
    journal.append(&run)?;

    for (action_index, action) in plan.actions.iter().enumerate() {
        let deletion_progress = match &action.target.operation {
            PlannedOperation::DirectDelete { path, .. } => {
                eprintln!(
                    "Applying {}: permanent delete {}",
                    action.target.id,
                    path.display()
                );
                ActionProgressIndicator::create(&action.target.id, !json && stderr_is_terminal())
            }
            PlannedOperation::Command { command } => {
                eprintln!(
                    "Applying {}: argv = {:?}",
                    action.target.id,
                    command_argv(command)
                );
                None
            }
            PlannedOperation::Informational => None,
        };
        let mut result = match &action.target.operation {
            PlannedOperation::DirectDelete {
                path,
                children_only,
                identity,
            } => match validate_direct_preflight(path, *identity, home) {
                Ok(()) => execute_direct_delete(
                    path,
                    *children_only,
                    *identity,
                    filesystem_allocated(&action.target.usage),
                ),
                Err(reason) => ActionResult::validation_abort(reason),
            },
            PlannedOperation::Command { command } => {
                if let Err(reason) = revalidate_provider(&action.target) {
                    ActionResult::validation_abort(reason)
                } else if let Some(error) = &action.executable_identity_error {
                    ActionResult::validation_abort(format!(
                        "executable identity was unavailable during review: {error}"
                    ))
                } else if let Some(identity) = action.executable_identity {
                    execute_command(command, identity, !json, 64 * 1024)
                } else {
                    ActionResult::validation_abort("reviewed command has no executable identity")
                }
            }
            PlannedOperation::Informational => {
                ActionResult::validation_abort("informational action reached execution")
            }
        };
        if let Some(progress) = deletion_progress {
            progress.finish(result.status);
        }
        if result.status == ActionStatus::Success && result.provider_reported.is_none() {
            result.provider_reported = result
                .stdout_summary
                .as_deref()
                .and_then(parse_provider_reported_bytes);
        }
        run.actions[selected_offset + action_index] =
            journal_action(&action.target).finish(&result);
        results.push(AppliedActionOutput {
            target_id: action.target.id.clone(),
            result,
        });
        journal.append(&run)?;
    }

    let successes = results
        .iter()
        .filter(|result| result.result.status == ActionStatus::Success)
        .count();
    let outcome = if successes == results.len() {
        "success"
    } else if successes == 0 {
        "failed"
    } else {
        "partial"
    };
    run.outcome = outcome.into();
    journal.append(&run)?;
    Ok((results, run))
}

fn validate_direct_preflight(
    path: &Path,
    planned: CandidateIdentity,
    home: Option<&Path>,
) -> Result<(), String> {
    if path == Path::new("/") || home.is_some_and(|home| path == home) {
        return Err("protected filesystem root".into());
    }
    if let Some(home) = home {
        for denied in [
            home.join("Library/Mobile Documents"),
            home.join("Library/CloudStorage"),
        ] {
            if path.starts_with(denied) {
                return Err("path is inside synced cloud storage".into());
            }
        }
    }
    ensure_no_symlink_ancestors(path)?;
    let parent = path.parent().ok_or("cleanup target has no parent")?;
    let parent_metadata = std::fs::metadata(parent).map_err(|error| error.to_string())?;
    let target_metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if parent_metadata.dev() != target_metadata.dev() {
            return Err("cleanup target is a mount root".into());
        }
    }
    let scan = Scanner::new(ScanOptions::default())
        .scan(path)
        .map_err(|error| format!("final metadata preflight failed: {error}"))?;
    if !scan.coverage_complete() {
        return Err("final metadata preflight had incomplete coverage".into());
    }
    let root = &scan.arena.nodes[scan.arena.root().as_usize()];
    let current = CandidateIdentity {
        device: root.device,
        inode: root.inode,
        kind: root.kind,
    };
    if current != planned {
        return Err("target device, inode, or type changed after planning".into());
    }
    if root.flags.contains(cleanrs_core::fs::NodeFlags::DATALESS) || scan.dataless_entries > 0 {
        return Err("candidate contains dataless or ubiquitous content".into());
    }
    if scan.mount_boundaries > 0 {
        return Err("candidate crosses a mount boundary".into());
    }
    Ok(())
}

fn ensure_no_symlink_ancestors(path: &Path) -> Result<(), String> {
    let mut cursor = PathBuf::new();
    for component in path.components() {
        cursor.push(component);
        let metadata = std::fs::symlink_metadata(&cursor).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "cleanup path has symlinked ancestor: {}",
                cursor.display()
            ));
        }
    }
    Ok(())
}

fn revalidate_provider(target: &DetectedTarget) -> Result<(), String> {
    if matches!(target.provider.id.as_str(), "workspace" | "configuration") {
        return Ok(());
    }
    if target.provider.id == "cargo" {
        let executable = target
            .provider
            .executable
            .clone()
            .ok_or_else(|| "planned Cargo executable is missing".to_owned())?;
        let output = SystemProviderHost::default()
            .run_inspection(&CommandSpec {
                executable,
                args: vec!["--version".into()],
                environment: Default::default(),
            })
            .map_err(|error| error.to_string())?;
        if output.exit_code != Some(0)
            || target.provider.version.as_deref() != Some(output.stdout.trim())
        {
            return Err("Cargo provider version changed after planning".into());
        }
        return Ok(());
    }
    let selected = HashSet::from([target.id.clone()]);
    let current = inspect_builtin_providers(&SystemProviderHost::default(), &selected)
        .items
        .into_iter()
        .find(|item| item.id == target.id)
        .ok_or_else(|| "provider action is no longer available".to_owned())?;
    if current.provider != target.provider {
        return Err(format!(
            "provider identity or context changed after planning (planned {:?}, current {:?})",
            target.provider, current.provider
        ));
    }
    Ok(())
}

fn filesystem_allocated(usage: &UsageEstimate) -> u64 {
    match usage {
        UsageEstimate::Filesystem { allocated, .. } => *allocated,
        _ => 0,
    }
}

fn journal_action(target: &DetectedTarget) -> JournalAction {
    let (planned_allocated, estimated_floor, estimated_ceiling, provider_estimated) =
        match &target.usage {
            UsageEstimate::Filesystem {
                allocated,
                reclaimable,
                ..
            } => (
                Some(*allocated),
                reclaimable.floor,
                reclaimable.ceiling,
                None,
            ),
            UsageEstimate::ProviderReported { bytes, .. } => (None, None, None, *bytes),
            UsageEstimate::Unknown { .. } => (None, None, None, None),
        };
    JournalAction {
        target_id: target.id.clone(),
        provider_id: target.provider.id.clone(),
        path: target.path.clone(),
        context: target.provider.context.clone(),
        argv: match &target.operation {
            PlannedOperation::Command { command } => Some(command_argv(command)),
            _ => None,
        },
        status: "pending".into(),
        planned_allocated,
        estimated_floor,
        estimated_ceiling,
        provider_estimated,
        removed_allocation: None,
        provider_reported: None,
        bytes_trashed: None,
        exit_code: None,
        stdout_summary: None,
        stderr_summary: None,
        error: None,
    }
}

fn parse_provider_reported_bytes(output: &str) -> Option<u64> {
    let words: Vec<_> = output.split_whitespace().collect();
    for index in 0..words.len() {
        let token = words[index]
            .trim_matches(|character: char| !character.is_ascii_alphanumeric() && character != '.');
        let split = token
            .find(|character: char| character.is_ascii_alphabetic())
            .unwrap_or(token.len());
        let (number, unit) = token.split_at(split);
        if number.is_empty() || unit.is_empty() {
            continue;
        }
        let value: f64 = number.parse().ok()?;
        let multiplier = match unit.to_ascii_lowercase().as_str() {
            "b" => 1_f64,
            "kb" => 1_000_f64,
            "mb" => 1_000_000_f64,
            "gb" => 1_000_000_000_f64,
            "tb" => 1_000_000_000_000_f64,
            "kib" => 1024_f64,
            "mib" => 1024_f64.powi(2),
            "gib" => 1024_f64.powi(3),
            "tib" => 1024_f64.powi(4),
            _ => continue,
        };
        let context = words[index.saturating_sub(4)..index]
            .join(" ")
            .to_ascii_lowercase();
        if context.contains("reclaim") || context.contains("free") || context.contains("removed") {
            return Some((value * multiplier) as u64);
        }
    }
    None
}

fn new_run_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("{nanos}-{}", std::process::id())
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn journal_for_environment(home: Option<&Path>) -> Result<Journal> {
    let state_home = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from);
    let path = default_journal_path(home, state_home.as_deref())
        .context("HOME or XDG_STATE_HOME is required for cleanup history")?;
    Ok(Journal::new(path))
}

fn resolve_explicit_config_trust(
    home: Option<&Path>,
    config: Option<&Path>,
    trust_now: bool,
) -> Result<Option<String>> {
    let Some(config) = config else {
        return Ok(None);
    };
    let Some(home) = home else {
        if trust_now {
            bail!("HOME is required to persist config trust");
        }
        return Ok(None);
    };
    let store = TrustStore::for_home(home);
    if trust_now {
        store.trust(config)?;
    }
    store.trusted_hash(config)
}

fn run_history(args: HistoryArgs) -> Result<()> {
    if args.limit == 0 {
        bail!("--limit must be at least 1");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let journal = journal_for_environment(home.as_deref())?;
    let mut runs = journal.read().with_context(|| {
        format!(
            "failed to read cleanup history {}",
            journal.path().display()
        )
    })?;
    runs.reverse();
    runs.truncate(args.limit);
    if args.json {
        serde_json::to_writer_pretty(io::stdout().lock(), &runs)?;
        println!();
    } else if runs.is_empty() {
        println!("No cleanup history has been recorded.");
    } else {
        for run in runs {
            println!(
                "{}  {}  {}  {} action(s)",
                run.timestamp_unix_seconds,
                run.level,
                run.outcome,
                run.actions.len()
            );
            for action in run.actions {
                let marker = match action.status.as_str() {
                    "success" => "✓",
                    "skipped" => "-",
                    "validation_abort" => "!",
                    _ => "✗",
                };
                let evidence = action
                    .removed_allocation
                    .map(|bytes| format!("removed {bytes} bytes allocation"))
                    .or_else(|| {
                        action
                            .provider_reported
                            .map(|bytes| format!("provider reported {bytes} bytes"))
                    })
                    .or(action.error)
                    .unwrap_or_else(|| action.status.clone());
                println!("  {marker} {:<28} {evidence}", action.target_id);
            }
        }
    }
    Ok(())
}

fn run_trust_config(args: TrustConfigArgs) -> Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is required for config trust")?;
    let store = TrustStore::for_home(&home);
    if args.list {
        let records = store.records()?;
        if args.json {
            serde_json::to_writer_pretty(io::stdout().lock(), &records)?;
            println!();
        } else if records.is_empty() {
            println!("No explicitly named command configurations are trusted.");
        } else {
            for record in records {
                println!("{}  {}", record.sha256, record.canonical_path.display());
            }
        }
        return Ok(());
    }
    let path = args
        .path
        .as_deref()
        .context("PATH is required unless --list is used")?;
    if args.revoke {
        let revoked = store.revoke(path)?;
        if args.json {
            serde_json::to_writer_pretty(
                io::stdout().lock(),
                &serde_json::json!({ "revoked": revoked }),
            )?;
            println!();
        } else {
            println!(
                "{}",
                if revoked {
                    "Config trust revoked."
                } else {
                    "Config was not trusted."
                }
            );
        }
    } else {
        let record = store.trust(path)?;
        if args.json {
            serde_json::to_writer_pretty(io::stdout().lock(), &record)?;
            println!();
        } else {
            println!(
                "Trusted {} at SHA-256 {}",
                record.canonical_path.display(),
                record.sha256
            );
            println!("Editing or replacing the file invalidates this trust.");
        }
    }
    Ok(())
}

fn target_sort_size(target: &DetectedTarget) -> u64 {
    target.usage.sortable_bytes()
}

fn validate_target_ids(ids: &[String], flag: &str, registry: &ResolvedRegistry) -> Result<()> {
    for id in ids {
        if !BUILTIN_TARGETS.iter().any(|definition| definition.id == id)
            && !BUILTIN_PROVIDER_ACTION_IDS.contains(&id.as_str())
            && !registry.targets.contains_key(id)
        {
            bail!("unknown target ID {id:?} passed to {flag}");
        }
    }
    Ok(())
}

fn selected_provider_ids(
    only: &[String],
    exclude: &[String],
    registry: &ResolvedRegistry,
) -> HashSet<String> {
    BUILTIN_PROVIDER_ACTION_IDS
        .iter()
        .filter(|id| only.is_empty() || only.iter().any(|selected| selected == **id))
        .filter(|id| !exclude.iter().any(|excluded| excluded == **id))
        .filter(|id| registry.permits(id))
        .filter(|id| !registry.targets.contains_key(**id))
        .map(|id| (*id).to_owned())
        .collect()
}

fn dedupe_inventory_paths(items: Vec<DetectedTarget>) -> Vec<DetectedTarget> {
    let mut output: Vec<DetectedTarget> = Vec::new();
    for item in items {
        if let Some(path) = item.path.as_ref() {
            if let Some(existing) = output
                .iter()
                .position(|candidate| candidate.path.as_ref() == Some(path))
            {
                if output[existing].provider.id == "configuration" {
                    continue;
                }
                // Registry and provider items are appended after embedded
                // marker findings, so the later, more specific resolution wins.
                output[existing] = item;
                continue;
            }
        }
        output.push(item);
    }
    output
}

fn run_scan(args: ScanArgs) -> Result<()> {
    let metric = selected_metric(&args);
    let mut options = ScanOptions {
        cross_mounts: args.cross_mounts,
        excludes: args.exclude,
        ..ScanOptions::default()
    };
    if let Some(workers) = args.workers {
        if workers == 0 {
            bail!("--workers must be at least 1");
        }
        options.workers = workers;
    }
    let scanner = Scanner::new(options);
    let progress = ScanProgressIndicator::create(
        &args.path.display().to_string(),
        !args.json && stderr_is_terminal(),
    );
    let result = if let Some(indicator) = &progress {
        scanner.scan_with_observer(&args.path, indicator.clone())
    } else {
        scanner.scan(&args.path)
    };
    if let Some(indicator) = &progress {
        indicator.finish();
    }
    let result = result.with_context(|| format!("failed to scan {}", args.path.display()))?;
    let output = build_output(
        &result,
        metric,
        args.depth,
        args.min_size,
        args.explain.as_deref(),
    )?;
    if args.json {
        serde_json::to_writer_pretty(io::stdout().lock(), &output)?;
        println!();
    } else {
        TerminalUi::detect().print_scan(&output, metric);
    }
    Ok(())
}

fn selected_metric(args: &ScanArgs) -> Metric {
    if args.apparent {
        Metric::Apparent
    } else if args.private {
        Metric::Private
    } else if args.reclaimable {
        Metric::Reclaimable
    } else {
        Metric::Allocated
    }
}

fn build_output(
    result: &ScanResult,
    metric: Metric,
    max_depth: usize,
    min_size: u64,
    explain: Option<&Path>,
) -> Result<ScanOutput> {
    let coverage_complete = result.coverage_complete();
    let targets = if let Some(path) = explain {
        let requested = if path.is_absolute() {
            path.to_path_buf()
        } else {
            result.root.join(path)
        };
        let requested = requested.canonicalize().unwrap_or(requested);
        let index = (0..result.arena.nodes.len())
            .map(NodeIndex::from_usize)
            .find(|index| result.arena.path(*index) == requested)
            .with_context(|| format!("{} is not part of this scan", requested.display()))?;
        vec![(index, depth_of(&result.arena, index))]
    } else {
        nodes_through_depth(&result.arena, max_depth)
    };

    let mut entries: Vec<_> = targets
        .into_iter()
        .map(|(index, depth)| {
            let (apparent, allocated) = subtree_sizes(&result.arena, index);
            OutputEntry {
                path: result.arena.path(index),
                depth,
                apparent,
                allocated,
                estimate: estimate_subtree(&result.arena, index, coverage_complete),
            }
        })
        .filter(|entry| metric_value(entry, metric) >= min_size)
        .collect();
    entries.sort_by(|left, right| {
        Reverse(metric_value(left, metric))
            .cmp(&Reverse(metric_value(right, metric)))
            .then_with(|| left.path.cmp(&right.path))
    });

    Ok(ScanOutput {
        root: result.root.clone(),
        metric: metric_name(metric),
        backend: result.backend.clone(),
        coverage_complete,
        coverage_errors: result.coverage_errors.clone(),
        dataless_entries: result.dataless_entries,
        mount_boundaries: result.mount_boundaries,
        entries,
    })
}

fn run_doctor(args: DoctorArgs) -> Result<()> {
    let mut report = doctor().context("failed to inspect filesystem state")?;
    if let Some(path) = args.scan.as_ref() {
        let scanner = Scanner::new(ScanOptions::default());
        let progress = ScanProgressIndicator::create(
            &path.display().to_string(),
            !args.json && stderr_is_terminal(),
        );
        let scan = if let Some(indicator) = &progress {
            scanner.scan_with_observer(path, indicator.clone())
        } else {
            scanner.scan(path)
        };
        if let Some(indicator) = &progress {
            indicator.finish();
        }
        let scan =
            scan.with_context(|| format!("failed to scan {} for reconciliation", path.display()))?;
        let estimate = estimate_subtree(&scan.arena, scan.arena.root(), scan.coverage_complete());
        report.attributed_allocated = estimate.ceiling;
        report.scan_coverage_complete = Some(scan.coverage_complete());
        let data_used = report
            .volumes
            .iter()
            .find(|volume| volume.name == "Data")
            .map_or(report.used, |volume| volume.used);
        report.unattributed = estimate
            .ceiling
            .map(|value| data_used.saturating_sub(value));
        report.notes.push(format!(
            "Attributed values cover only the explicit scan scope {}.",
            scan.root.display()
        ));
        if !scan.coverage_complete() {
            report.notes.push(format!(
                "Scan coverage is incomplete at {} locations; their size is unknown.",
                scan.coverage_errors.len()
            ));
        }
    }
    if args.json {
        serde_json::to_writer_pretty(io::stdout().lock(), &report)?;
        println!();
        return Ok(());
    }
    TerminalUi::detect().print_doctor(&report);
    Ok(())
}

fn nodes_through_depth(arena: &Arena, max_depth: usize) -> Vec<(NodeIndex, usize)> {
    let mut output = Vec::new();
    let mut queue = std::collections::VecDeque::from([(arena.root(), 0)]);
    while let Some((index, depth)) = queue.pop_front() {
        output.push((index, depth));
        if depth < max_depth {
            queue.extend(arena.children(index).map(|child| (child, depth + 1)));
        }
    }
    output
}

fn depth_of(arena: &Arena, index: NodeIndex) -> usize {
    let mut depth = 0;
    let mut cursor = index;
    while cursor != arena.root() {
        depth += 1;
        cursor = arena.parent(cursor).expect("non-root node has parent");
    }
    depth
}

fn subtree_sizes(arena: &Arena, root: NodeIndex) -> (u64, u64) {
    use std::collections::HashSet;
    let mut stack = vec![root];
    let mut apparent = 0_u64;
    let mut allocated = 0_u64;
    let mut allocated_inodes = HashSet::new();
    while let Some(index) = stack.pop() {
        let node = &arena.nodes[index.as_usize()];
        stack.extend(arena.children(index));
        if node.kind.is_file_like() {
            apparent = apparent.saturating_add(node.apparent_size);
            if allocated_inodes.insert((node.device, node.inode)) {
                allocated = allocated.saturating_add(node.allocated_size);
            }
        }
    }
    (apparent, allocated)
}

fn metric_value(entry: &OutputEntry, metric: Metric) -> u64 {
    match metric {
        Metric::Apparent => entry.apparent,
        Metric::Allocated => entry.allocated,
        Metric::Private => entry.estimate.floor.unwrap_or_default(),
        Metric::Reclaimable => entry.estimate.ceiling.unwrap_or_default(),
    }
}

const fn metric_name(metric: Metric) -> &'static str {
    match metric {
        Metric::Apparent => "apparent",
        Metric::Allocated => "allocated",
        Metric::Private => "immediately private",
        Metric::Reclaimable => "estimated reclaimable",
    }
}

pub(crate) fn format_interval(estimate: &ReclaimEstimate) -> String {
    match (estimate.floor, estimate.ceiling) {
        (Some(floor), Some(ceiling)) if floor == ceiling => human_bytes(floor),
        (Some(floor), Some(ceiling)) => format!("{}–{}", human_bytes(floor), human_bytes(ceiling)),
        (None, Some(ceiling)) => format!("unknown–{}", human_bytes(ceiling)),
        _ => "unknown".into(),
    }
}

fn parse_size(value: &str) -> Result<u64, String> {
    let split = value
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(value.len());
    let number: u64 = value[..split]
        .parse()
        .map_err(|_| "size must start with an integer")?;
    let suffix = value[split..].trim().to_ascii_lowercase();
    let multiplier = match suffix.as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1_000,
        "m" | "mb" => 1_000_000,
        "g" | "gb" => 1_000_000_000,
        "t" | "tb" => 1_000_000_000_000,
        "kib" => 1 << 10,
        "mib" => 1 << 20,
        "gib" => 1 << 30,
        "tib" => 1_u64 << 40,
        _ => return Err(format!("unsupported size suffix {suffix:?}")),
    };
    number
        .checked_mul(multiplier)
        .ok_or_else(|| "size is too large".into())
}

pub(crate) fn human_bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut scaled = value as f64;
    let mut unit = 0;
    while scaled >= 1024.0 && unit < UNITS.len() - 1 {
        scaled /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} B")
    } else {
        format!("{scaled:.1} {}", UNITS[unit])
    }
}

fn init_logging(verbose: u8) {
    let fallback = match verbose {
        0 => "warn",
        1 => "cleanrs=info,cleanrs_core=info",
        _ => "cleanrs=debug,cleanrs_core=debug",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(fallback)),
        )
        .with_writer(io::stderr)
        .with_ansi(std::env::var_os("NO_COLOR").is_none())
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_binary_and_decimal_sizes() {
        assert_eq!(parse_size("2MiB"), Ok(2 * 1024 * 1024));
        assert_eq!(parse_size("3GB"), Ok(3_000_000_000));
    }

    #[test]
    fn command_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
