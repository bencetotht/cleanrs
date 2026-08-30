//! Declarative cleanup targets and read-only workspace detection.
//!
//! Phase 2 deliberately builds planning before execution. This module contains
//! no deletion or command-spawning API: it turns target definitions and an
//! already collected filesystem scan into inspectable candidates.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::accounting::{Confidence, ReclaimEstimate, estimate_subtree};
use crate::fs::{Arena, NodeFlags, NodeIndex, NodeKind, ScanResult};

/// User-facing shorthand for a target's independent safety properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetClass {
    Free,
    Cheap,
    Network,
    Expensive,
    Irreplaceable,
}

/// How removed data can be recovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recoverability {
    Generated,
    Downloadable,
    UserData,
}

/// Expected cost of reconstructing a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RebuildCost {
    Trivial,
    Moderate,
    High,
}

/// Whether a future execution may proceed without interactive confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Automation {
    Unattended,
    Confirmed,
    ExplicitTargetOnly,
}

/// Expanded safety policy carried by every resolved target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetProperties {
    pub recoverability: Recoverability,
    pub rebuild_cost: Option<RebuildCost>,
    pub network_required: bool,
    pub automation: Automation,
}

impl TargetClass {
    /// Expand the human-facing class into independent policy properties.
    #[must_use]
    pub const fn properties(self) -> TargetProperties {
        match self {
            Self::Free => TargetProperties {
                recoverability: Recoverability::Generated,
                rebuild_cost: Some(RebuildCost::Trivial),
                network_required: false,
                automation: Automation::Unattended,
            },
            Self::Cheap => TargetProperties {
                recoverability: Recoverability::Generated,
                rebuild_cost: Some(RebuildCost::Moderate),
                network_required: false,
                automation: Automation::Unattended,
            },
            Self::Network => TargetProperties {
                recoverability: Recoverability::Downloadable,
                rebuild_cost: Some(RebuildCost::Moderate),
                network_required: true,
                automation: Automation::Confirmed,
            },
            Self::Expensive => TargetProperties {
                recoverability: Recoverability::Downloadable,
                rebuild_cost: Some(RebuildCost::High),
                network_required: true,
                automation: Automation::Confirmed,
            },
            Self::Irreplaceable => TargetProperties {
                recoverability: Recoverability::UserData,
                rebuild_cost: None,
                network_required: false,
                automation: Automation::ExplicitTargetOnly,
            },
        }
    }
}

/// Cleanup intensity used by the `clean` planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CleanupLevel {
    Safe,
    Standard,
    Aggressive,
}

impl CleanupLevel {
    const fn rank(self) -> u8 {
        match self {
            Self::Safe => 0,
            Self::Standard => 1,
            Self::Aggressive => 2,
        }
    }

    /// Return whether this level reaches an action's minimum severity.
    #[must_use]
    pub const fn reaches(self, minimum: Self) -> bool {
        self.rank() >= minimum.rank()
    }
}

impl CleanupLevel {
    /// Return whether a level may select these properties.
    ///
    /// User data is deliberately unreachable unless its stable target ID was
    /// named explicitly; increasing the level never weakens that invariant.
    #[must_use]
    pub fn selects(self, properties: TargetProperties, explicitly_named: bool) -> bool {
        if properties.recoverability == Recoverability::UserData
            || matches!(properties.automation, Automation::ExplicitTargetOnly)
        {
            return explicitly_named;
        }
        match self {
            Self::Safe => {
                properties.recoverability == Recoverability::Generated
                    && matches!(properties.automation, Automation::Unattended)
                    && !matches!(properties.rebuild_cost, Some(RebuildCost::High))
            }
            Self::Standard => {
                !matches!(properties.automation, Automation::ExplicitTargetOnly)
                    && !matches!(properties.rebuild_cost, Some(RebuildCost::High))
            }
            Self::Aggressive => !matches!(properties.automation, Automation::ExplicitTargetOnly),
        }
    }
}

