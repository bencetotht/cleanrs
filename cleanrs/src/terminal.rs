//! TTY-aware human output. JSON, completions, and man pages bypass this module.

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use comfy_table::{
    Attribute, Cell, CellAlignment, Color, ContentArrangement, Table,
    modifiers::UTF8_ROUND_CORNERS,
    presets::{ASCII_FULL_CONDENSED, UTF8_FULL_CONDENSED},
};
use console::Style;
use indicatif::{ProgressBar, ProgressStyle};

use cleanrs_core::targets::{
    Automation, CandidateSafety, DetectedTarget, PlanSelection, PlannedOperation, Recoverability,
    SelectionSource, UsageEstimate,
};
use cleanrs_core::{Confidence, DoctorReport, ScanObserver, ScanProgress};

use crate::pipeline::{ExecutionEvent, ExecutionObserver, PlanEvent, PlanObserver};
use crate::{CleanOutput, Metric, ScanOutput, TargetsOutput, format_interval, human_bytes};
use cleanrs_core::execution::{OutputStream, command_argv};

/// Presentation policy derived from the actual output streams and `NO_COLOR`.
pub(crate) struct TerminalUi {
    stdout_is_terminal: bool,
    colors: bool,
}

impl TerminalUi {
    pub(crate) fn detect() -> Self {
        let stdout_is_terminal = io::stdout().is_terminal();
        Self {
            stdout_is_terminal,
            colors: stdout_is_terminal && std::env::var_os("NO_COLOR").is_none(),
        }
    }

    pub(crate) fn print_scan(&self, output: &ScanOutput, metric: Metric) {
        println!(
            "{}  {}",
            self.heading("cleanrs scan"),
            self.muted(&output.root.display().to_string())
        );
        println!();

        let mut results = self.table();
        match metric {
            Metric::Reclaimable => results.set_header(vec![
                self.header("RECLAIMABLE"),
                self.header("CONFIDENCE"),
                self.header("PATH"),
                self.header("DETAIL"),
            ]),
            Metric::Private => results.set_header(vec![
                self.header("PRIVATE"),
                self.header("CONFIDENCE"),
                self.header("PATH"),
                self.header("DETAIL"),
            ]),
            Metric::Apparent => {
                results.set_header(vec![self.header("APPARENT"), self.header("PATH")])
            }
            Metric::Allocated => {
                results.set_header(vec![self.header("ALLOCATED"), self.header("PATH")])
            }
        };

        for entry in &output.entries {
            let path = display_path(&output.root, &entry.path, entry.depth);
            match metric {
                Metric::Apparent => {
                    results.add_row(vec![self.size(entry.apparent), Cell::new(path)]);
                }
                Metric::Allocated => {
                    results.add_row(vec![self.size(entry.allocated), Cell::new(path)]);
                }
                Metric::Private => {
                    let value = entry
                        .estimate
                        .floor
                        .map_or_else(|| "unknown".into(), human_bytes);
                    results.add_row(vec![
                        self.value(value, value_color(entry.estimate.floor.is_some())),
                        self.confidence(entry.estimate.confidence),
                        Cell::new(path),
                        Cell::new(entry.estimate.explanation.as_deref().unwrap_or("—")),
                    ]);
                }
                Metric::Reclaimable => {
                    results.add_row(vec![
                        self.value(format_interval(&entry.estimate), Color::Cyan),
                        self.confidence(entry.estimate.confidence),
                        Cell::new(path),
                        Cell::new(entry.estimate.explanation.as_deref().unwrap_or("—")),
                    ]);
                }
            }
        }
        println!("{results}");

        let mut summary = self.table();
        summary.set_header(vec![self.header("SCAN SUMMARY"), self.header("VALUE")]);
        summary.add_row(vec![Cell::new("Metric"), Cell::new(output.metric)]);
        summary.add_row(vec![Cell::new("Backend"), Cell::new(&output.backend)]);
        summary.add_row(vec![
            Cell::new("Coverage"),
            if output.coverage_complete {
                self.value("complete", Color::Green)
            } else {
                self.value("incomplete", Color::Red)
            },
        ]);
        summary.add_row(vec![
            Cell::new("Displayed rows"),
            Cell::new(output.entries.len()),
        ]);
        summary.add_row(vec![
            Cell::new("Dataless entries"),
            self.value(
                output.dataless_entries,
                if output.dataless_entries == 0 {
                    Color::Green
                } else {
                    Color::Yellow
                },
            ),
        ]);
        summary.add_row(vec![
            Cell::new("Mount boundaries"),
            Cell::new(output.mount_boundaries),
        ]);
        println!();
        println!("{summary}");

        if output.dataless_entries > 0 {
            println!();
            println!(
                "{} {} dataless entries observed; file content was never opened.",
                self.warning("SAFETY"),
                output.dataless_entries
            );
        }
        if !output.coverage_complete {
            println!();
            println!(
                "{} {} inaccessible locations; their size is unknown.",
                self.error("INCOMPLETE"),
                output.coverage_errors.len()
            );
            let mut errors = self.table();
            errors.set_header(vec![self.header("PATH"), self.header("ERROR")]);
            for error in output.coverage_errors.iter().take(10) {
                errors.add_row(vec![
                    Cell::new(error.path.display()),
                    Cell::new(&error.message),
                ]);
            }
            println!("{errors}");
        }
    }

