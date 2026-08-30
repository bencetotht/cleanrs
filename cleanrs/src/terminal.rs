//! TTY-aware human output. JSON, completions, and man pages bypass this module.

use std::io::{self, IsTerminal};
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

use cleanrs_core::targets::{CandidateSafety, DetectedTarget, PlanSelection, UsageEstimate};
use cleanrs_core::{Confidence, DoctorReport, ScanObserver, ScanProgress};

use crate::{CleanOutput, Metric, ScanOutput, TargetsOutput, format_interval, human_bytes};

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

    pub(crate) fn print_targets(&self, output: &TargetsOutput) {
        println!(
            "{}  {}",
            self.heading("cleanrs targets"),
            self.muted(&output.scope.display().to_string())
        );
        println!();

        let mut targets = self.table();
        targets.set_header(vec![
            self.header("RECLAIMABLE"),
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
                CandidateSafety::Eligible if target.stale == Some(true) => {
                    self.value("eligible", Color::Green)
                }
                CandidateSafety::Eligible => self.value("not stale", Color::Yellow),
                CandidateSafety::Refused(reason) => {
                    self.value(format!("refused: {reason}"), Color::Red)
                }
            };
            let (usage, _) = usage_display(&target.usage);
            targets.add_row(vec![
                self.value(usage, Color::Cyan),
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
            self.muted(&output.scope.display().to_string())
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
                PlanSelection::Selected => ("plan", "selected", Color::Yellow),
                PlanSelection::Skipped(reason) => ("skip", reason.as_str(), Color::DarkGrey),
            };
            let (usage, _) = usage_display(&planned.target.usage);
            plan.add_row(vec![
                self.value(action, color),
                self.value(usage, Color::Cyan),
                Cell::new(target_project_name(&planned.target)),
                Cell::new(target_location(&planned.target)),
                Cell::new(&planned.target.id),
                Cell::new(reason),
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
            self.value(output.execution, Color::Green),
        ]);
        println!();
        println!("{summary}");
        self.print_provider_statuses(&output.providers);
        println!();
        println!(
            "{} Permanent execution remains disabled until Phase 3 identity revalidation and journalling are implemented.",
            self.warning("NOTICE")
        );
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

fn usage_display(usage: &UsageEstimate) -> (String, Option<Confidence>) {
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

/// An indeterminate spinner backed by the scanner's monotonic counters.
pub(crate) struct ScanProgressIndicator {
    bar: ProgressBar,
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