/// A detector that recognizes a generated directory next to a project marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MarkerDetector {
    pub marker: &'static str,
    pub directory: &'static str,
}

/// A built-in declarative workspace target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TargetDefinition {
    pub id: &'static str,
    pub description: &'static str,
    pub detector: MarkerDetector,
    pub class: TargetClass,
    pub rebuild_cost_override: Option<RebuildCost>,
    pub stale_after_days: u32,
    pub children_only: bool,
}

/// Identity captured while inspecting a provider. Future execution must prove
/// that it is still talking to this same executable and provider context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderIdentity {
    pub id: String,
    pub executable: Option<PathBuf>,
    pub version: Option<String>,
    pub context: Option<String>,
}

impl ProviderIdentity {
    #[must_use]
    pub fn workspace() -> Self {
        Self {
            id: "workspace".into(),
            executable: None,
            version: None,
            context: None,
        }
    }
}

/// An external command represented without a shell or argument re-parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandSpec {
    pub executable: PathBuf,
    pub args: Vec<String>,
    /// Only non-secret, provider-controlled environment overrides belong here.
    pub environment: BTreeMap<String, String>,
}

/// Exact operation described by a Phase 2 plan. Nothing in this enum executes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum PlannedOperation {
    DirectDelete {
        path: PathBuf,
        children_only: bool,
        identity: CandidateIdentity,
    },
    Command {
        command: CommandSpec,
    },
    Informational,
}

/// Origin and semantics of a displayed usage estimate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum UsageEstimate {
    Filesystem {
        apparent: u64,
        allocated: u64,
        reclaimable: ReclaimEstimate,
    },
    ProviderReported {
        bytes: Option<u64>,
        confidence: Confidence,
        explanation: String,
    },
    Unknown {
        explanation: String,
    },
}

impl UsageEstimate {
    /// Best sortable upper value. Unknown provider usage intentionally sorts last.
    #[must_use]
    pub fn sortable_bytes(&self) -> u64 {
        match self {
            Self::Filesystem {
                allocated,
                reclaimable,
                ..
            } => reclaimable.ceiling.unwrap_or(*allocated),
            Self::ProviderReported { bytes, .. } => bytes.unwrap_or_default(),
            Self::Unknown { .. } => 0,
        }
    }
}

impl TargetDefinition {
    #[must_use]
    pub const fn properties(self) -> TargetProperties {
        let mut properties = self.class.properties();
        if let Some(cost) = self.rebuild_cost_override {
            properties.rebuild_cost = Some(cost);
        }
        properties
    }
}

