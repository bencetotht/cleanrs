//! Non-printing cleanup handoff shared by CLI presentation and future TUI
//! workers. This lives in the binary crate because registry resolution and
//! config trust are binary policy; filesystem planning/execution primitives
//! remain in cleanrs-core. Building a plan performs inspection only.

use crate::registry::ResolvedRegistry;
use crate::trust::resolve_explicit_config_trust;
use anyhow::{Context, Result, bail};
use cleanrs_core::accounting::estimate_subtrees;
use cleanrs_core::execution::{
    ActionResult, ActionStatus, ExecutableIdentity, OutputStream, command_argv,
    executable_identity, execute_command_with_observer, execute_direct_delete,
};
use cleanrs_core::fs::{
    Arena, FileSystem, NativeFileSystem, Node, NodeFlags, NodeIndex, NodeKind, ScanObserver,
    ScanProgress, ScanResult,
};
use cleanrs_core::journal::{Journal, JournalAction, JournalRun, default_journal_path};
use cleanrs_core::providers::{
    BUILTIN_PROVIDER_ACTION_IDS, ProviderHost, ProviderStatus, SystemProviderHost,
    inspect_builtin_providers, inspect_cargo_builds_with_progress,
};
use cleanrs_core::targets::{
    Automation, BUILTIN_TARGETS, CandidateIdentity, CandidateKey, CleanupLevel, CommandSpec,
    DetectedTarget, PlanSelection, PlannedOperation, PlannedTarget, SelectionMode, SelectionSource,
    UsageEstimate, detect_workspace_targets, plan_cleanup, plan_cleanup_with_selection,
};
use cleanrs_core::{ReclaimEstimate, ScanOptions, Scanner};
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub(crate) struct PlanRequest {
    pub(crate) path: PathBuf,
    pub(crate) home: Option<PathBuf>,
    pub(crate) level: CleanupLevel,
    pub(crate) explicitly_named_ids: Vec<String>,
    pub(crate) only: Vec<String>,
    pub(crate) exclude: Vec<String>,
    pub(crate) ignore_staleness: bool,
    pub(crate) workers: Option<usize>,
    pub(crate) workspace_only: bool,
    pub(crate) cargo_stale_after: u32,
    pub(crate) config: Option<PathBuf>,
    pub(crate) no_builtin_targets: bool,
    pub(crate) target: Vec<String>,
    pub(crate) target_argv: Vec<String>,
    pub(crate) targets_only_cli: bool,
    pub(crate) manual_selections: HashSet<CandidateKey>,
    /// CLI path/key spellings, resolved after detection. TUI uses keys directly.
    pub(crate) select: Vec<String>,
    pub(crate) selection_mode: SelectionMode,
}

#[derive(Debug)]
pub(crate) enum PlanEvent {
    PhaseStarted { phase: &'static str },
    PhaseFinished { phase: &'static str, count: usize },
    CargoMetadata { completed: usize, total: usize },
}

pub(crate) trait PlanObserver: Send + Sync {
    fn on_event(&self, event: PlanEvent);
    fn on_scan_progress(&self, progress: ScanProgress);
}
struct PlanningScanObserver(Arc<dyn PlanObserver>);
impl ScanObserver for PlanningScanObserver {
    fn on_progress(&self, progress: ScanProgress) {
        self.0.on_scan_progress(progress);
    }
}

#[derive(Debug)]
pub(crate) enum ExecutionEvent<'a> {
    RunStarted {
        total: usize,
    },
    ActionStarted {
        index: usize,
        target: &'a DetectedTarget,
    },
    /// Raw bytes including newlines; long lines arrive in bounded fragments.
    OutputLine {
        index: usize,
        stream: OutputStream,
        line: &'a [u8],
    },
    ActionFinished {
        index: usize,
        result: &'a ActionResult,
    },
    RunFinished {
        summary: &'a JournalRun,
    },
}
pub(crate) trait ExecutionObserver {
    fn on_event(&mut self, event: ExecutionEvent<'_>);
}

/// The owned handoff shared by rendering, confirmation, execution, and history.
/// No action is rediscovered or widened after this value is constructed.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReviewedPlan {
    #[serde(skip)]
    pub(crate) scan_finished: Option<std::time::Instant>,
    #[serde(skip)]
    pub(crate) scan_age_offset: std::time::Duration,
    pub(crate) coverage_complete: bool,
    pub(crate) providers: Vec<ProviderStatus>,
    pub(crate) selected_provider_actions: usize,
    pub(crate) selected_unknown_actions: usize,
    pub(crate) run_id: String,
    pub(crate) created_unix_seconds: u64,
    pub(crate) scope: PathBuf,
    pub(crate) level: CleanupLevel,
    pub(crate) explicitly_named_ids: Vec<String>,
    pub(crate) targets: Vec<PlannedTarget>,
    pub(crate) selected_estimate: ReclaimEstimate,
    pub(crate) selected_provider_reported: u64,
    pub(crate) actions: Vec<ReviewedAction>,
    /// Detection facts for live totals; never serialized or used to mutate.
    #[serde(skip)]
    pub(crate) accounting: Arc<SelectionAccounting>,
}

/// Cached source arena and candidate roots preserve set accounting (including
/// overlapping trees, hardlinks and clones), rather than adding row estimates.
#[derive(Debug)]
pub(crate) struct SelectionAccounting {
    arena: Arena,
    roots: HashMap<CandidateKey, NodeIndex>,
    coverage_complete: bool,
    presets: [Vec<PlanSelection>; 3],
}
#[derive(Debug, Clone)]
pub(crate) struct SelectionSummary {
    pub(crate) count: usize,
    pub(crate) estimate: ReclaimEstimate,
    pub(crate) provider_reported: u64,
    pub(crate) provider_actions: usize,
    pub(crate) unknown_actions: usize,
}
impl SelectionAccounting {
    pub(crate) fn node_count(&self) -> usize {
        self.arena.nodes.len()
    }