    pub(crate) fn print_doctor(&self, report: &DoctorReport) {
        println!(
            "{}  {}",
            self.heading("cleanrs doctor"),
            self.muted(
                report
                    .container
                    .as_deref()
                    .unwrap_or("container unavailable")
            )
        );
        println!();

        let mut container = self.table();
        container.set_header(vec![self.header("CONTAINER"), self.header("SIZE")]);
        container.add_row(vec![Cell::new("Capacity"), self.size(report.capacity)]);
        container.add_row(vec![
            Cell::new("Free (shared)"),
            self.value(human_bytes(report.free_shared), Color::Green),
        ]);
        container.add_row(vec![
            Cell::new("Used"),
            self.value(human_bytes(report.used), Color::Yellow),
        ]);
        println!("{container}");

        println!();
        println!("{}", self.section("Volumes"));
        let mut volumes = self.table();
        volumes.set_header(vec![
            self.header("VOLUME"),
            self.header("USED"),
            self.header("MOUNT"),
            self.header("ROLE"),
        ]);
        for volume in &report.volumes {
            volumes.add_row(vec![
                Cell::new(&volume.name),
                self.size(volume.used),
                Cell::new(volume.mount_point.display()),
                Cell::new(&volume.role),
            ]);
        }
        println!("{volumes}");

        println!();
        println!("{}", self.section("Data attribution"));
        let mut attribution = self.table();
        attribution.set_header(vec![self.header("MEASURE"), self.header("VALUE")]);
        attribution.add_row(vec![
            Cell::new("Clone-deduped allocated"),
            optional_bytes(report.attributed_allocated, "not scanned", self),
        ]);
        attribution.add_row(vec![
            Cell::new("Unattributed remainder"),
            optional_bytes(report.unattributed, "unknown", self),
        ]);
        attribution.add_row(vec![
            Cell::new("Scan coverage"),
            match report.scan_coverage_complete {
                Some(true) => self.value("complete", Color::Green),
                Some(false) => self.value("incomplete", Color::Red),
                None => self.value("not scanned", Color::DarkGrey),
            },
        ]);
        println!("{attribution}");

        println!();
        println!("{}", self.section("Diagnostics"));
        let mut diagnostics = self.table();
        diagnostics.set_header(vec![self.header("CHECK"), self.header("STATUS")]);
        diagnostics.add_row(vec![
            Cell::new("Purgeable"),
            optional_bytes(report.purgeable, "unknown", self),
        ]);
        diagnostics.add_row(vec![
            Cell::new("Local snapshots"),
            report.snapshot_count.map_or_else(
                || self.value("unknown", Color::Yellow),
                |count| {
                    self.value(
                        count,
                        if count == 0 {
                            Color::Green
                        } else {
                            Color::Yellow
                        },
                    )
                },
            ),
        ]);
        diagnostics.add_row(vec![
            Cell::new("Full Disk Access"),
            self.value(&report.full_disk_access, Color::Yellow),
        ]);
        println!("{diagnostics}");

        if !report.notes.is_empty() {
            println!();
            println!("{}", self.section("Notes"));
            for note in &report.notes {
                println!("  {} {note}", self.muted("•"));
            }
        }
    }