/// Embedded Phase 2 workspace manifest.
///
/// Known-location and command-provider targets will extend this catalog. These
/// entries are intentionally limited to artifacts that can be proven to sit
/// strictly below a detected project root.
pub const BUILTIN_TARGETS: &[TargetDefinition] = &[
    TargetDefinition {
        id: "cargo.target",
        description: "Rust build output",
        detector: MarkerDetector {
            marker: "Cargo.toml",
            directory: "target",
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: Some(RebuildCost::High),
        stale_after_days: 14,
        children_only: false,
    },
    TargetDefinition {
        id: "node.modules",
        description: "installed Node.js dependencies",
        detector: MarkerDetector {
            marker: "package.json",
            directory: "node_modules",
        },
        class: TargetClass::Network,
        rebuild_cost_override: None,
        stale_after_days: 30,
        children_only: false,
    },
    TargetDefinition {
        id: "node.next",
        description: "Next.js generated output",
        detector: MarkerDetector {
            marker: "package.json",
            directory: ".next",
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 14,
        children_only: false,
    },
    TargetDefinition {
        id: "node.dist",
        description: "JavaScript distribution output",
        detector: MarkerDetector {
            marker: "package.json",
            directory: "dist",
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 30,
        children_only: false,
    },
];

/// Candidate safety result. Refused candidates remain visible in `targets`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", content = "reason", rename_all = "lowercase")]
pub enum CandidateSafety {
    Eligible,
    Refused(String),
}

/// A detected, sized, but never executed target candidate.
#[derive(Debug, Clone, Serialize)]
pub struct DetectedTarget {
    pub id: String,
    pub provider: ProviderIdentity,
    pub description: String,
    pub project_root: Option<PathBuf>,
    pub path: Option<PathBuf>,
    pub class: TargetClass,
    pub properties: TargetProperties,
    pub minimum_level: CleanupLevel,
    pub usage: UsageEstimate,
    pub age_days: Option<u64>,
    pub stale_after_days: u32,
    pub stale: Option<bool>,
    pub safety: CandidateSafety,
    /// Filesystem identity captured during planning for Phase 3 revalidation.
    pub operation: PlannedOperation,
    /// Actions in the same group overlap. The planner selects only the highest
    /// priority applicable member, so totals never double count alternatives.
    pub exclusive_group: Option<String>,
    pub exclusive_priority: u8,
    pub consequence: String,
    pub evidence: Vec<String>,
}

/// Stable filesystem facts that must still match immediately before deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CandidateIdentity {
    pub device: u64,
    pub inode: u64,
    pub kind: NodeKind,
}

/// Why a detected target is or is not part of a cleanup plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", content = "reason", rename_all = "snake_case")]
pub enum PlanSelection {
    Selected,
    Skipped(String),
}

/// One fully explained row in a dry-run cleanup plan.
#[derive(Debug, Clone, Serialize)]
pub struct PlannedTarget {
    #[serde(flatten)]
    pub target: DetectedTarget,
    pub selection: PlanSelection,
}

/// Select detected targets without executing any filesystem operation.
#[must_use]
pub fn plan_cleanup(
    targets: Vec<DetectedTarget>,
    level: CleanupLevel,
    explicitly_named_ids: &[String],
    ignore_staleness: bool,
) -> Vec<PlannedTarget> {
    let mut planned: Vec<_> = targets
        .into_iter()
        .map(|target| {
            let explicitly_named = explicitly_named_ids.iter().any(|id| id == &target.id);
            let selection = match &target.safety {
                CandidateSafety::Refused(reason) => {
                    PlanSelection::Skipped(format!("safety refusal: {reason}"))
                }
                CandidateSafety::Eligible
                    if matches!(&target.operation, PlannedOperation::Informational) =>
                {
                    PlanSelection::Skipped("informational only; no cleanup operation".into())
                }
                CandidateSafety::Eligible if !level.reaches(target.minimum_level) => {
                    PlanSelection::Skipped(
                        format!("requires {:?} cleanup level", target.minimum_level).to_lowercase(),
                    )
                }
                CandidateSafety::Eligible
                    if !level.selects(target.properties, explicitly_named) =>
                {
                    PlanSelection::Skipped("not selected by cleanup level".into())
                }
                CandidateSafety::Eligible if !ignore_staleness && target.stale != Some(true) => {
                    PlanSelection::Skipped(match target.stale {
                        Some(false) => "target is not stale".into(),
                        None => "target age is unknown".into(),
                        Some(true) => unreachable!("guard excludes stale targets"),
                    })
                }
                CandidateSafety::Eligible => PlanSelection::Selected,
            };
            PlannedTarget { target, selection }
        })
        .collect();

    // Resolve overlapping alternatives after ordinary policy selection. A
    // scrub action, for example, supersedes routine cleanup at aggressive level.
    let mut winners = HashMap::<String, (u8, String)>::new();
    for item in &planned {
        if !matches!(item.selection, PlanSelection::Selected) {
            continue;
        }
        let Some(group) = &item.target.exclusive_group else {
            continue;
        };
        let candidate = (item.target.exclusive_priority, item.target.id.clone());
        winners
            .entry(group.clone())
            .and_modify(|winner| {
                if candidate > *winner {
                    *winner = candidate.clone();
                }
            })
            .or_insert(candidate);
    }
    for item in &mut planned {
        let Some(group) = &item.target.exclusive_group else {
            continue;
        };
        if matches!(item.selection, PlanSelection::Selected)
            && winners
                .get(group)
                .is_some_and(|(_, id)| id != &item.target.id)
        {
            let winner = &winners[group].1;
            item.selection = PlanSelection::Skipped(format!("superseded by {winner}"));
        }
    }
    if level == CleanupLevel::Safe {
        const MAX_UNATTENDED_TARGETS: usize = 100;
        const MAX_UNATTENDED_BYTES: u64 = 50 * 1024 * 1024 * 1024;
        let selected_count = planned
            .iter()
            .filter(|item| matches!(item.selection, PlanSelection::Selected))
            .count();
        let selected_bytes = planned
            .iter()
            .filter(|item| matches!(item.selection, PlanSelection::Selected))
            .map(|item| item.target.usage.sortable_bytes())
            .fold(0_u64, u64::saturating_add);
        if selected_count > MAX_UNATTENDED_TARGETS || selected_bytes > MAX_UNATTENDED_BYTES {
            for item in &mut planned {
                if matches!(item.selection, PlanSelection::Selected) {
                    item.selection = PlanSelection::Skipped(
                        "unattended plan exceeds the conservative count/byte limit".into(),
                    );
                }
            }
        }
    }
    planned
}

/// Detect all selected built-in workspace targets from one explicit scan.
#[must_use]
pub fn detect_workspace_targets(
    scan: &ScanResult,
    definitions: &[TargetDefinition],
    home: Option<&Path>,
) -> Vec<DetectedTarget> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let mut children_by_name: HashMap<NodeIndex, HashMap<&str, NodeIndex>> = HashMap::new();
    for index in (0..scan.arena.nodes.len()).map(NodeIndex::from_usize) {
        if scan.arena.nodes[index.as_usize()].kind != NodeKind::Directory {
            continue;
        }
        children_by_name.insert(
            index,
            scan.arena
                .children(index)
                .map(|child| (scan.arena.name(child), child))
                .collect(),
        );
    }

    // A stable path key prevents duplicate actions when overlapping definitions
    // resolve to the same artifact directory.
    let mut detected = BTreeMap::<PathBuf, DetectedTarget>::new();
    for definition in definitions {
        for (&project, children) in &children_by_name {
            if !children.contains_key(definition.detector.marker) {
                continue;
            }
            let Some(&candidate) = children.get(definition.detector.directory) else {
                continue;
            };
            if scan.arena.nodes[candidate.as_usize()].kind != NodeKind::Directory {
                continue;
            }

            let project_root = scan.arena.path(project);
            let path = scan.arena.path(candidate);
            let age_days = if definition.id == "cargo.target" {
                cargo_activity_age_days(
                    &scan.arena,
                    candidate,
                    children[definition.detector.marker],
                    now,
                )
            } else {
                modified_age_days(&scan.arena, candidate, now)
            };
            let safety = validate_candidate(&scan.arena, candidate, &project_root, home);
            let (apparent, allocated) = subtree_sizes(&scan.arena, candidate);
            detected
                .entry(path.clone())
                .or_insert_with(|| DetectedTarget {
                    id: definition.id.into(),
                    provider: ProviderIdentity::workspace(),
                    description: definition.description.into(),
                    project_root: Some(project_root),
                    path: Some(path.clone()),
                    class: definition.class,
                    properties: definition.properties(),
                    minimum_level: minimum_level_for(definition.properties()),
                    usage: UsageEstimate::Filesystem {
                        apparent,
                        allocated,
                        reclaimable: estimate_subtree(
                            &scan.arena,
                            candidate,
                            scan.coverage_complete(),
                        ),
                    },
                    age_days,
                    stale_after_days: definition.stale_after_days,
                    stale: age_days.map(|age| age >= u64::from(definition.stale_after_days)),
                    safety,
                    operation: PlannedOperation::DirectDelete {
                        path,
                        children_only: definition.children_only,
                        identity: CandidateIdentity {
                            device: scan.arena.nodes[candidate.as_usize()].device,
                            inode: scan.arena.nodes[candidate.as_usize()].inode,
                            kind: scan.arena.nodes[candidate.as_usize()].kind,
                        },
                    },
                    exclusive_group: None,
                    exclusive_priority: 0,
                    consequence: workspace_consequence(definition).into(),
                    evidence: if definition.id == "cargo.target" {
                        vec![
                            "detected target beside Cargo.toml".into(),
                            "age uses the newest Cargo.toml or build-artifact modification".into(),
                        ]
                    } else {
                        vec![format!(
                            "detected {} beside {}",
                            definition.detector.directory, definition.detector.marker
                        )]
                    },
                });
        }
    }

    // Package trees commonly contain their own markers (`node_modules/foo` has
    // a package.json). Once an outer artifact directory is a candidate, every
    // candidate below it is part of the same prospective action and must not be
    // printed or counted again.
    let mut candidates: Vec<_> = detected.into_values().collect();
    candidates.sort_by(|left, right| {
        left.path
            .as_ref()
            .map_or(0, |path| path.components().count())
            .cmp(
                &right
                    .path
                    .as_ref()
                    .map_or(0, |path| path.components().count()),
            )
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut roots: Vec<DetectedTarget> = Vec::new();
    for candidate in candidates {
        if roots.iter().any(|parent| {
            candidate.path.as_ref().is_some_and(|candidate_path| {
                parent
                    .path
                    .as_ref()
                    .is_some_and(|parent_path| candidate_path.starts_with(parent_path))
            })
        }) {
            continue;
        }
        roots.push(candidate);
    }
    roots
}

/// Resolve one explicitly configured path against the immutable scan arena.
/// Paths outside the explicit scan remain visible but refused.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn detect_explicit_path_target(
    scan: &ScanResult,
    id: String,
    description: String,
    path: PathBuf,
    class: TargetClass,
    minimum_level: CleanupLevel,
    stale_after_days: u32,
    children_only: bool,
    home: Option<&Path>,
) -> DetectedTarget {
    let index = (0..scan.arena.nodes.len())
        .map(NodeIndex::from_usize)
        .find(|index| scan.arena.path(*index) == path);
    let properties = class.properties();
    let (usage, age_days, safety, operation) = if let Some(index) = index {
        let (apparent, allocated) = subtree_sizes(&scan.arena, index);
        let age_days = modified_age_days(&scan.arena, index, now_seconds());
        let mut safety = validate_scanned_candidate(&scan.arena, index, home);
        if path == scan.root {
            safety = CandidateSafety::Refused(
                "configured cleanup path cannot equal the explicit scan root".into(),
            );
        }
        (
            UsageEstimate::Filesystem {
                apparent,
                allocated,
                reclaimable: estimate_subtree(&scan.arena, index, scan.coverage_complete()),
            },
            age_days,
            safety,
            PlannedOperation::DirectDelete {
                path: path.clone(),
                children_only,
                identity: CandidateIdentity {
                    device: scan.arena.nodes[index.as_usize()].device,
                    inode: scan.arena.nodes[index.as_usize()].inode,
                    kind: scan.arena.nodes[index.as_usize()].kind,
                },
            },
        )
    } else {
        (
            UsageEstimate::Unknown {
                explanation: "configured path is outside the explicit scan scope".into(),
            },
            None,
            CandidateSafety::Refused("configured path is outside the explicit scan scope".into()),
            PlannedOperation::Informational,
        )
    };
    DetectedTarget {
        id,
        provider: ProviderIdentity {
            id: "configuration".into(),
            executable: None,
            version: None,
            context: Some("resolved target registry".into()),
        },
        description,
        project_root: path.parent().map(Path::to_path_buf),
        path: Some(path.clone()),
        class,
        properties,
        minimum_level,
        usage,
        age_days,
        stale_after_days,
        stale: age_days.map(|age| age >= u64::from(stale_after_days)),
        safety,
        operation,
        exclusive_group: None,
        exclusive_priority: 0,
        consequence: "configured generated/downloadable data will need to be restored".into(),
        evidence: vec![format!("resolved configured path: {}", path.display())],
    }
}

fn minimum_level_for(properties: TargetProperties) -> CleanupLevel {
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

fn workspace_consequence(definition: &TargetDefinition) -> &'static str {
    match definition.id {
        "cargo.target" => "Rust build output will be rebuilt on the next build",
        "node.modules" => "dependencies must be restored by the package manager",
        "node.next" => "Next.js output will be regenerated on the next build",
        "node.dist" => "distribution output will be regenerated by its build",
        _ => "generated workspace output will need to be recreated",
    }
}

fn modified_age_days(arena: &Arena, index: NodeIndex, now: u64) -> Option<u64> {
    let modified = arena.nodes[index.as_usize()].modified_unix_seconds;
    u64::try_from(modified)
        .ok()
        .filter(|modified| *modified <= now)
        .map(|modified| (now - modified) / 86_400)
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

pub(crate) fn cargo_activity_age_days(
    arena: &Arena,
    target: NodeIndex,
    manifest: NodeIndex,
    now: u64,
) -> Option<u64> {
    let mut newest = arena.nodes[manifest.as_usize()].modified_unix_seconds;
    let mut stack = vec![target];
    while let Some(index) = stack.pop() {
        newest = newest.max(arena.nodes[index.as_usize()].modified_unix_seconds);
        stack.extend(arena.children(index));
    }
    u64::try_from(newest)
        .ok()
        .filter(|modified| *modified <= now)
        .map(|modified| (now - modified) / 86_400)
}

fn validate_candidate(
    arena: &Arena,
    candidate: NodeIndex,
    project_root: &Path,
    home: Option<&Path>,
) -> CandidateSafety {
    let path = arena.path(candidate);
    // Marker output must be a strict descendant. This is checked on the
    // normalized scan paths, not inferred from string concatenation.
    if path == project_root || !path.starts_with(project_root) {
        return CandidateSafety::Refused(
            "artifact path is not strictly beneath its detected project root".into(),
        );
    }
    if path.parent() != Some(project_root) {
        return CandidateSafety::Refused(
            "artifact normalization escaped the detected project root".into(),
        );
    }
    if path == Path::new("/") || home.is_some_and(|home| path == home) {
        return CandidateSafety::Refused("protected filesystem root".into());
    }
    if let Some(home) = home {
        for denied in [
            home.join("Library/Mobile Documents"),
            home.join("Library/CloudStorage"),
        ] {
            if path.starts_with(&denied) {
                return CandidateSafety::Refused("path is inside synced cloud storage".into());
            }
        }
    }

    validate_scanned_candidate(arena, candidate, home)
}

pub(crate) fn validate_scanned_candidate(
    arena: &Arena,
    candidate: NodeIndex,
    home: Option<&Path>,
) -> CandidateSafety {
    let path = arena.path(candidate);
    if path == Path::new("/") || home.is_some_and(|home| path == home) {
        return CandidateSafety::Refused("protected filesystem root".into());
    }
    if let Some(home) = home {
        for denied in [
            home.join("Library/Mobile Documents"),
            home.join("Library/CloudStorage"),
        ] {
            if path.starts_with(&denied) {
                return CandidateSafety::Refused("path is inside synced cloud storage".into());
            }
        }
    }
    let mut stack = vec![candidate];
    while let Some(index) = stack.pop() {
        let node = &arena.nodes[index.as_usize()];
        if node.flags.contains(NodeFlags::DATALESS) {
            return CandidateSafety::Refused(
                "candidate contains dataless or ubiquitous content".into(),
            );
        }
        if node.flags.contains(NodeFlags::MOUNT_BOUNDARY) {
            return CandidateSafety::Refused("candidate crosses a mount boundary".into());
        }
        stack.extend(arena.children(index));
    }
    CandidateSafety::Eligible
}

pub(crate) fn subtree_sizes(arena: &Arena, root: NodeIndex) -> (u64, u64) {
    use std::collections::HashSet;
    let mut stack = vec![root];
    let mut apparent = 0_u64;
    let mut allocated = 0_u64;
    let mut inodes = HashSet::new();
    while let Some(index) = stack.pop() {
        let node = &arena.nodes[index.as_usize()];
        stack.extend(arena.children(index));
        if node.kind.is_file_like() {
            apparent = apparent.saturating_add(node.apparent_size);
            if inodes.insert((node.device, node.inode)) {
                allocated = allocated.saturating_add(node.allocated_size);
            }
        }
    }
    (apparent, allocated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{Node, ScanResult};

    fn fixture_scan(root: &str, dataless: bool) -> ScanResult {
        let mut arena = Arena::new(root);
        let project = arena.push_child(
            arena.root(),
            "project",
            Node {
                kind: NodeKind::Directory,
                ..Node::default()
            },
        );
        arena.push_child(
            project,
            "package.json",
            Node {
                kind: NodeKind::File,
                ..Node::default()
            },
        );
        let modules = arena.push_child(
            project,
            "node_modules",
            Node {
                kind: NodeKind::Directory,
                modified_unix_seconds: 1,
                ..Node::default()
            },
        );
        arena.push_child(
            modules,
            "package.js",
            Node {
                kind: NodeKind::File,
                device: 1,
                inode: 9,
                apparent_size: 100,
                allocated_size: 4096,
                private_size: Some(4096),
                flags: if dataless {
                    NodeFlags::DATALESS
                } else {
                    NodeFlags::empty()
                },
                ..Node::default()
            },
        );
        ScanResult {
            root: PathBuf::from(root),
            arena,
            coverage_errors: Vec::new(),
            dataless_entries: usize::from(dataless),
            mount_boundaries: 0,
            backend: "fixture".into(),
        }
    }

    #[test]
    fn classes_expand_to_the_documented_properties() {
        assert_eq!(
            TargetClass::Irreplaceable.properties().recoverability,
            Recoverability::UserData
        );
        assert!(TargetClass::Network.properties().network_required);
        assert_eq!(
            TargetClass::Free.properties().automation,
            Automation::Unattended
        );
    }

    #[test]
    fn no_level_selects_user_data_without_explicit_naming() {
        let protected = TargetClass::Irreplaceable.properties();
        for level in [
            CleanupLevel::Safe,
            CleanupLevel::Standard,
            CleanupLevel::Aggressive,
        ] {
            assert!(!level.selects(protected, false));
            assert!(level.selects(protected, true));
        }
    }

    #[test]
    fn explicit_only_generated_data_requires_its_stable_id() {
        let mut protected = TargetClass::Cheap.properties();
        protected.automation = Automation::ExplicitTargetOnly;
        for level in [
            CleanupLevel::Safe,
            CleanupLevel::Standard,
            CleanupLevel::Aggressive,
        ] {
            assert!(!level.selects(protected, false));
            assert!(level.selects(protected, true));
        }
    }

    #[test]
    fn planner_explains_level_and_staleness_selection() {
        let targets = detect_workspace_targets(
            &fixture_scan("/fixture", false),
            BUILTIN_TARGETS,
            Some(Path::new("/Users/test")),
        );
        let safe = plan_cleanup(targets.clone(), CleanupLevel::Safe, &[], true);
        assert!(matches!(
            &safe[0].selection,
            PlanSelection::Skipped(reason) if reason.contains("cleanup level")
        ));

        let standard = plan_cleanup(targets, CleanupLevel::Standard, &[], true);
        assert_eq!(standard[0].selection, PlanSelection::Selected);
    }

    #[test]
    fn safe_plans_refuse_to_cross_the_unattended_count_limit() {
        let mut template = detect_workspace_targets(
            &fixture_scan("/fixture", false),
            BUILTIN_TARGETS,
            Some(Path::new("/Users/test")),
        )
        .remove(0);
        template.class = TargetClass::Free;
        template.properties = TargetClass::Free.properties();
        template.minimum_level = CleanupLevel::Safe;
        template.stale = Some(true);
        let targets = (0..101)
            .map(|index| {
                let mut target = template.clone();
                target.id = format!("fixture.{index}");
                target
            })
            .collect();

        let planned = plan_cleanup(targets, CleanupLevel::Safe, &[], false);
        assert!(planned.iter().all(|item| matches!(
            &item.selection,
            PlanSelection::Skipped(reason) if reason.contains("count/byte limit")
        )));
    }

    #[test]
    fn marker_target_is_detected_and_sized() {
        let detected = detect_workspace_targets(
            &fixture_scan("/fixture", false),
            BUILTIN_TARGETS,
            Some(Path::new("/Users/test")),
        );
        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].id, "node.modules");
        let UsageEstimate::Filesystem {
            allocated,
            reclaimable,
            ..
        } = &detected[0].usage
        else {
            panic!("workspace target must carry filesystem usage")
        };
        assert_eq!(*allocated, 4096);
        assert_eq!(reclaimable.floor, Some(4096));
        assert_eq!(detected[0].safety, CandidateSafety::Eligible);
    }

    #[test]
    fn dataless_descendant_refuses_an_ordinary_marker_match() {
        let detected = detect_workspace_targets(
            &fixture_scan("/fixture", true),
            BUILTIN_TARGETS,
            Some(Path::new("/Users/test")),
        );
        assert!(matches!(
            &detected[0].safety,
            CandidateSafety::Refused(reason) if reason.contains("dataless")
        ));
    }

    #[test]
    fn known_cloud_location_is_refused_even_without_dataless_metadata() {
        let detected = detect_workspace_targets(
            &fixture_scan("/Users/test/Library/CloudStorage/provider", false),
            BUILTIN_TARGETS,
            Some(Path::new("/Users/test")),
        );
        assert!(matches!(
            &detected[0].safety,
            CandidateSafety::Refused(reason) if reason.contains("synced cloud")
        ));
    }

    #[test]
    fn targets_nested_below_an_existing_candidate_are_collapsed() {
        let mut scan = fixture_scan("/fixture", false);
        let modules = (0..scan.arena.nodes.len())
            .map(NodeIndex::from_usize)
            .find(|index| scan.arena.name(*index) == "node_modules")
            .unwrap();
        let dependency = scan.arena.push_child(
            modules,
            "dependency",
            Node {
                kind: NodeKind::Directory,
                ..Node::default()
            },
        );
        scan.arena.push_child(
            dependency,
            "package.json",
            Node {
                kind: NodeKind::File,
                ..Node::default()
            },
        );
        scan.arena.push_child(
            dependency,
            "dist",
            Node {
                kind: NodeKind::Directory,
                ..Node::default()
            },
        );

        let detected = detect_workspace_targets(&scan, BUILTIN_TARGETS, None);
        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].id, "node.modules");
        assert_eq!(
            detected[0].path.as_deref(),
            Some(Path::new("/fixture/project/node_modules"))
        );
    }

    #[test]
    fn planner_selects_only_one_overlapping_action() {
        let mut targets =
            detect_workspace_targets(&fixture_scan("/fixture", false), BUILTIN_TARGETS, None);
        let mut alternative = targets[0].clone();
        targets[0].id = "mode.normal".into();
        targets[0].exclusive_group = Some("mode".into());
        targets[0].exclusive_priority = 10;
        targets[0].minimum_level = CleanupLevel::Safe;
        alternative.id = "mode.broad".into();
        alternative.exclusive_group = Some("mode".into());
        alternative.exclusive_priority = 20;
        alternative.minimum_level = CleanupLevel::Safe;
        targets.push(alternative);

        let planned = plan_cleanup(targets, CleanupLevel::Aggressive, &[], true);
        assert_eq!(
            planned
                .iter()
                .filter(|item| matches!(item.selection, PlanSelection::Selected))
                .count(),
            1
        );
        assert!(planned.iter().any(|item| {
            matches!(
                &item.selection,
                PlanSelection::Skipped(reason) if reason == "superseded by mode.broad"
            )
        }));
    }
}