    pub(crate) fn new(arena: Arena, coverage_complete: bool, targets: &[PlannedTarget]) -> Self {
        let paths: HashMap<_, _> = targets
            .iter()
            .filter(|t| matches!(t.target.usage, UsageEstimate::Filesystem { .. }))
            .map(|t| {
                (
                    t.target.path.as_ref().expect("filesystem target path"),
                    &t.candidate_key,
                )
            })
            .collect();
        let roots = (0..arena.nodes.len())
            .map(NodeIndex::from_usize)
            .filter_map(|index| {
                paths
                    .get(&arena.path(index))
                    .map(|key| ((*key).clone(), index))
            })
            .collect::<HashMap<_, _>>();
        // Resolve every level on the planning worker. The core planner also
        // canonicalizes candidate keys; keeping just its policy results here
        // lets presets stay pure in-memory App transitions, preserving the
        // original detection keys and exact exclusive/safe-limit decisions.
        let presets = [
            CleanupLevel::Safe,
            CleanupLevel::Standard,
            CleanupLevel::Aggressive,
        ]
        .map(|level| {
            plan_cleanup(
                targets.iter().map(|r| r.target.clone()).collect(),
                level,
                &[],
                false,
            )
            .into_iter()
            .map(|row| row.selection)
            .collect()
        });
        Self {
            arena,
            roots,
            coverage_complete,
            presets,
        }
    }
    pub(crate) fn preset(&self, level: CleanupLevel) -> &[PlanSelection] {
        &self.presets[match level {
            CleanupLevel::Safe => 0,
            CleanupLevel::Standard => 1,
            CleanupLevel::Aggressive => 2,
        }]
    }
    fn identity(&self, key: &CandidateKey) -> Option<CandidateIdentity> {
        self.roots
            .get(key)
            .map(|index| node_identity(&self.arena, *index))
    }
    /// CLI plan summaries and TUI live selection use exactly this computation.
    pub(crate) fn summarize<'a>(
        &self,
        selected: impl Iterator<Item = &'a PlannedTarget>,
    ) -> SelectionSummary {
        let selected: Vec<_> = selected.collect();
        let roots: Vec<_> = selected
            .iter()
            .filter(|t| matches!(t.target.usage, UsageEstimate::Filesystem { .. }))
            .map(|t| {
                *self
                    .roots
                    .get(&t.candidate_key)
                    .expect("selected filesystem target must remain in its source arena")
            })
            .collect();
        SelectionSummary {
            count: selected.len(),
            estimate: estimate_subtrees(&self.arena, &roots, self.coverage_complete),
            provider_reported: selected
                .iter()
                .filter_map(|t| match t.target.usage {
                    UsageEstimate::ProviderReported { bytes, .. } => bytes,
                    _ => None,
                })
                .fold(0_u64, u64::saturating_add),
            provider_actions: selected
                .iter()
                .filter(|t| matches!(t.target.usage, UsageEstimate::ProviderReported { .. }))
                .count(),
            unknown_actions: selected
                .iter()
                .filter(|t| matches!(t.target.usage, UsageEstimate::Unknown { .. }))
                .count(),
        }
    }
}