    pub(crate) fn print_projects(&self, inventory: &cleanrs_core::projects::ProjectInventory) {
        println!(
            "{}",
            self.heading("cleanrs projects — read-only recommendations")
        );
        let mut table = self.table();
        table.set_header([
            "PROJECT",
            "ATTRIBUTED",
            "ACTIVITY",
            "GIT",
            "INACTIVITY",
            "ARTIFACTS",
        ]);
        for project in &inventory.projects {
            table.add_row(vec![
                project.root.display().to_string(),
                match &project.usage {
                    UsageEstimate::Filesystem { allocated, .. } => human_bytes(*allocated),
                    _ => "unknown".into(),
                },
                format!("{:?}", project.activity),
                project.git.as_ref().map_or_else(
                    || "not inspected".into(),
                    |git| {
                        format!(
                            "{:?} dirty={} untracked={} complete={}",
                            git.synchronization, git.dirty, git.untracked, git.complete
                        )
                    },
                ),
                project.inactivity_days.map_or_else(
                    || "unknown".into(),
                    |d| format!("{d}d / {}d", project.dormant_after_days),
                ),
                project.artifacts.len().to_string(),
            ]);
        }
        println!("{table}");
        for project in &inventory.projects {
            println!("\n{}", project.root.display());
            println!("  {}", project.recommendation);
            for evidence in &project.evidence {
                println!("  {evidence}");
            }
            for shared in &project.shared_artifacts {
                println!(
                    "  shared Cargo output (counted under primary owner): {}",
                    shared.display()
                );
            }
            for artifact in &project.artifacts {
                let (usage, confidence) = usage_display(&artifact.target.usage);
                println!(
                    "  {} {}: {} · {}",
                    if artifact.recommended {
                        "recommend"
                    } else {
                        "review"
                    },
                    artifact.path.display(),
                    usage,
                    artifact.ecosystem
                );
                println!(
                    "    confidence: {}",
                    confidence.map_or_else(
                        || "unknown".into(),
                        |confidence| format!("{confidence:?}").to_lowercase()
                    )
                );
                match &artifact.target.operation {
                    PlannedOperation::DirectDelete {
                        path,
                        children_only,
                        ..
                    } => println!(
                        "    permanently delete {}{}",
                        if *children_only { "children of " } else { "" },
                        path.display()
                    ),
                    PlannedOperation::Command { command } => {
                        println!("    argv: {:?}", command_argv(command))
                    }
                    PlannedOperation::Informational => {
                        println!("    informational; no cleanup operation")
                    }
                }
                println!("    consequence: {}", artifact.target.consequence);
                for evidence in artifact
                    .supporting_evidence
                    .iter()
                    .chain(&artifact.weakening_evidence)
                {
                    println!("    {evidence}");
                }
            }
        }
        for diagnostic in &inventory.diagnostics {
            println!("\n{diagnostic}");
        }
        println!(
            "\n{} repositories inspected, {} commands, {} omitted",
            inventory.inspected_repositories,
            inventory.inspection_commands,
            inventory.omitted_repositories
        );
    }

