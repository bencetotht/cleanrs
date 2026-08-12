//! Declarative cleanup targets and read-only workspace detection.
//!
//! Phase 2 deliberately builds planning before execution. This module contains
//! no deletion or command-spawning API: it turns target definitions and an
//! already collected filesystem scan into inspectable candidates.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::accounting::{ReclaimEstimate, estimate_subtree};
use crate::fs::{Arena, NodeFlags, NodeIndex, NodeKind, ScanResult};

/// User-facing shorthand for a target's independent safety properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetClass {
    Free,
    Cheap,
    Network,
    Expensive,
    Irreplaceable,
}

/// How removed data can be recovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Recoverability {
    Generated,
    Downloadable,
    UserData,
}

/// Expected cost of reconstructing a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RebuildCost {
    Trivial,
    Moderate,
    High,
}

/// Whether a future execution may proceed without interactive confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Automation {
    Unattended,
    Confirmed,
    ExplicitTargetOnly,
}

/// Expanded safety policy carried by every resolved target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

/// Cleanup intensity used by the future `clean` planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupLevel {
    Safe,
    Standard,
    Aggressive,
}

impl CleanupLevel {
    /// Return whether a level may select these properties.
    ///
    /// User data is deliberately unreachable unless its stable target ID was
    /// named explicitly; increasing the level never weakens that invariant.
    #[must_use]
    pub fn selects(self, properties: TargetProperties, explicitly_named: bool) -> bool {
        if properties.recoverability == Recoverability::UserData {
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
        stale_after_days: 30,
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
    pub id: &'static str,
    pub description: &'static str,
    pub project_root: PathBuf,
    pub path: PathBuf,
    pub class: TargetClass,
    pub properties: TargetProperties,
    pub children_only: bool,
    pub apparent: u64,
    pub allocated: u64,
    pub estimate: ReclaimEstimate,
    pub age_days: Option<u64>,
    pub stale_after_days: u32,
    pub stale: Option<bool>,
    pub safety: CandidateSafety,
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
            let age_days = modified_age_days(&scan.arena, candidate, now);
            let safety = validate_candidate(&scan.arena, candidate, &project_root, home);
            let (apparent, allocated) = subtree_sizes(&scan.arena, candidate);
            detected
                .entry(path.clone())
                .or_insert_with(|| DetectedTarget {
                    id: definition.id,
                    description: definition.description,
                    project_root,
                    path,
                    class: definition.class,
                    properties: definition.properties(),
                    children_only: definition.children_only,
                    apparent,
                    allocated,
                    estimate: estimate_subtree(&scan.arena, candidate, scan.coverage_complete()),
                    age_days,
                    stale_after_days: definition.stale_after_days,
                    stale: age_days.map(|age| age >= u64::from(definition.stale_after_days)),
                    safety,
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
            .components()
            .count()
            .cmp(&right.path.components().count())
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut roots: Vec<DetectedTarget> = Vec::new();
    for candidate in candidates {
        if roots
            .iter()
            .any(|parent| candidate.path.starts_with(&parent.path))
        {
            continue;
        }
        roots.push(candidate);
    }
    roots
}

fn modified_age_days(arena: &Arena, index: NodeIndex, now: u64) -> Option<u64> {
    let modified = arena.nodes[index.as_usize()].modified_unix_seconds;
    u64::try_from(modified)
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

fn subtree_sizes(arena: &Arena, root: NodeIndex) -> (u64, u64) {
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
    fn marker_target_is_detected_and_sized() {
        let detected = detect_workspace_targets(
            &fixture_scan("/fixture", false),
            BUILTIN_TARGETS,
            Some(Path::new("/Users/test")),
        );
        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].id, "node.modules");
        assert_eq!(detected[0].allocated, 4096);
        assert_eq!(detected[0].estimate.floor, Some(4096));
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
        assert_eq!(detected[0].path, Path::new("/fixture/project/node_modules"));
    }
}