fn node_identity(arena: &Arena, index: NodeIndex) -> CandidateIdentity {
    let node = &arena.nodes[index.as_usize()];
    CandidateIdentity {
        device: node.device,
        inode: node.inode,
        kind: node.kind,
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReviewedAction {
    pub(crate) target: DetectedTarget,
    pub(crate) selection_source: SelectionSource,
    pub(crate) executable_identity: Option<ExecutableIdentity>,
    pub(crate) executable_identity_error: Option<String>,
}

pub(crate) fn build_reviewed_plan(
    request: PlanRequest,
    observer: Arc<dyn PlanObserver>,
) -> Result<ReviewedPlan> {
    build_plan(request, observer, None, None)
}

/// Interactive review may only narrow stale ticks. CLI selection still fails
/// closed on unknown/refused keys; App explains every dropped tick before y.
pub(crate) fn build_tui_reviewed_plan(
    request: PlanRequest,
    observer: Arc<dyn PlanObserver>,
    detection: &ReviewedPlan,
) -> Result<ReviewedPlan> {
    build_plan(request, observer, Some(detection), None)
}
/// Browser facts accelerate discovery only. Execution always revalidates live paths.
pub(crate) fn build_plan_from_scan(
    request: PlanRequest,
    observer: Arc<dyn PlanObserver>,
    scan: ScanResult,
    finished: std::time::Instant,
    age_offset: std::time::Duration,
) -> Result<ReviewedPlan> {
    let mut plan = build_plan(request, observer, None, Some(scan))?;
    plan.scan_finished = Some(finished);
    plan.scan_age_offset = age_offset;
    Ok(plan)
}

/// Review enumerates only ancestors and selected candidate trees. New unrelated
/// projects cannot widen a manual selection, so walking them serves no purpose.
fn scan_review_selection(
    request: &PlanRequest,
    previous: &ReviewedPlan,
    scanner: &Scanner,
    observer: Arc<dyn ScanObserver>,
) -> Result<ScanResult> {
    let paths: Vec<_> = previous
        .targets
        .iter()
        .filter(|t| request.manual_selections.contains(&t.candidate_key))
        .filter_map(|t| t.target.path.clone())
        .filter(|p| p.starts_with(&request.path))
        .collect();
    let filesystem = NativeFileSystem;
    let root = filesystem.metadata(&request.path)?;
    let mut arena = Arena::new(&request.path);
    arena.update_metadata(arena.root(), metadata_node(&root));
    let mut errors = Vec::new();
    let mut pending = vec![(request.path.clone(), arena.root())];
    while let Some((path, parent)) = pending.pop() {
        let node = &arena.nodes[parent.as_usize()];
        if node.kind != NodeKind::Directory
            || node
                .flags
                .intersects(NodeFlags::DATALESS | NodeFlags::FIRMLINK | NodeFlags::MOUNT_BOUNDARY)
        {
            continue;
        }
        if paths.iter().any(|p| p == &path) {
            let scan = scanner.scan_with_observer(&path, observer.clone())?;

            arena.update_metadata(parent, scan.arena.nodes[0].clone());
            // Root links belong to the destination arena, not the source scan.
            // Copy metadata through push_child for descendants; leave root links intact.
            scan.arena
                .copy_children_into(scan.arena.root(), &mut arena, parent);
            errors.extend(scan.coverage_errors);
            continue;
        }
        match filesystem.read_dir(&path) {
            Ok(entries) => {
                for entry in entries {
                    let child = path.join(&entry.name);
                    let mut node = metadata_node(&entry.metadata);
                    if node.device != root.device {
                        node.flags.insert(NodeFlags::MOUNT_BOUNDARY);
                    }
                    let index = arena.push_child(parent, &entry.name.to_string_lossy(), node);
                    if paths.iter().any(|p| p.starts_with(&child)) {
                        pending.push((child, index));
                    }
                }
            }
            Err(error) => errors.push(cleanrs_core::fs::CoverageError {
                path,
                kind: "review".into(),
                message: error.to_string(),
            }),
        }
    }
    let dataless_entries = arena
        .nodes
        .iter()
        .filter(|n| n.flags.contains(NodeFlags::DATALESS))
        .count();
    let mount_boundaries = arena
        .nodes
        .iter()
        .filter(|n| n.flags.contains(NodeFlags::MOUNT_BOUNDARY))
        .count();
    Ok(ScanResult {
        root: request.path.clone(),
        arena,
        coverage_errors: errors,
        backend: "selected review".into(),
        dataless_entries,
        mount_boundaries,
    })
}

pub(crate) fn metadata_node(m: &cleanrs_core::fs::EntryMetadata) -> Node {
    let mut node = Node::default();
    node.kind = m.kind;
    node.apparent_size = m.apparent_size;
    node.allocated_size = m.allocated_size;
    node.private_size = m.private_size;
    node.device = m.device;
    node.inode = m.inode;
    node.link_count = m.link_count;
    node.clone_id = m.clone_id;
    node.clone_refcount = m.clone_refcount;
    node.modified_unix_seconds = m.modified_unix_seconds;
    node.flags = m.flags;
    node
}

fn build_plan(
    request: PlanRequest,
    observer: Arc<dyn PlanObserver>,
    detection: Option<&ReviewedPlan>,
    cached_scan: Option<ScanResult>,
) -> Result<ReviewedPlan> {
    if request.workers == Some(0) {
        bail!("--workers must be at least 1");
    }
    let home = request.home.clone();
    let trusted_hash =
        resolve_explicit_config_trust(home.as_deref(), request.config.as_deref(), false)?;
    let mut registry = if request.targets_only_cli {
        ResolvedRegistry::default()
    } else {
        ResolvedRegistry::load(
            home.as_deref(),
            request.config.as_deref(),
            trusted_hash.as_deref(),
        )?
    };
    registry.add_cli_targets(&request.target, &request.target_argv)?;
    validate_target_ids(&request.only, "--only", &registry)?;
    validate_target_ids(&request.exclude, "--exclude", &registry)?;
    let definitions: Vec<_> = BUILTIN_TARGETS
        .iter()
        .copied()
        .filter(|_| !request.no_builtin_targets && !request.targets_only_cli)
        .filter(|definition| registry.permits(definition.id))
        .filter(|definition| !registry.targets.contains_key(definition.id))
        .filter(|definition| {
            request.only.is_empty() || request.only.iter().any(|id| id == definition.id)
        })
        .filter(|definition| !request.exclude.iter().any(|id| id == definition.id))
        .map(|mut definition| {
            if definition.id == "cargo.target" {
                definition.stale_after_days = request.cargo_stale_after;
            }
            definition
        })
        .collect();

    let mut options = ScanOptions::default();
    if let Some(workers) = request.workers {
        options.workers = workers;
    }
    let scanner = Scanner::new(options);
    observer.on_event(PlanEvent::PhaseStarted {
        phase: "workspace detection",
    });
    let scan = if let Some(scan) = cached_scan {
        scan
    } else if let Some(previous) = detection {
        scan_review_selection(
            &request,
            previous,
            &scanner,
            Arc::new(PlanningScanObserver(observer.clone())),
        )?
    } else {
        scanner
            .scan_with_observer(
                &request.path,
                Arc::new(PlanningScanObserver(observer.clone())),
            )
            .with_context(|| {
                format!("failed to build cleanup plan in {}", request.path.display())
            })?
    };
    let mut detected = detect_workspace_targets(&scan, &definitions, home.as_deref());
    detected.extend(
        registry
            .detect(&scan, home.as_deref(), &SystemProviderHost::default())
            .into_iter()
            .filter(|item| request.only.is_empty() || request.only.contains(&item.id))
            .filter(|item| !request.exclude.contains(&item.id)),
    );
    observer.on_event(PlanEvent::PhaseFinished {
        phase: "workspace detection",
        count: detected.len(),
    });
    observer.on_event(PlanEvent::PhaseStarted {
        phase: "provider inspection",
    });
    let provider_ids = if request.no_builtin_targets || request.targets_only_cli {
        HashSet::new()
    } else {
        selected_provider_ids(&request.only, &request.exclude, &registry)
    };
    let mut provider_inventory = if request.workspace_only || provider_ids.is_empty() {
        Default::default()
    } else {
        inspect_builtin_providers(&SystemProviderHost::default(), &provider_ids)
    };
    if !request.no_builtin_targets
        && !request.targets_only_cli
        && registry.permits("cargo.target")
        && !registry.targets.contains_key("cargo.target")
        && (request.only.is_empty() || request.only.iter().any(|id| id == "cargo.target"))
    {
        let cargo_inventory = inspect_cargo_builds_with_progress(
            &scan,
            &SystemProviderHost::default(),
            home.as_deref(),
            request.cargo_stale_after,
            |completed, total| observer.on_event(PlanEvent::CargoMetadata { completed, total }),
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
    let provider_count = provider_inventory.items.len();
    detected.extend(provider_inventory.items);
    detected = dedupe_inventory_paths(detected);
    let level = request.level;
    observer.on_event(PlanEvent::PhaseFinished {
        phase: "provider inspection",
        count: provider_count,
    });
    let mut manual =
        resolve_manual_selections(&detected, &request.manual_selections, &request.select)?;
    if let Some(previous) = detection {
        // Compare captured identities and exact operations before accepting a
        // stale tick. CandidateKey is a stable path/ID, not an inode identity.
        let paths: HashSet<_> = detected
            .iter()
            .filter(|t| matches!(t.usage, UsageEstimate::Filesystem { .. }))
            .filter_map(|t| t.path.as_ref())
            .collect();
        let identities: HashMap<_, _> = (0..scan.arena.nodes.len())
            .map(NodeIndex::from_usize)
            .filter_map(|index| {
                let path = scan.arena.path(index);
                paths
                    .contains(&path)
                    .then(|| (path, node_identity(&scan.arena, index)))
            })
            .collect();
        let selectable: HashSet<_> = detected
            .iter()
            .filter(|t| t.manually_selectable())
            .filter(|t| {
                let key = CandidateKey::from_target(t);
                previous
                    .targets
                    .iter()
                    .find(|old| old.candidate_key == key)
                    .is_some_and(|old| {
                        old.target.operation == t.operation
                            && old.target.provider == t.provider
                            && old.target.usage == t.usage
                            && old.target.properties == t.properties
                            && old.target.minimum_level == t.minimum_level
                            && old.target.consequence == t.consequence
                            && previous.accounting.identity(&key)
                                == t.path
                                    .as_ref()
                                    .and_then(|path| identities.get(path).copied())
                    })
            })
            .map(CandidateKey::from_target)
            .collect();
        manual.retain(|key| selectable.contains(key));
    }
    let mut targets = plan_cleanup_with_selection(
        detected,
        level,
        &request.explicitly_named_ids,
        request.ignore_staleness,
        &manual,
        request.selection_mode,
    )?;
    targets.sort_by(|left, right| {
        let left_selected = matches!(left.selection, PlanSelection::Selected { .. });
        let right_selected = matches!(right.selection, PlanSelection::Selected { .. });
        Reverse((left_selected, target_sort_size(&left.target)))
            .cmp(&Reverse((right_selected, target_sort_size(&right.target))))
            .then_with(|| left.target.path.cmp(&right.target.path))
    });

    let selected: Vec<_> = targets
        .iter()
        .filter(|target| matches!(target.selection, PlanSelection::Selected { .. }))
        .collect();
    let coverage_complete = scan.coverage_complete();
    let accounting = Arc::new(SelectionAccounting::new(
        scan.arena,
        coverage_complete,
        &targets,
    ));
    let summary = accounting.summarize(selected.iter().copied());
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
                selection_source: match &planned.selection {
                    PlanSelection::Selected { source } => source.clone(),
                    _ => unreachable!(),
                },
                executable_identity: identity,
                executable_identity_error: error,
            }
        })
        .collect();
    let plan = ReviewedPlan {
        scan_finished: None,
        scan_age_offset: std::time::Duration::ZERO,
        coverage_complete,
        providers: provider_inventory.statuses,
        selected_provider_actions: summary.provider_actions,
        selected_unknown_actions: summary.unknown_actions,
        run_id: new_run_id(),
        created_unix_seconds: now_seconds(),
        scope: scan.root.clone(),
        level,
        explicitly_named_ids: request.explicitly_named_ids.clone(),
        targets,
        selected_estimate: summary.estimate,
        selected_provider_reported: summary.provider_reported,
        actions,
        accounting,
    };
    Ok(plan)
}

fn resolve_manual_selections(
    detected: &[DetectedTarget],
    keys: &HashSet<CandidateKey>,
    values: &[String],
) -> Result<HashSet<CandidateKey>> {
    let mut manual = keys.clone();
    for value in values {
        if let Some(item) = detected
            .iter()
            .find(|item| CandidateKey::from_target(item).to_string() == *value)
        {
            manual.insert(CandidateKey::from_target(item));
            continue;
        }
        let path = Path::new(value);
        let canonical = path.canonicalize().unwrap_or_else(|_| {
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir().unwrap_or_default().join(path)
            }
        });
        let matching: Vec<_> = detected
            .iter()
            .filter(|item| {
                item.path.as_ref().is_some_and(|path| {
                    path.canonicalize().unwrap_or_else(|_| path.clone()) == canonical
                })
            })
            .collect();
        if matching.len() > 1 {
            bail!(
                "ambiguous candidate path {value}; select an exact CandidateKey: {}",
                matching
                    .iter()
                    .map(|item| CandidateKey::from_target(item).to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if let Some(item) = matching.first() {
            manual.insert(CandidateKey::from_target(item));
        } else {
            let possible = detected
                .iter()
                .filter_map(|item| item.path.as_ref())
                .filter(|path| path.starts_with(&canonical))
                .take(5)
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            bail!(
                "unknown cleanup candidate: {value}{}",
                if possible.is_empty() {
                    String::new()
                } else {
                    format!("; possible candidates: {}", possible.join(", "))
                }
            );
        }
    }
    Ok(manual)
}

pub(crate) fn enforce_apply_boundary(plan: &ReviewedPlan, unattended: bool) -> Result<()> {
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

pub(crate) fn execute_reviewed_plan(
    plan: &ReviewedPlan,
    home: Option<&Path>,
    unattended: bool,
    stop: &AtomicBool,
    observer: &mut impl ExecutionObserver,
    journal: &Journal,
) -> Result<(Vec<AppliedActionOutput>, JournalRun)> {
    enforce_apply_boundary(plan, unattended)?;
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
            journal_actions.push(journal_action(&planned.target, None).finish(&result));
        }
    }

    let selected_offset = journal_actions.len();
    for action in &plan.actions {
        journal_actions.push(journal_action(
            &action.target,
            Some(&action.selection_source),
        ));
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

    observer.on_event(ExecutionEvent::RunStarted {
        total: plan.actions.len(),
    });
    let mut stopped = false;
    for (action_index, action) in plan.actions.iter().enumerate() {
        // Only check between actions. Once started, deletion/command execution
        // completes with its normal validation and durable result snapshot.
        stopped |= stop.load(Ordering::Acquire);
        if !stopped {
            observer.on_event(ExecutionEvent::ActionStarted {
                index: action_index,
                target: &action.target,
            });
        }
        let mut result = if stopped {
            ActionResult {
                status: ActionStatus::Skipped,
                removed_allocation: None,
                provider_reported: None,
                exit_code: None,
                stdout_summary: None,
                stderr_summary: None,
                error: Some("stopped by user".into()),
            }
        } else {
            match &action.target.operation {
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
                        execute_command_with_observer(
                            command,
                            identity,
                            64 * 1024,
                            |stream, line| {
                                observer.on_event(ExecutionEvent::OutputLine {
                                    index: action_index,
                                    stream,
                                    line,
                                });
                            },
                        )
                    } else {
                        ActionResult::validation_abort(
                            "reviewed command has no executable identity",
                        )
                    }
                }
                PlannedOperation::Informational => {
                    ActionResult::validation_abort("informational action reached execution")
                }
            }
        };
        if result.status == ActionStatus::Success && result.provider_reported.is_none() {
            result.provider_reported = result
                .stdout_summary
                .as_deref()
                .and_then(parse_provider_reported_bytes);
        }
        run.actions[selected_offset + action_index] =
            journal_action(&action.target, Some(&action.selection_source)).finish(&result);
        results.push(AppliedActionOutput {
            target_id: action.target.id.clone(),
            result,
        });
        journal.append(&run)?;
        observer.on_event(ExecutionEvent::ActionFinished {
            index: action_index,
            result: &results.last().expect("completed action").result,
        });
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
    observer.on_event(ExecutionEvent::RunFinished { summary: &run });
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

fn journal_action(target: &DetectedTarget, source: Option<&SelectionSource>) -> JournalAction {
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
        selection_source: if matches!(source, Some(SelectionSource::Manual { .. })) {
            "manual"
        } else {
            "level"
        }
        .into(),
        overridden_skip_reason: match source {
            Some(SelectionSource::Manual { overrode }) => overrode.clone(),
            _ => None,
        },
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

pub(crate) fn journal_for_environment(home: Option<&Path>) -> Result<Journal> {
    let state_home = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from);
    let path = default_journal_path(home, state_home.as_deref())
        .context("HOME or XDG_STATE_HOME is required for cleanup history")?;
    Ok(Journal::new(path))
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AppliedActionOutput {
    pub(crate) target_id: String,
    pub(crate) result: ActionResult,
}

pub(crate) fn target_sort_size(target: &DetectedTarget) -> u64 {
    target.usage.sortable_bytes()
}

pub(crate) fn validate_target_ids(
    ids: &[String],
    flag: &str,
    registry: &ResolvedRegistry,
) -> Result<()> {
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

pub(crate) fn selected_provider_ids(
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

pub(crate) fn dedupe_inventory_paths(items: Vec<DetectedTarget>) -> Vec<DetectedTarget> {
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Mutex;

    use super::*;
    use tempfile::tempdir;

    #[derive(Default)]
    struct RecordingPlanObserver(Mutex<Vec<String>>);
    impl PlanObserver for RecordingPlanObserver {
        fn on_event(&self, event: PlanEvent) {
            self.0.lock().unwrap().push(format!("{event:?}"));
        }
        fn on_scan_progress(&self, _progress: ScanProgress) {}
    }

    fn request(scope: &Path) -> PlanRequest {
        PlanRequest {
            path: scope.into(),
            home: Some(scope.into()),
            level: CleanupLevel::Safe,
            explicitly_named_ids: Vec::new(),
            only: vec!["node.next".into()],
            exclude: Vec::new(),
            ignore_staleness: true,
            workers: Some(1),
            workspace_only: true,
            cargo_stale_after: 14,
            config: None,
            no_builtin_targets: false,
            target: Vec::new(),
            target_argv: Vec::new(),
            targets_only_cli: false,
            manual_selections: HashSet::new(),
            select: Vec::new(),
            selection_mode: SelectionMode::LevelPlusManual,
        }
    }

    struct Recorder<'a> {
        events: Vec<String>,
        stop: &'a AtomicBool,
        stop_after_start: bool,
        journal: &'a Journal,
    }
    impl ExecutionObserver for Recorder<'_> {
        fn on_event(&mut self, event: ExecutionEvent<'_>) {
            match event {
                ExecutionEvent::RunStarted { total } => {
                    let run = &self.journal.read().unwrap()[0];
                    assert_eq!(run.outcome, "running");
                    assert!(
                        run.actions
                            .iter()
                            .filter(|action| action.status == "pending")
                            .count()
                            == total
                    );
                    self.events.push(format!("run:{total}"));
                }
                ExecutionEvent::ActionStarted { index, .. } => {
                    self.events.push(format!("start:{index}"));
                    if self.stop_after_start {
                        self.stop.store(true, Ordering::Release);
                    }
                }
                ExecutionEvent::OutputLine {
                    index,
                    stream,
                    line,
                } => self.events.push(format!(
                    "output:{index}:{stream:?}:{}",
                    String::from_utf8_lossy(line)
                )),
                ExecutionEvent::ActionFinished { index, result } => {
                    let run = &self.journal.read().unwrap()[0];
                    assert!(run.actions.iter().any(|action| action.status != "pending"));
                    self.events
                        .push(format!("finish:{index}:{:?}", result.status));
                }
                ExecutionEvent::RunFinished { summary } => {
                    assert_eq!(self.journal.read().unwrap()[0].outcome, summary.outcome);
                    self.events.push(format!("done:{}", summary.outcome));
                }
            }
        }
    }

    #[test]
    fn shared_pipeline_orders_events_stops_between_actions_and_journals_selection() {
        let fixture = tempdir().unwrap();
        let scope = fixture.path().canonicalize().unwrap();
        for project in ["one", "two"] {
            fs::create_dir_all(scope.join(project).join(".next")).unwrap();
            fs::write(scope.join(project).join("package.json"), b"{}").unwrap();
            fs::write(
                scope.join(project).join(".next/payload"),
                b"generated fixture",
            )
            .unwrap();
        }
        let observer = Arc::new(RecordingPlanObserver::default());
        let mut req = request(&scope);
        req.ignore_staleness = false;
        req.select = vec![
            scope.join("one/.next").display().to_string(),
            scope.join("two/.next").display().to_string(),
        ];
        req.selection_mode = SelectionMode::ManualOnly;
        let plan = build_reviewed_plan(req, observer.clone()).unwrap();
        assert!(
            scope.join("one/.next/payload").exists(),
            "planning must not mutate"
        );
        let phases = observer.0.lock().unwrap();
        assert!(phases[0].contains("PhaseStarted"));
        assert!(phases.last().unwrap().contains("PhaseFinished"));
        drop(phases);
        let journal = Journal::new(scope.join("state/history.jsonl"));
        let stop = AtomicBool::new(false);
        let mut recorder = Recorder {
            events: Vec::new(),
            stop: &stop,
            stop_after_start: true,
            journal: &journal,
        };
        // The same root refusal applies to this API as the CLI, even in tests.
        #[cfg(unix)]
        if unsafe { libc::geteuid() } == 0 {
            assert!(
                execute_reviewed_plan(&plan, Some(&scope), true, &stop, &mut recorder, &journal)
                    .is_err()
            );
            assert!(scope.join("one/.next").exists());
            return;
        }
        let (results, run) =
            execute_reviewed_plan(&plan, Some(&scope), true, &stop, &mut recorder, &journal)
                .unwrap();
        assert_eq!(
            recorder.events,
            [
                "run:2",
                "start:0",
                "finish:0:Success",
                "finish:1:Skipped",
                "done:partial"
            ]
        );
        assert_eq!(results[1].result.error.as_deref(), Some("stopped by user"));
        assert!(
            !scope.join("one/.next").exists(),
            "an action already started must finish despite the stop flag"
        );
        assert!(scope.join("two/.next/payload").exists());
        for action in &run.actions {
            assert_eq!(action.selection_source, "manual");
            assert_eq!(
                action.overridden_skip_reason.as_deref(),
                Some("target is not stale")
            );
        }
        assert_eq!(journal.read().unwrap()[0].actions[1].status, "skipped");
        let snapshots: Vec<JournalRun> = fs::read_to_string(journal.path())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(snapshots.len(), 4, "pending, each action, finished");
        assert!(
            snapshots[0]
                .actions
                .iter()
                .all(|action| action.status == "pending")
        );
    }

    #[test]
    #[cfg(unix)]
    fn pipeline_streams_provider_output_between_start_and_finish() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempdir().unwrap();
        let scope = fixture.path().canonicalize().unwrap();
        let executable = scope.join("fixture-provider");
        fs::write(
            &executable,
            "#!/bin/sh\nprintf 'provider reclaimed 2MiB\\n'\nprintf 'fixture stderr\\n' >&2\n",
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let mut req = request(&scope);
        req.only.clear();
        req.targets_only_cli = true;
        req.target_argv =
            vec![serde_json::to_string(&vec![executable.display().to_string()]).unwrap()];
        let baseline =
            build_reviewed_plan(req, Arc::new(RecordingPlanObserver::default())).unwrap();
        let key = baseline.targets[0].candidate_key.clone();
        let mut req = request(&scope);
        req.only.clear();
        req.targets_only_cli = true;
        req.target_argv =
            vec![serde_json::to_string(&vec![executable.display().to_string()]).unwrap()];
        req.select = vec![key.to_string()];
        req.selection_mode = SelectionMode::ManualOnly;
        let plan = build_reviewed_plan(req, Arc::new(RecordingPlanObserver::default())).unwrap();
        let journal = Journal::new(scope.join("state/history.jsonl"));
        let stop = AtomicBool::new(false);
        let mut recorder = Recorder {
            events: Vec::new(),
            stop: &stop,
            stop_after_start: false,
            journal: &journal,
        };
        if unsafe { libc::geteuid() } == 0 {
            assert!(
                execute_reviewed_plan(&plan, Some(&scope), true, &stop, &mut recorder, &journal)
                    .is_err()
            );
            return;
        }
        let (results, run) =
            execute_reviewed_plan(&plan, Some(&scope), true, &stop, &mut recorder, &journal)
                .unwrap();
        assert_eq!(&recorder.events[..2], ["run:1", "start:0"]);
        assert!(
            recorder.events[2..4].contains(&"output:0:Stdout:provider reclaimed 2MiB\n".into())
        );
        assert!(recorder.events[2..4].contains(&"output:0:Stderr:fixture stderr\n".into()));
        assert_eq!(&recorder.events[4..], ["finish:0:Success", "done:success"]);
        assert_eq!(results[0].result.provider_reported, Some(2 * 1024 * 1024));
        assert_eq!(run.actions[0].selection_source, "manual");
        assert_eq!(run.actions[0].overridden_skip_reason, None);
    }
}