    pub(crate) fn print_targets(&self, output: &TargetsOutput) {
        println!(
            "{}  {}",
            self.heading("cleanrs targets"),
            self.muted(&if output.system {
                "scope: system".into()
            } else {
                output.scope.display().to_string()
            })
        );
        println!();

        let mut targets = self.table();
        targets.set_header(vec![
            self.header("RECLAIMABLE"),
            self.header("OCCUPIED"),
            self.header("PROJECT"),
            self.header("LOCATION"),
            self.header("AGE"),
            self.header("STATUS"),
            self.header("TARGET"),
        ]);
        for target in &output.targets {
            let age = target.age_days.map_or_else(
                || "unknown".into(),
                |days| format!("{days}d / {}d", target.stale_after_days),
            );
            let status = match &target.safety {
                CandidateSafety::Eligible
                    if matches!(target.operation, PlannedOperation::Informational) =>
                {
                    self.value("inventory only", Color::Yellow)
                }
                CandidateSafety::Eligible if target.stale == Some(true) => {
                    self.value("eligible", Color::Green)
                }
                CandidateSafety::Eligible => self.value("not stale", Color::Yellow),
                CandidateSafety::Refused(reason) => {
                    self.value(format!("refused: {reason}"), Color::Red)
                }
            };
            let (usage, _) = target_usage_display(target);
            targets.add_row(vec![
                self.value(usage, Color::Cyan),
                Cell::new(occupied_display(target)),
                Cell::new(target_project_name(target)),
                Cell::new(target_location(target)),
                Cell::new(age),
                status,
                Cell::new(&target.id),
            ]);
        }
        println!("{targets}");

        println!();
        let mut summary = self.table();
        summary.set_header(vec![self.header("TARGET SUMMARY"), self.header("VALUE")]);
        summary.add_row(vec![
            Cell::new("Showing"),
            Cell::new(format!(
                "{} of {}",
                output.targets.len(),
                output.total_detected
            )),
        ]);
        summary.add_row(vec![
            Cell::new("Hidden by filters"),
            Cell::new(output.hidden),
        ]);
        summary.add_row(vec![
            Cell::new("Eligible and stale"),
            Cell::new(
                output
                    .targets
                    .iter()
                    .filter(|target| {
                        matches!(target.safety, CandidateSafety::Eligible)
                            && target.stale == Some(true)
                    })
                    .count(),
            ),
        ]);
        summary.add_row(vec![
            Cell::new("Coverage"),
            if output.coverage_complete {
                self.value("complete", Color::Green)
            } else {
                self.value("incomplete", Color::Red)
            },
        ]);
        summary.add_row(vec![
            Cell::new("Execution"),
            self.value("read-only; no action was run", Color::Green),
        ]);
        println!("{summary}");
        self.print_provider_statuses(&output.providers);
        if output.default_limit_applied && output.hidden > 0 {
            println!();
            println!(
                "{} Showing the 25 largest targets. Use --top N or --min-size SIZE to adjust.",
                self.muted("•")
            );
        }
    }

    pub(crate) fn print_clean(&self, output: &CleanOutput) {
        println!(
            "{}  {}",
            self.heading("cleanrs clean — dry run"),
            self.muted(&if output.system {
                "scope: system".into()
            } else {
                output.scope.display().to_string()
            })
        );
        println!();

        let mut plan = self.table();
        plan.set_header(vec![
            self.header("ACTION"),
            self.header("RECLAIMABLE"),
            self.header("PROJECT"),
            self.header("LOCATION"),
            self.header("TARGET"),
            self.header("REASON"),
        ]);
        for planned in &output.targets {
            let (action, reason, color) = match &planned.selection {
                PlanSelection::Selected {
                    source: SelectionSource::Level,
                } => ("plan", "selected".to_owned(), Color::Yellow),
                PlanSelection::Selected {
                    source: SelectionSource::Manual { overrode },
                } => (
                    "plan (manual)",
                    overrode
                        .clone()
                        .unwrap_or_else(|| "selected manually".into()),
                    if planned.target.properties.recoverability == Recoverability::UserData
                        || planned.target.properties.automation == Automation::ExplicitTargetOnly
                    {
                        Color::Red
                    } else {
                        Color::Yellow
                    },
                ),
                PlanSelection::Skipped(reason) => ("skip", reason.clone(), Color::DarkGrey),
            };
            let (usage, _) = target_usage_display(&planned.target);
            plan.add_row(vec![
                self.value(action, color),
                self.value(usage, Color::Cyan),
                Cell::new(target_project_name(&planned.target)),
                Cell::new(target_location(&planned.target)),
                Cell::new(&planned.target.id),
                if matches!(
                    planned.selection,
                    PlanSelection::Selected {
                        source: SelectionSource::Manual { .. }
                    }
                ) {
                    self.value(reason, color)
                } else {
                    Cell::new(reason)
                },
            ]);
        }
        println!("{plan}");

        let planned_interval = format_interval(&output.selected_estimate);
        let mut summary = self.table();
        summary.set_header(vec![self.header("PLAN SUMMARY"), self.header("VALUE")]);
        summary.add_row(vec![
            Cell::new("Level"),
            Cell::new(format!("{:?}", output.level).to_lowercase()),
        ]);
        summary.add_row(vec![
            Cell::new("Selected targets"),
            Cell::new(output.selected_targets),
        ]);
        summary.add_row(vec![
            Cell::new("Filesystem reclaimable"),
            self.value(planned_interval, Color::Cyan),
        ]);
        summary.add_row(vec![
            Cell::new("Provider-reported reclaimable"),
            self.value(human_bytes(output.selected_provider_reported), Color::Cyan),
        ]);
        summary.add_row(vec![
            Cell::new("Provider-reported actions"),
            Cell::new(output.selected_provider_actions),
        ]);
        summary.add_row(vec![
            Cell::new("Selected actions with unknown size"),
            Cell::new(output.selected_unknown_actions),
        ]);
        summary.add_row(vec![
            Cell::new("Coverage"),
            if output.coverage_complete {
                self.value("complete", Color::Green)
            } else {
                self.value("incomplete", Color::Red)
            },
        ]);
        summary.add_row(vec![
            Cell::new("Execution"),
            self.value(&output.execution, Color::Green),
        ]);
        println!();
        println!("{summary}");
        self.print_provider_statuses(&output.providers);
        if output.execution.starts_with("dry-run") {
            println!();
            if output.targets.iter().any(|item| {
                matches!(
                    item.selection,
                    PlanSelection::Selected {
                        source: SelectionSource::Manual { .. }
                    }
                )
            }) {
                println!(
                    "{} Only your {} candidates are selected. Review manual overrides, then add {} to execute.",
                    self.muted("TIP"),
                    self.warning("--select"),
                    self.warning("--apply")
                );
            } else if output.selected_targets > 0 {
                println!(
                    "{} This was a dry run. Add {} to execute the reviewed plan.",
                    self.muted("TIP"),
                    self.warning("--apply")
                );
            } else if let Some(level) = required_cleanup_level(&output.targets) {
                println!(
                    "{} No actions were selected. Try {}, review the plan, then add {}.",
                    self.muted("TIP"),
                    self.warning(&format!("--level {level}")),
                    self.warning("--apply")
                );
            } else {
                println!(
                    "{} No actions were selected. Adjust the filters or level, review again, then add {}.",
                    self.muted("TIP"),
                    self.warning("--apply")
                );
            }
        }
    }

