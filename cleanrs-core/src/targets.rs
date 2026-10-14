//! Declarative cleanup targets and read-only workspace detection.
//!
//! This module turns target definitions and a collected filesystem scan into
//! inspectable candidates. Mutation remains isolated in the execution module.

use std::collections::{BTreeMap, HashMap, HashSet};
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

/// Whether execution may proceed without interactive confirmation.
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
    pub markers: &'static [&'static str],
    pub directory: &'static str,
    pub inner_marker: Option<&'static str>,
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
    pub requires_gitignore: bool,
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

/// Exact operation described by a plan. This data is inert by itself.
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
        id: "project.build",
        description: "gitignored generated build output",
        detector: MarkerDetector {
            marker: "",
            markers: &[],
            directory: "build",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 30,
        children_only: false,
        requires_gitignore: true,
    },
    TargetDefinition {
        id: "project.out",
        description: "gitignored generated build output",
        detector: MarkerDetector {
            marker: "",
            markers: &[],
            directory: "out",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 30,
        children_only: false,
        requires_gitignore: true,
    },
    TargetDefinition {
        id: "project.coverage",
        description: "gitignored generated build output",
        detector: MarkerDetector {
            marker: "",
            markers: &[],
            directory: "coverage",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 30,
        children_only: false,
        requires_gitignore: true,
    },
    TargetDefinition {
        id: "react-native.ios-build",
        description: "gitignored generated build output",
        detector: MarkerDetector {
            marker: "",
            markers: &[],
            directory: "ios/build",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 30,
        children_only: false,
        requires_gitignore: true,
    },
    TargetDefinition {
        id: "react-native.android-build",
        description: "gitignored generated build output",
        detector: MarkerDetector {
            marker: "",
            markers: &[],
            directory: "android/build",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 30,
        children_only: false,
        requires_gitignore: true,
    },
    TargetDefinition {
        id: "react-native.android-app-build",
        description: "gitignored generated build output",
        detector: MarkerDetector {
            marker: "",
            markers: &[],
            directory: "android/app/build",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 30,
        children_only: false,
        requires_gitignore: true,
    },
    TargetDefinition {
        id: "cargo.target",
        description: "Rust build output",
        detector: MarkerDetector {
            marker: "",
            markers: &["Cargo.toml"],
            directory: "target",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: Some(RebuildCost::High),
        stale_after_days: 14,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "node.modules",
        description: "installed Node.js dependencies",
        detector: MarkerDetector {
            marker: "",
            markers: &["package.json"],
            directory: "node_modules",
            inner_marker: None,
        },
        class: TargetClass::Network,
        rebuild_cost_override: None,
        stale_after_days: 30,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "node.next",
        description: "Next.js generated output",
        detector: MarkerDetector {
            marker: "",
            markers: &["package.json"],
            directory: ".next",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 14,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "node.dist",
        description: "JavaScript distribution output",
        detector: MarkerDetector {
            marker: "",
            markers: &["package.json"],
            directory: "dist",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 30,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "python.venv",
        description: "Python virtual environment (may include manually installed packages)",
        detector: MarkerDetector {
            marker: "",
            markers: &[],
            directory: "",
            inner_marker: Some("pyvenv.cfg"),
        },
        class: TargetClass::Network,
        rebuild_cost_override: None,
        stale_after_days: 30,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "python.bytecode",
        description: "Python generated tool cache",
        detector: MarkerDetector {
            marker: "",
            markers: &[],
            directory: "__pycache__",
            inner_marker: None,
        },
        class: TargetClass::Free,
        rebuild_cost_override: None,
        stale_after_days: 14,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "python.pytest",
        description: "Python generated tool cache",
        detector: MarkerDetector {
            marker: "",
            markers: &[],
            directory: ".pytest_cache",
            inner_marker: None,
        },
        class: TargetClass::Free,
        rebuild_cost_override: None,
        stale_after_days: 14,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "python.mypy",
        description: "Python generated tool cache",
        detector: MarkerDetector {
            marker: "",
            markers: &[],
            directory: ".mypy_cache",
            inner_marker: None,
        },
        class: TargetClass::Free,
        rebuild_cost_override: None,
        stale_after_days: 14,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "python.ruff",
        description: "Python generated tool cache",
        detector: MarkerDetector {
            marker: "",
            markers: &[],
            directory: ".ruff_cache",
            inner_marker: None,
        },
        class: TargetClass::Free,
        rebuild_cost_override: None,
        stale_after_days: 14,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "python.tox",
        description: "Python test environment cache",
        detector: MarkerDetector {
            marker: "",
            markers: &[],
            directory: ".tox",
            inner_marker: None,
        },
        class: TargetClass::Network,
        rebuild_cost_override: None,
        stale_after_days: 30,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "python.nox",
        description: "Python test environment cache",
        detector: MarkerDetector {
            marker: "",
            markers: &[],
            directory: ".nox",
            inner_marker: None,
        },
        class: TargetClass::Network,
        rebuild_cost_override: None,
        stale_after_days: 30,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "node.turbo",
        description: "generated JS/TS framework cache",
        detector: MarkerDetector {
            marker: "",
            markers: &["package.json"],
            directory: ".turbo",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 14,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "node.nuxt",
        description: "generated JS/TS framework cache",
        detector: MarkerDetector {
            marker: "",
            markers: &["package.json"],
            directory: ".nuxt",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 14,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "node.svelte-kit",
        description: "generated JS/TS framework cache",
        detector: MarkerDetector {
            marker: "",
            markers: &["package.json"],
            directory: ".svelte-kit",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 14,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "node.parcel",
        description: "generated JS/TS framework cache",
        detector: MarkerDetector {
            marker: "",
            markers: &["package.json"],
            directory: ".parcel-cache",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 14,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "node.angular",
        description: "generated JS/TS framework cache",
        detector: MarkerDetector {
            marker: "",
            markers: &["package.json"],
            directory: ".angular/cache",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 14,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "node.cache",
        description: "generated JS/TS framework cache",
        detector: MarkerDetector {
            marker: "",
            markers: &["package.json"],
            directory: "node_modules/.cache",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 14,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "expo.cache",
        description: "Expo project-local generated state",
        detector: MarkerDetector {
            marker: "",
            markers: &["package.json"],
            directory: ".expo",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 14,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "cocoapods.project-pods",
        description: "project-local CocoaPods dependencies",
        detector: MarkerDetector {
            marker: "",
            markers: &["ios/Podfile"],
            directory: "ios/Pods",
            inner_marker: None,
        },
        class: TargetClass::Network,
        rebuild_cost_override: None,
        stale_after_days: 30,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "android.gradle",
        description: "Android project-local Gradle state",
        detector: MarkerDetector {
            marker: "",
            markers: &[
                "android/build.gradle",
                "android/build.gradle.kts",
                "android/settings.gradle",
                "android/settings.gradle.kts",
            ],
            directory: "android/.gradle",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 14,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "android.cxx",
        description: "Android native build intermediates",
        detector: MarkerDetector {
            marker: "",
            markers: &["android/build.gradle", "android/build.gradle.kts"],
            directory: "android/app/.cxx",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 14,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "gradle.cache",
        description: "project-local Gradle state",
        detector: MarkerDetector {
            marker: "",
            markers: &[
                "settings.gradle",
                "settings.gradle.kts",
                "build.gradle",
                "build.gradle.kts",
            ],
            directory: ".gradle",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 14,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "dart.tool",
        description: "Dart/Flutter generated tool state",
        detector: MarkerDetector {
            marker: "",
            markers: &["pubspec.yaml"],
            directory: ".dart_tool",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 14,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "zig.cache",
        description: "Zig generated cache",
        detector: MarkerDetector {
            marker: "",
            markers: &["build.zig"],
            directory: ".zig-cache",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 14,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "zig.cache-modern",
        description: "Zig generated cache",
        detector: MarkerDetector {
            marker: "",
            markers: &["build.zig"],
            directory: "zig-cache",
            inner_marker: None,
        },
        class: TargetClass::Cheap,
        rebuild_cost_override: None,
        stale_after_days: 14,
        children_only: false,
        requires_gitignore: false,
    },
    TargetDefinition {
        id: "terraform.providers",
        description: "Terraform downloaded providers and modules",
        detector: MarkerDetector {
            marker: "",
            markers: &[],
            directory: ".terraform",
            inner_marker: None,
        },
        class: TargetClass::Network,
        rebuild_cost_override: None,
        stale_after_days: 30,
        children_only: false,
        requires_gitignore: false,
    },
];

/// Candidate safety result. Refused candidates remain visible in `targets`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", content = "reason", rename_all = "lowercase")]
pub enum CandidateSafety {
    Eligible,
    Refused(String),
}

/// A detected and sized target candidate.
#[derive(Debug, Clone, Serialize)]
pub struct DetectedTarget {
    pub gitignore: Option<GitIgnoreEvidence>,
    /// Structured Cargo ownership from metadata inspection; incomplete shared
    /// ownership is evidence only and never authorizes cleanup.
    pub cargo_ownership: Option<CargoOwnership>,
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
    /// Filesystem identity captured during planning for immediate revalidation.
    pub operation: PlannedOperation,
    /// Actions in the same group overlap. The planner selects only the highest
    /// priority applicable member, so totals never double count alternatives.
    pub exclusive_group: Option<String>,
    pub exclusive_priority: u8,
    pub consequence: String,
    pub evidence: Vec<String>,
}

/// Evidence required for ambiguous folder names, rechecked before deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitIgnoreEvidence {
    pub repository: PathBuf,
    pub relative_path: PathBuf,
    pub pattern: String,
}

/// Owners observed during a bounded, explicit-root Cargo inspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CargoOwnership {
    pub target_directory: PathBuf,
    pub observed_owners: Vec<PathBuf>,
    pub complete: bool,
}

/// Structured policy explanation for presentation. The planner and UI share
/// this decision; presentation never infers eligibility from string messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyStatus<'a> {
    Eligible,
    Refused(&'a str),
    Informational,
    NeedsLevel(CleanupLevel),
    ExplicitOnly,
    NotSelectedByLevel,
    NotStale(u32),
    UnknownAge,
    Skipped(&'a str),
}

impl PolicyStatus<'_> {
    fn reason(&self) -> String {
        match self {
            Self::Eligible => "eligible".into(),
            Self::Refused(reason) => format!("safety refusal: {reason}"),
            Self::Informational => "informational only; no cleanup operation".into(),
            Self::NeedsLevel(level) => format!("requires {level:?} cleanup level").to_lowercase(),
            Self::ExplicitOnly | Self::NotSelectedByLevel => "not selected by cleanup level".into(),
            Self::NotStale(_) => "target is not stale".into(),
            Self::UnknownAge => "target age is unknown".into(),
            Self::Skipped(reason) => (*reason).into(),
        }
    }
}

impl DetectedTarget {
    /// Safety and informational actions are untickable even with explicit naming.
    #[must_use]
    pub fn manually_selectable(&self) -> bool {
        matches!(self.safety, CandidateSafety::Eligible)
            && !matches!(self.operation, PlannedOperation::Informational)
    }

    #[must_use]
    pub fn explicit_only(&self) -> bool {
        self.properties.recoverability == Recoverability::UserData
            || matches!(self.properties.automation, Automation::ExplicitTargetOnly)
    }

    #[must_use]
    pub fn policy_status(
        &self,
        level: CleanupLevel,
        explicitly_named: bool,
        ignore_staleness: bool,
    ) -> PolicyStatus<'_> {
        match &self.safety {
            CandidateSafety::Refused(reason) => PolicyStatus::Refused(reason),
            CandidateSafety::Eligible
                if matches!(self.operation, PlannedOperation::Informational) =>
            {
                PolicyStatus::Informational
            }
            CandidateSafety::Eligible if !level.reaches(self.minimum_level) => {
                PolicyStatus::NeedsLevel(self.minimum_level)
            }
            CandidateSafety::Eligible if !level.selects(self.properties, explicitly_named) => {
                if self.explicit_only() {
                    PolicyStatus::ExplicitOnly
                } else {
                    PolicyStatus::NotSelectedByLevel
                }
            }
            CandidateSafety::Eligible if !ignore_staleness && self.stale != Some(true) => {
                if self.stale == Some(false) {
                    PolicyStatus::NotStale(self.stale_after_days)
                } else {
                    PolicyStatus::UnknownAge
                }
            }
            CandidateSafety::Eligible => PolicyStatus::Eligible,
        }
    }
}

/// Stable filesystem facts that must still match immediately before deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CandidateIdentity {
    pub device: u64,
    pub inode: u64,
    pub kind: NodeKind,
}

/// Stable, printable identity for a detected candidate. The JSON tuple encoding
/// avoids delimiter ambiguity in paths, provider contexts, and exact argv.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CandidateKey(String);

impl CandidateKey {
    #[must_use]
    pub fn from_target(target: &DetectedTarget) -> Self {
        let value = if let Some(path) = &target.path {
            // Inspected paths are already absolute; refused missing paths keep
            // their inspected spelling so they can still be explained.
            serde_json::to_string(&(
                &target.id,
                path.canonicalize().unwrap_or_else(|_| path.clone()),
            ))
        } else {
            let command = match &target.operation {
                PlannedOperation::Command { command } => Some(command),
                _ => None,
            };
            serde_json::to_string(&(&target.id, &target.provider, command))
        };
        Self(value.expect("candidate identity is serializable"))
    }
}

impl std::fmt::Display for CandidateKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Manual picks are explicit naming, but never override a safety refusal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SelectionSource {
    Level,
    Manual { overrode: Option<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionMode {
    LevelPlusManual,
    ManualOnly,
}

/// Why a detected target is or is not part of a cleanup plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanSelection {
    Selected { source: SelectionSource },
    Skipped(String),
}

// Preserve the existing status/reason JSON contract, adding source only for
// selected rows. Existing consumers can continue testing status == selected.
impl Serialize for PlanSelection {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(2))?;
        match self {
            Self::Selected { source } => {
                map.serialize_entry("status", "selected")?;
                map.serialize_entry("source", source)?;
            }
            Self::Skipped(reason) => {
                map.serialize_entry("status", "skipped")?;
                map.serialize_entry("reason", reason)?;
            }
        }
        map.end()
    }
}

/// A manual request rejected before a reviewed plan can be constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionError(pub String);

impl std::fmt::Display for SelectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::error::Error for SelectionError {}

/// One fully explained row in a dry-run cleanup plan.
#[derive(Debug, Clone, Serialize)]
pub struct PlannedTarget {
    pub candidate_key: CandidateKey,
    #[serde(flatten)]
    pub target: DetectedTarget,
    pub selection: PlanSelection,
}

impl PlannedTarget {
    #[must_use]
    pub fn policy_status(&self, level: CleanupLevel) -> PolicyStatus<'_> {
        let status = self.target.policy_status(level, false, false);
        if status == PolicyStatus::Eligible {
            if let PlanSelection::Skipped(reason) = &self.selection {
                return PolicyStatus::Skipped(reason);
            }
        }
        status
    }
}

/// Select detected targets without executing any filesystem operation.
#[must_use]
pub fn plan_cleanup(
    targets: Vec<DetectedTarget>,
    level: CleanupLevel,
    explicitly_named_ids: &[String],
    ignore_staleness: bool,
) -> Vec<PlannedTarget> {
    plan_cleanup_with_selection(
        targets,
        level,
        explicitly_named_ids,
        ignore_staleness,
        &HashSet::new(),
        SelectionMode::LevelPlusManual,
    )
    .expect("level-only selection cannot be rejected")
}

/// The ordinary severity/staleness decision for one candidate, before
/// exclusive alternatives, safe-plan limits, or manual choices are applied.
fn policy_selection(
    target: &DetectedTarget,
    level: CleanupLevel,
    explicitly_named_ids: &[String],
    ignore_staleness: bool,
) -> PlanSelection {
    match target.policy_status(
        level,
        explicitly_named_ids.iter().any(|id| id == &target.id),
        ignore_staleness,
    ) {
        PolicyStatus::Eligible => PlanSelection::Selected {
            source: SelectionSource::Level,
        },
        status => PlanSelection::Skipped(status.reason()),
    }
}

/// Apply per-candidate choices before resolving exclusive alternatives. Manual
/// choices supersede policy only; refused/informational rows remain untouchable.
pub fn plan_cleanup_with_selection(
    targets: Vec<DetectedTarget>,
    level: CleanupLevel,
    explicitly_named_ids: &[String],
    ignore_staleness: bool,
    manual: &HashSet<CandidateKey>,
    mode: SelectionMode,
) -> Result<Vec<PlannedTarget>, SelectionError> {
    // Capture the ordinary plan first, including exclusive alternatives and
    // conservative safe limits, so a manual override explains the actual
    // decision the user would have seen without the explicit choice.
    let mut planned: Vec<_> = if manual.is_empty() {
        targets
            .into_iter()
            .map(|target| {
                let selection =
                    policy_selection(&target, level, explicitly_named_ids, ignore_staleness);
                PlannedTarget {
                    candidate_key: CandidateKey::from_target(&target),
                    target,
                    selection,
                }
            })
            .collect()
    } else {
        // Recursing with no manual picks yields the complete level-only plan.
        plan_cleanup(targets, level, explicitly_named_ids, ignore_staleness)
    };
    let keys: HashSet<_> = planned.iter().map(|item| &item.candidate_key).collect();
    if let Some(key) = manual.iter().find(|key| !keys.contains(key)) {
        return Err(SelectionError(format!("unknown candidate: {key}")));
    }

    let mut manual_groups = HashMap::<String, usize>::new();
    for index in 0..planned.len() {
        let item = &mut planned[index];
        let key = item.candidate_key.clone();
        if manual.contains(&key) {
            if let CandidateSafety::Refused(reason) = &item.target.safety {
                return Err(SelectionError(format!(
                    "cannot select {key}: safety refusal: {reason}"
                )));
            }
            if matches!(item.target.operation, PlannedOperation::Informational) {
                return Err(SelectionError(format!(
                    "cannot select {key}: informational only; no cleanup operation"
                )));
            }
            let overrode = match &item.selection {
                PlanSelection::Skipped(reason) => Some(reason.clone()),
                _ => None,
            };
            item.selection = PlanSelection::Selected {
                source: SelectionSource::Manual { overrode },
            };
            if let Some(group) = item.target.exclusive_group.clone() {
                if let Some(other) = manual_groups.insert(group.clone(), index) {
                    return Err(SelectionError(format!(
                        "manual selections conflict in exclusive group {group}: {} and {key}",
                        planned[other].candidate_key
                    )));
                }
            }
        } else if mode == SelectionMode::ManualOnly
            && matches!(item.selection, PlanSelection::Selected { .. })
        {
            item.selection = PlanSelection::Skipped("not manually selected".into());
        }
    }

    // Compare by row index rather than target ID: multiple candidates can share
    // an ID, yet an exclusive group must still have exactly one winner.
    let mut winners = manual_groups;
    for (index, item) in planned.iter().enumerate() {
        if !matches!(item.selection, PlanSelection::Selected { .. }) {
            continue;
        }
        let Some(group) = &item.target.exclusive_group else {
            continue;
        };
        winners
            .entry(group.clone())
            .and_modify(|winner| {
                let existing = &planned[*winner];
                if !matches!(
                    existing.selection,
                    PlanSelection::Selected {
                        source: SelectionSource::Manual { .. }
                    }
                ) && (item.target.exclusive_priority, &item.target.id)
                    > (existing.target.exclusive_priority, &existing.target.id)
                {
                    *winner = index;
                }
            })
            .or_insert(index);
    }
    for index in 0..planned.len() {
        if let Some(group) = &planned[index].target.exclusive_group {
            if matches!(planned[index].selection, PlanSelection::Selected { .. })
                && winners[group] != index
            {
                let winner = &planned[winners[group]].target.id;
                planned[index].selection =
                    PlanSelection::Skipped(format!("superseded by {winner}"));
            }
        }
    }
    // Automatic safe selection retains its conservative limits. Explicit
    // manual choices are reviewed separately; unattended execution still
    // enforces the count/byte boundary over the entire reviewed set.
    if level == CleanupLevel::Safe {
        const MAX_UNATTENDED_TARGETS: usize = 100;
        const MAX_UNATTENDED_BYTES: u64 = 50 * 1024 * 1024 * 1024;
        let selected_count = planned
            .iter()
            .filter(|item| {
                matches!(
                    item.selection,
                    PlanSelection::Selected {
                        source: SelectionSource::Level
                    }
                )
            })
            .count();
        let selected_bytes = planned
            .iter()
            .filter(|item| {
                matches!(
                    item.selection,
                    PlanSelection::Selected {
                        source: SelectionSource::Level
                    }
                )
            })
            .map(|item| item.target.usage.sortable_bytes())
            .fold(0_u64, u64::saturating_add);
        if selected_count > MAX_UNATTENDED_TARGETS || selected_bytes > MAX_UNATTENDED_BYTES {
            for item in &mut planned {
                if matches!(
                    item.selection,
                    PlanSelection::Selected {
                        source: SelectionSource::Level
                    }
                ) {
                    item.selection = PlanSelection::Skipped(
                        "unattended plan exceeds the conservative count/byte limit".into(),
                    );
                }
            }
        }
    }
    Ok(planned)
}

/// Metadata-only path index for any-of, nested, and inner-marker detectors.
/// Paths must be nonempty relative normal components; symlinks are never followed.
pub struct DetectorIndex<'a> {
    arena: &'a Arena,
    children: HashMap<NodeIndex, HashMap<&'a str, NodeIndex>>,
}
impl<'a> DetectorIndex<'a> {
    pub fn new(arena: &'a Arena) -> Self {
        let children = (0..arena.nodes.len())
            .map(NodeIndex::from_usize)
            .filter(|i| arena.nodes[i.as_usize()].kind == NodeKind::Directory)
            .map(|i| {
                (
                    i,
                    arena
                        .children(i)
                        .map(|child| (arena.name(child), child))
                        .collect(),
                )
            })
            .collect();
        Self { arena, children }
    }
    pub fn relative(&self, root: NodeIndex, path: &str) -> Option<NodeIndex> {
        if !valid_detector_path(path) {
            return None;
        }
        let mut current = root;
        for component in Path::new(path).components() {
            let name = component.as_os_str().to_str()?;
            current = *self.children.get(&current)?.get(name)?;
            if self.arena.nodes[current.as_usize()].kind == NodeKind::Symlink {
                return None;
            }
        }
        Some(current)
    }
    pub fn candidates(
        &self,
        markers: &[&str],
        directory: &str,
        inner: Option<&str>,
    ) -> Vec<(NodeIndex, NodeIndex)> {
        let mut matches = Vec::new();
        for (&project, children) in &self.children {
            if !markers.is_empty()
                && !markers.iter().any(|marker| {
                    self.relative(project, marker)
                        .is_some_and(|i| self.arena.nodes[i.as_usize()].kind == NodeKind::File)
                })
            {
                continue;
            }
            let candidates: Vec<_> = if directory.is_empty() && inner.is_some() {
                children.values().copied().collect()
            } else {
                self.relative(project, directory).into_iter().collect()
            };
            for candidate in candidates {
                if self.arena.nodes[candidate.as_usize()].kind != NodeKind::Directory {
                    continue;
                }
                if inner.is_some_and(|marker| {
                    !self
                        .relative(candidate, marker)
                        .is_some_and(|i| self.arena.nodes[i.as_usize()].kind == NodeKind::File)
                }) {
                    continue;
                }
                matches.push((project, candidate));
            }
        }
        matches.sort_by_key(|(_, candidate)| self.arena.path(*candidate));
        matches
    }
}

#[must_use]
pub fn valid_detector_path(value: &str) -> bool {
    !value.is_empty()
        && !Path::new(value).is_absolute()
        && Path::new(value)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
        && !value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
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
    let index = DetectorIndex::new(&scan.arena);

    // A stable path key prevents duplicate actions when overlapping definitions
    // resolve to the same artifact directory.
    let mut detected = BTreeMap::<PathBuf, DetectedTarget>::new();
    for definition in definitions {
        let mut markers = definition.detector.markers.to_vec();
        if !definition.detector.marker.is_empty() {
            markers.push(definition.detector.marker);
        }
        for (project, candidate) in index.candidates(
            &markers,
            definition.detector.directory,
            definition.detector.inner_marker,
        ) {
            let project_root = scan.arena.path(project);
            let path = scan.arena.path(candidate);
            let age_days = if definition.id == "cargo.target" {
                cargo_activity_age_days(
                    &scan.arena,
                    candidate,
                    index.relative(project, "Cargo.toml").expect("Cargo marker"),
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
                    cargo_ownership: None,
                    gitignore: None,
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
    let guarded: HashSet<_> = definitions
        .iter()
        .filter(|definition| definition.requires_gitignore)
        .map(|definition| definition.id)
        .collect();
    let ambiguous_paths: Vec<_> = detected
        .values()
        .filter(|target| guarded.contains(target.id.as_str()))
        .filter_map(|target| target.path.clone())
        .collect();
    let mut ignore_evidence = crate::projects::gitignore_evidence(scan, &ambiguous_paths, home);
    let mut candidates: Vec<_> = detected
        .into_values()
        .filter_map(|mut target| {
            if guarded.contains(target.id.as_str()) {
                target.gitignore = ignore_evidence.remove(target.path.as_ref()?);
                let evidence = target.gitignore.as_ref()?;
                target.evidence.push(format!(
                    "gitignored inside {}: {}",
                    evidence.repository.display(),
                    evidence.pattern
                ));
            }
            Some(target)
        })
        .collect();
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
        cargo_ownership: None,
        gitignore: None,
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
        "python.venv" | "python.requirements-venv" => {
            "Python environments may contain local installs or user data; review individually before permanent removal"
        }
        "gradle.build" | "gradle.kotlin-build" | "gradle.cache" | "gradle.kotlin-cache" => {
            "Gradle will regenerate build/cache output; the next build may be expensive"
        }
        "xcode.project-build" => "Xcode project build output will require a rebuild",
        "node.dist" => "distribution output will be regenerated by its build",
        _ => "generated workspace output will need to be recreated",
    }
}

pub(crate) fn modified_age_days(arena: &Arena, index: NodeIndex, now: u64) -> Option<u64> {
    // Directory mtime alone misses writes to an existing build artifact.
    let mut stack = vec![index];
    let mut modified = 0;
    while let Some(index) = stack.pop() {
        modified = modified.max(arena.nodes[index.as_usize()].modified_unix_seconds);
        stack.extend(arena.children(index));
    }
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
    if path.strip_prefix(project_root).ok().is_none_or(|relative| {
        !relative
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
    }) {
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
        assert_eq!(
            standard[0].selection,
            PlanSelection::Selected {
                source: SelectionSource::Level
            }
        );
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
                .filter(|item| matches!(item.selection, PlanSelection::Selected { .. }))
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
    fn manual_fixture() -> (tempfile::TempDir, DetectedTarget) {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(fixture.path().join("package.json"), b"{}").unwrap();
        std::fs::create_dir(fixture.path().join("node_modules")).unwrap();
        let scan = crate::Scanner::new(crate::ScanOptions::default())
            .scan(fixture.path())
            .unwrap();
        let target = detect_workspace_targets(&scan, BUILTIN_TARGETS, None).remove(0);
        (fixture, target)
    }

    fn manually_plan(
        targets: Vec<DetectedTarget>,
        manual_indices: &[usize],
        mode: SelectionMode,
    ) -> Result<Vec<PlannedTarget>, SelectionError> {
        let keys = manual_indices
            .iter()
            .map(|index| CandidateKey::from_target(&targets[*index]))
            .collect();
        plan_cleanup_with_selection(targets, CleanupLevel::Safe, &[], false, &keys, mode)
    }

    #[test]
    fn manual_selection_overrides_level_and_records_reason() {
        let (_fixture, mut target) = manual_fixture();
        target.stale = Some(true);
        let planned = manually_plan(vec![target], &[0], SelectionMode::ManualOnly).unwrap();
        assert_eq!(
            planned[0].selection,
            PlanSelection::Selected {
                source: SelectionSource::Manual {
                    overrode: Some("requires standard cleanup level".into())
                }
            }
        );
        let json = serde_json::to_value(&planned[0]).unwrap();
        assert_eq!(json["selection"]["status"], "selected");
        assert_eq!(json["selection"]["source"]["kind"], "manual");
    }

    #[test]
    fn manual_selection_overrides_staleness_and_unknown_age() {
        let (_fixture, mut target) = manual_fixture();
        target.minimum_level = CleanupLevel::Safe;
        target.properties = TargetClass::Cheap.properties();
        for (stale, reason) in [
            (Some(false), "target is not stale"),
            (None, "target age is unknown"),
        ] {
            target.stale = stale;
            let planned =
                manually_plan(vec![target.clone()], &[0], SelectionMode::ManualOnly).unwrap();
            assert_eq!(
                planned[0].selection,
                PlanSelection::Selected {
                    source: SelectionSource::Manual {
                        overrode: Some(reason.into())
                    }
                }
            );
        }
    }

    #[test]
    fn manual_selection_is_explicit_naming_for_user_data() {
        let (_fixture, mut target) = manual_fixture();
        target.minimum_level = CleanupLevel::Safe;
        target.class = TargetClass::Irreplaceable;
        target.properties = target.class.properties();
        target.stale = Some(true);
        let planned = manually_plan(vec![target], &[0], SelectionMode::ManualOnly).unwrap();
        assert_eq!(
            planned[0].selection,
            PlanSelection::Selected {
                source: SelectionSource::Manual {
                    overrode: Some("not selected by cleanup level".into())
                }
            }
        );
    }

    #[test]
    fn manual_selection_cannot_override_refusal_or_information() {
        let (_fixture, target) = manual_fixture();
        let mut refused = target.clone();
        refused.safety = CandidateSafety::Refused("dataless content".into());
        assert!(
            manually_plan(vec![refused], &[0], SelectionMode::ManualOnly)
                .unwrap_err()
                .to_string()
                .contains("safety refusal: dataless content")
        );
        let mut information = target;
        information.operation = PlannedOperation::Informational;
        assert!(
            manually_plan(vec![information], &[0], SelectionMode::ManualOnly)
                .unwrap_err()
                .to_string()
                .contains("informational only")
        );
    }

    #[test]
    fn exclusive_manual_conflicts_name_both_candidates_and_manual_wins() {
        let (_fixture, mut target) = manual_fixture();
        target.minimum_level = CleanupLevel::Safe;
        target.properties = TargetClass::Cheap.properties();
        target.stale = Some(true);
        target.exclusive_group = Some("alternatives".into());
        let mut alternative = target.clone();
        alternative.id = "alternative".into();
        alternative.exclusive_priority = 255;
        let error = manually_plan(
            vec![target.clone(), alternative.clone()],
            &[0, 1],
            SelectionMode::LevelPlusManual,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains(&CandidateKey::from_target(&target).to_string()));
        assert!(error.contains(&CandidateKey::from_target(&alternative).to_string()));
        let planned = manually_plan(
            vec![target, alternative],
            &[0],
            SelectionMode::LevelPlusManual,
        )
        .unwrap();
        assert!(matches!(
            planned[0].selection,
            PlanSelection::Selected {
                source: SelectionSource::Manual { overrode: Some(ref reason) }
            } if reason == "superseded by alternative"
        ));
        assert_eq!(
            planned[1].selection,
            PlanSelection::Skipped("superseded by node.modules".into())
        );
    }

    #[test]
    fn selection_modes_control_level_selected_siblings() {
        let (_fixture, mut target) = manual_fixture();
        target.minimum_level = CleanupLevel::Safe;
        target.properties = TargetClass::Cheap.properties();
        target.stale = Some(true);
        let mut sibling = target.clone();
        sibling.path = Some(
            target
                .path
                .as_ref()
                .unwrap()
                .parent()
                .unwrap()
                .join("sibling"),
        );
        for (mode, count) in [
            (SelectionMode::ManualOnly, 1),
            (SelectionMode::LevelPlusManual, 2),
        ] {
            let planned = manually_plan(vec![target.clone(), sibling.clone()], &[0], mode).unwrap();
            assert_eq!(
                planned
                    .iter()
                    .filter(|item| matches!(item.selection, PlanSelection::Selected { .. }))
                    .count(),
                count
            );
        }
        assert!(
            manually_plan(vec![target], &[], SelectionMode::ManualOnly)
                .unwrap()
                .iter()
                .all(|item| matches!(item.selection, PlanSelection::Skipped(_)))
        );
    }

    #[test]
    fn candidate_keys_preserve_exact_command_identity_and_serde() {
        let (_fixture, mut target) = manual_fixture();
        target.path = None;
        target.operation = PlannedOperation::Command {
            command: CommandSpec {
                executable: "/fixture/tool".into(),
                args: vec!["one two".into()],
                environment: BTreeMap::new(),
            },
        };
        let key = CandidateKey::from_target(&target);
        let decoded: CandidateKey =
            serde_json::from_str(&serde_json::to_string(&key).unwrap()).unwrap();
        assert_eq!(key, decoded);
        if let PlannedOperation::Command { command } = &mut target.operation {
            command.args = vec!["one".into(), "two".into()];
        }
        assert_ne!(key, CandidateKey::from_target(&target));
        target.provider.context = Some("other daemon".into());
        assert_ne!(key, CandidateKey::from_target(&target));
    }
    #[test]
    fn manual_selection_overrides_explicit_only_generated_policy() {
        let (_fixture, mut target) = manual_fixture();
        target.minimum_level = CleanupLevel::Safe;
        target.properties = TargetClass::Cheap.properties();
        target.properties.automation = Automation::ExplicitTargetOnly;
        target.stale = Some(true);
        let planned = manually_plan(vec![target], &[0], SelectionMode::ManualOnly).unwrap();
        assert_eq!(
            planned[0].selection,
            PlanSelection::Selected {
                source: SelectionSource::Manual {
                    overrode: Some("not selected by cleanup level".into())
                }
            }
        );
    }

    #[test]
    fn manual_choice_does_not_widen_automatic_safe_limits() {
        let (fixture, mut target) = manual_fixture();
        target.minimum_level = CleanupLevel::Safe;
        target.properties = TargetClass::Cheap.properties();
        target.stale = Some(true);
        let targets = (0..101)
            .map(|index| {
                let path = fixture.path().join(format!("candidate-{index}"));
                std::fs::create_dir(&path).unwrap();
                let mut candidate = target.clone();
                candidate.path = Some(path);
                candidate
            })
            .collect();
        let planned = manually_plan(targets, &[0], SelectionMode::LevelPlusManual).unwrap();
        assert!(matches!(&planned[0].selection, PlanSelection::Selected {
            source: SelectionSource::Manual { overrode: Some(reason) }
        } if reason.contains("conservative count/byte limit")));
        assert!(
            planned[1..]
                .iter()
                .all(|item| matches!(item.selection, PlanSelection::Skipped(_)))
        );
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::{ScanOptions, Scanner};
    use std::fs;

    #[test]
    fn every_unambiguous_detector_has_fixture_and_candidate_cloud_dataless_guards() {
        for definition in BUILTIN_TARGETS.iter().filter(|d| !d.requires_gitignore) {
            let fixture = tempfile::tempdir().unwrap();
            let project = fixture.path().join("project");
            fs::create_dir_all(&project).unwrap();
            let detector = definition.detector;
            if let Some(marker) = detector
                .markers
                .first()
                .copied()
                .or_else(|| (!detector.marker.is_empty()).then_some(detector.marker))
            {
                let path = project.join(marker);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, "marker").unwrap();
            }
            let artifact = project.join(if detector.directory.is_empty() {
                "custom-python-name"
            } else {
                detector.directory
            });
            fs::create_dir_all(&artifact).unwrap();
            fs::write(artifact.join("generated"), "fixture").unwrap();
            if let Some(marker) = detector.inner_marker {
                fs::write(artifact.join(marker), "marker").unwrap();
            }
            let mut scan = Scanner::new(ScanOptions::default()).scan(&project).unwrap();
            let targets = detect_workspace_targets(&scan, &[*definition], Some(fixture.path()));
            assert_eq!(targets.len(), 1, "{}", definition.id);
            assert_eq!(targets[0].path.as_ref(), Some(&artifact));
            assert!(targets[0].manually_selectable(), "{}", definition.id);
            let candidate = scan
                .arena
                .nodes
                .iter()
                .enumerate()
                .find_map(|(i, _)| {
                    (scan.arena.path(NodeIndex::from_usize(i)) == artifact).then_some(i)
                })
                .unwrap();
            scan.arena.nodes[candidate]
                .flags
                .insert(NodeFlags::DATALESS);
            assert!(
                !detect_workspace_targets(&scan, &[*definition], Some(fixture.path()))[0]
                    .manually_selectable(),
                "{}",
                definition.id
            );
            // The identical metadata tree is also refused when scoped under a
            // synced provider path, independently of its placeholder flags.
            let cloud_home = project.parent().unwrap();
            let cloud_project = cloud_home.join("Library/CloudStorage/service/project");
            fs::create_dir_all(cloud_project.parent().unwrap()).unwrap();
            fs::rename(&project, &cloud_project).unwrap();
            let cloud = Scanner::new(Default::default())
                .scan(&cloud_project)
                .unwrap();
            assert!(
                !detect_workspace_targets(&cloud, &[*definition], Some(cloud_home))[0]
                    .manually_selectable(),
                "{}",
                definition.id
            );
        }
    }
    #[test]
    fn expo_never_targets_source_roots_and_venv_collapses_nested_bytecode() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        for path in [
            "ios/Pods",
            "android/.gradle",
            "android/app/.cxx",
            "custom-env/lib/__pycache__",
        ] {
            fs::create_dir_all(root.join(path)).unwrap();
        }
        for path in [
            "package.json",
            "ios/Podfile",
            "android/settings.gradle.kts",
            "android/build.gradle",
            "custom-env/pyvenv.cfg",
        ] {
            fs::write(root.join(path), "fixture").unwrap();
        }
        let scan = Scanner::new(Default::default()).scan(root).unwrap();
        let targets = detect_workspace_targets(&scan, BUILTIN_TARGETS, None);
        for relative in [
            "ios/Pods",
            "android/.gradle",
            "android/app/.cxx",
            "custom-env",
        ] {
            assert!(
                targets
                    .iter()
                    .any(|t| t.path.as_ref() == Some(&root.join(relative))),
                "{relative}"
            );
        }
        for relative in ["ios", "android", "custom-env/lib/__pycache__"] {
            assert!(
                !targets
                    .iter()
                    .any(|t| t.path.as_ref() == Some(&root.join(relative))),
                "{relative}"
            );
        }
        let environment = targets.iter().find(|t| t.id == "python.venv").unwrap();
        assert_eq!(environment.class, TargetClass::Network);
        assert!(!CleanupLevel::Safe.selects(environment.properties, false));
    }
    #[test]
    fn generic_names_require_gitignore_and_ignore_evidence_is_rechecked() {
        let fixture = tempfile::tempdir().unwrap();
        for path in [
            "build",
            "out",
            "coverage",
            "ios/build",
            "android/build",
            "android/app/build",
        ] {
            fs::create_dir_all(fixture.path().join(path)).unwrap();
        }
        let scan = Scanner::new(Default::default())
            .scan(fixture.path())
            .unwrap();
        assert!(detect_workspace_targets(&scan, BUILTIN_TARGETS, None).is_empty());
        let git = crate::providers::SystemProviderHost::project_inspection();
        use crate::providers::ProviderHost;
        let Some(executable) = git.resolve_executable("git").unwrap() else {
            return;
        };
        let init = CommandSpec {
            executable,
            args: vec!["init".into(), fixture.path().display().to_string()],
            environment: BTreeMap::new(),
        };
        assert_eq!(git.run_inspection(&init).unwrap().exit_code, Some(0));
        fs::write(
            fixture.path().join(".gitignore"),
            "build/\nout/\ncoverage/\n",
        )
        .unwrap();
        let scan = Scanner::new(Default::default())
            .scan(fixture.path())
            .unwrap();
        let targets = detect_workspace_targets(&scan, BUILTIN_TARGETS, None);
        assert_eq!(targets.len(), 6);
        assert!(targets.iter().all(|t| t.gitignore.is_some()));
        let target = &targets[0];
        assert!(
            crate::projects::revalidate_artifact_repository(target, fixture.path(), None, &git)
                .is_ok()
        );
        fs::write(fixture.path().join(".gitignore"), "").unwrap();
        assert!(
            crate::projects::revalidate_artifact_repository(target, fixture.path(), None, &git)
                .is_err()
        );
        let scan = Scanner::new(Default::default())
            .scan(fixture.path())
            .unwrap();
        assert!(detect_workspace_targets(&scan, BUILTIN_TARGETS, None).is_empty());
    }
}