    pub(crate) fn print_apply_results(&self, output: &CleanOutput) {
        println!();
        println!("{}", self.heading("Cleanup results"));
        let mut table = self.table();
        table.set_header(vec![
            self.header("STATUS"),
            self.header("TARGET"),
            self.header("EVIDENCE"),
        ]);
        for applied in &output.results {
            let result = &applied.result;
            let (status, color) = match result.status {
                cleanrs_core::execution::ActionStatus::Success => ("success", Color::Green),
                cleanrs_core::execution::ActionStatus::Skipped => ("skipped", Color::DarkGrey),
                cleanrs_core::execution::ActionStatus::ValidationAbort => {
                    ("aborted", Color::Yellow)
                }
                cleanrs_core::execution::ActionStatus::Failed => ("failed", Color::Red),
            };
            let evidence = result
                .removed_allocation
                .map(|bytes| format!("{} allocation removed", human_bytes(bytes)))
                .or_else(|| {
                    result
                        .provider_reported
                        .map(|bytes| format!("provider reported {}", human_bytes(bytes)))
                })
                .or_else(|| result.error.clone())
                .unwrap_or_else(|| "completed".into());
            table.add_row(vec![
                self.value(status, color),
                Cell::new(&applied.target_id),
                Cell::new(evidence),
            ]);
        }
        println!("{table}");
        println!("Outcome: {}", output.execution);
    }

    fn print_provider_statuses(&self, statuses: &[cleanrs_core::providers::ProviderStatus]) {
        if statuses.is_empty() {
            return;
        }
        println!();
        let inspected = statuses
            .iter()
            .filter(|status| status.detected)
            .map(|status| status.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if !inspected.is_empty() {
            println!("{} {inspected}", self.muted("Providers inspected:"));
        }

        // Successful identity/context details remain available in JSON. The
        // default terminal view only expands providers that need attention.
        let notices = statuses
            .iter()
            .filter(|status| {
                !status.detected
                    || status.message.contains("failed")
                    || (status.message.contains("skipped")
                        && !status.message.contains("0 manifest(s) skipped"))
            })
            .collect::<Vec<_>>();
        if !notices.is_empty() {
            println!();
            println!("{}", self.section("Provider notices"));
            let mut providers = self.table();
            providers.set_header(vec![self.header("PROVIDER"), self.header("STATUS")]);
            for status in notices {
                providers.add_row(vec![Cell::new(&status.id), Cell::new(&status.message)]);
            }
            println!("{providers}");
        }
    }

    fn table(&self) -> Table {
        let mut table = Table::new();
        if self.stdout_is_terminal {
            table.load_preset(UTF8_FULL_CONDENSED);
            table.apply_modifier(UTF8_ROUND_CORNERS);
        } else {
            table.load_preset(ASCII_FULL_CONDENSED);
        }
        table.set_content_arrangement(ContentArrangement::Dynamic);
        table
    }

    fn header(&self, text: &str) -> Cell {
        let cell = Cell::new(text).add_attribute(Attribute::Bold);
        self.maybe_color(cell, Color::Cyan)
    }

    fn size(&self, bytes: u64) -> Cell {
        self.value(human_bytes(bytes), Color::Cyan)
            .set_alignment(CellAlignment::Right)
    }

    fn confidence(&self, confidence: Confidence) -> Cell {
        let (label, color) = match confidence {
            Confidence::High => ("high", Color::Green),
            Confidence::Medium => ("medium", Color::Yellow),
            Confidence::Low => ("low", Color::Red),
        };
        self.value(label, color)
    }

    fn value(&self, value: impl ToString, color: Color) -> Cell {
        self.maybe_color(Cell::new(value), color)
    }

    fn maybe_color(&self, cell: Cell, color: Color) -> Cell {
        if self.colors { cell.fg(color) } else { cell }
    }

    fn heading(&self, text: &str) -> String {
        self.paint(text, Style::new().bold().cyan())
    }

    fn section(&self, text: &str) -> String {
        self.paint(text, Style::new().bold())
    }

    fn warning(&self, text: &str) -> String {
        self.paint(text, Style::new().bold().yellow())
    }

    fn error(&self, text: &str) -> String {
        self.paint(text, Style::new().bold().red())
    }

    fn muted(&self, text: &str) -> String {
        self.paint(text, Style::new().dim())
    }

    fn paint(&self, text: &str, style: Style) -> String {
        if self.colors {
            style.apply_to(text).to_string()
        } else {
            text.to_owned()
        }
    }
}

fn required_cleanup_level(
    targets: &[cleanrs_core::targets::PlannedTarget],
) -> Option<&'static str> {
    let mut required = None;
    for planned in targets {
        let PlanSelection::Skipped(reason) = &planned.selection else {
            continue;
        };
        if reason.contains("requires aggressive cleanup level") {
            return Some("aggressive");
        }
        if reason.contains("requires standard cleanup level") {
            required = Some("standard");
        }
    }
    required
}

fn optional_bytes(value: Option<u64>, absent: &str, ui: &TerminalUi) -> Cell {
    value.map_or_else(|| ui.value(absent, Color::Yellow), |bytes| ui.size(bytes))
}

fn value_color(known: bool) -> Color {
    if known { Color::Cyan } else { Color::Yellow }
}

fn display_path(root: &Path, path: &Path, depth: usize) -> String {
    if path == root {
        return ".".into();
    }
    let visible = path.strip_prefix(root).unwrap_or(path).display();
    format!("{}{visible}", "  ".repeat(depth.saturating_sub(1)))
}

fn target_display_path(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    home_relative_display(path, home.as_deref())
}

fn home_relative_display(path: &Path, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return path.display().to_string();
    };
    path.strip_prefix(home).map_or_else(
        |_| path.display().to_string(),
        |relative| {
            if relative.as_os_str().is_empty() {
                "~".into()
            } else {
                format!("~/{}", relative.display())
            }
        },
    )
}

/// Use the owning workspace name as the primary human identifier. Provider
/// actions have no project root, so their stable provider name is the clearest
/// equivalent label.
fn target_project_name(target: &DetectedTarget) -> String {
    target
        .project_root
        .as_deref()
        .and_then(Path::file_name)
        .or_else(|| target.path.as_deref().and_then(Path::file_name))
        .map_or_else(
            || target.provider.id.clone(),
            |name| name.to_string_lossy().into_owned(),
        )
}

/// Keep the default table focused on location. Exact command argv and direct
/// deletion semantics remain in the JSON representation for detailed review.
fn target_location(target: &DetectedTarget) -> String {
    target.path.as_ref().map_or_else(
        || {
            target
                .provider
                .context
                .clone()
                .unwrap_or_else(|| "—".into())
        },
        |path| target_display_path(path),
    )
}

pub(crate) fn usage_display(usage: &UsageEstimate) -> (String, Option<Confidence>) {
    match usage {
        UsageEstimate::Filesystem { reclaimable, .. } => {
            (format_interval(reclaimable), Some(reclaimable.confidence))
        }
        UsageEstimate::ProviderReported {
            bytes, confidence, ..
        } => (
            bytes.map_or_else(|| "unknown".into(), human_bytes),
            Some(*confidence),
        ),
        UsageEstimate::Unknown { .. } => ("unknown".into(), None),
    }
}

/// Keep occupied allocation separate from native cleanup yield. Multiple roots
/// are deliberately not summed here: sharing can span the individual trees.
pub(crate) fn occupied_display(target: &DetectedTarget) -> String {
    match target.storage.as_slice() {
        [storage] => human_bytes(storage.allocated),
        [] => match target.usage {
            UsageEstimate::Filesystem { allocated, .. } => human_bytes(allocated),
            _ if target
                .evidence
                .iter()
                .any(|e| e.starts_with("storage absent")) =>
            {
                "absent".into()
            }
            _ => "not measured".into(),
        },
        roots => format!("{} measured roots", roots.len()),
    }
}

pub(crate) fn target_usage_display(target: &DetectedTarget) -> (String, Option<Confidence>) {
    if matches!(target.safety, CandidateSafety::Refused(_)) {
        return ("inspection failed/refused".into(), None);
    }
    if matches!(target.operation, PlannedOperation::Informational) {
        return ("inventory only".into(), None);
    }
    if matches!(target.usage, UsageEstimate::Unknown { .. }) {
        return (
            if target.storage.is_empty() {
                "not measured"
            } else {
                "yield unknown"
            }
            .into(),
            None,
        );
    }
    usage_display(&target.usage)
}

/// An indeterminate spinner backed by the scanner's monotonic counters.
pub(crate) struct ScanProgressIndicator {
    bar: ProgressBar,
}

/// Spinner for a potentially long direct-delete action. Provider commands
/// stream their own output and therefore do not use this indicator.
pub(crate) struct ActionProgressIndicator {
    bar: ProgressBar,
}

impl ActionProgressIndicator {
    pub(crate) fn create(label: &str, enabled: bool) -> Option<Self> {
        enabled.then(|| {
            let bar = ProgressBar::new_spinner();
            bar.set_style(
                ProgressStyle::with_template("{spinner:.cyan} {msg}")
                    .expect("static action progress template"),
            );
            bar.set_message(format!("permanently deleting {label}"));
            bar.enable_steady_tick(Duration::from_millis(90));
            Self { bar }
        })
    }

    pub(crate) fn finish(self, status: cleanrs_core::execution::ActionStatus) {
        let message = match status {
            cleanrs_core::execution::ActionStatus::Success => "deletion complete",
            cleanrs_core::execution::ActionStatus::ValidationAbort => "deletion aborted",
            cleanrs_core::execution::ActionStatus::Failed => "deletion failed",
            cleanrs_core::execution::ActionStatus::Skipped => "deletion skipped",
        };
        self.bar.finish_with_message(message);
    }
}

impl ScanProgressIndicator {
    pub(crate) fn create(label: &str, enabled: bool) -> Option<Arc<Self>> {
        enabled.then(|| {
            let bar = ProgressBar::new_spinner();
            bar.set_style(
                ProgressStyle::with_template("{spinner:.cyan} {msg}  [{elapsed_precise}]")
                    .expect("static progress template is valid")
                    .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
            );
            bar.set_message(format!("Scanning {label}"));
            bar.enable_steady_tick(Duration::from_millis(80));
            bar.tick();
            Arc::new(Self { bar })
        })
    }

    pub(crate) fn message(&self, message: &str) {
        self.bar.set_message(message.to_owned());
    }

    pub(crate) fn finish(&self) {
        self.bar.finish_and_clear();
    }
}

impl ScanObserver for ScanProgressIndicator {
    fn on_progress(&self, progress: ScanProgress) {
        self.bar.set_message(format!(
            "Scanning · {} dirs · {} entries · {} allocated",
            grouped(progress.directories),
            grouped(progress.entries),
            human_bytes(progress.allocated_bytes)
        ));
    }
}

pub(crate) fn stderr_is_terminal() -> bool {
    io::stderr().is_terminal()
}

fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut output = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            output.push(',');
        }
        output.push(character);
    }
    output
}

/// Adapt pipeline notifications to the existing CLI scan indicator. Phase
/// counters are available to future views without adding redirected output.
pub(crate) struct CliPlanObserver {
    pub(crate) progress: Option<Arc<ScanProgressIndicator>>,
}
impl PlanObserver for CliPlanObserver {
    fn on_event(&self, event: PlanEvent) {
        match event {
            PlanEvent::PhaseStarted { phase } => {
                tracing::debug!(phase, "planning started");
                if let Some(indicator) = &self.progress {
                    indicator.bar.set_message(format!("Inspecting {phase}"));
                }
            }
            PlanEvent::PhaseFinished { phase, count } => {
                tracing::debug!(phase, count, "planning finished")
            }
            PlanEvent::ProviderInspection { provider } => {
                tracing::debug!(provider, "provider inspection");
                if let Some(indicator) = &self.progress {
                    indicator
                        .bar
                        .set_message(format!("Inspecting system provider: {provider}"));
                }
            }
            PlanEvent::CargoMetadata { completed, total } => {
                tracing::debug!(completed, total, "cargo metadata")
            }
        }
    }
    fn on_scan_progress(&self, progress: ScanProgress) {
        if let Some(indicator) = &self.progress {
            indicator.on_progress(progress);
        }
    }
}

pub(crate) struct CliExecutionObserver {
    json: bool,
    progress: Option<ActionProgressIndicator>,
}
impl CliExecutionObserver {
    pub(crate) fn new(json: bool) -> Self {
        Self {
            json,
            progress: None,
        }
    }
}
impl ExecutionObserver for CliExecutionObserver {
    fn on_event(&mut self, event: ExecutionEvent<'_>) {
        match event {
            ExecutionEvent::RunStarted { total } => tracing::debug!(total, "cleanup started"),
            ExecutionEvent::ActionStarted { index, target } => {
                tracing::debug!(index, target = target.id, "action started");
                match &target.operation {
                    PlannedOperation::DirectDelete { path, .. } => {
                        eprintln!(
                            "Applying {}: permanent delete {}",
                            target.id,
                            path.display()
                        );
                        self.progress = ActionProgressIndicator::create(
                            &target.id,
                            !self.json && stderr_is_terminal(),
                        );
                    }
                    PlannedOperation::Command { command } => {
                        eprintln!("Applying {}: argv = {:?}", target.id, command_argv(command))
                    }
                    PlannedOperation::Informational => {}
                }
            }
            ExecutionEvent::OutputLine {
                index,
                stream,
                line,
            } => {
                tracing::trace!(index, ?stream, "provider output");
                if !self.json {
                    match stream {
                        OutputStream::Stdout => {
                            let _ = io::stdout().lock().write_all(line);
                        }
                        OutputStream::Stderr => {
                            let _ = io::stderr().lock().write_all(line);
                        }
                    }
                }
            }
            ExecutionEvent::ActionFinished { index, result } => {
                tracing::debug!(index, status = ?result.status, "action finished");
                if let Some(progress) = self.progress.take() {
                    progress.finish(result.status);
                }
            }
            ExecutionEvent::RunFinished { summary } => {
                tracing::debug!(outcome = summary.outcome, "cleanup finished")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_relative_and_indented() {
        let root = PathBuf::from("/tmp/root");
        assert_eq!(display_path(&root, &root, 0), ".");
        assert_eq!(display_path(&root, &root.join("a/b"), 2), "  a/b");
    }

    #[test]
    fn counts_have_grouping() {
        assert_eq!(grouped(12), "12");
        assert_eq!(grouped(12_345_678), "12,345,678");
    }

    #[test]
    fn target_paths_keep_the_command_root_in_home_relative_form() {
        let home = Path::new("/Users/test");
        let target = Path::new("/Users/test/Developer/Projects/example/target");
        assert_eq!(
            home_relative_display(target, Some(home)),
            "~/Developer/Projects/example/target"
        );
        assert_eq!(home_relative_display(home, Some(home)), "~");
    }
}
