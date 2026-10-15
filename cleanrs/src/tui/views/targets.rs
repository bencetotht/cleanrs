//! Target rows are policy explanations, never an independent cleanup planner.
use crate::pipeline::{ReviewedPlan, SelectionSummary};
use crate::tui::{
    app::{App, Screen},
    keymap,
    theme::Theme,
};
use cleanrs_core::execution::command_argv;
use cleanrs_core::targets::{PlanSelection, PlannedOperation, PolicyStatus, UsageEstimate};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    text::Line,
    widgets::{Cell, Paragraph, Row, Table, TableState, Wrap},
};

pub fn summary(plan: &ReviewedPlan) -> String {
    summary_values(&SelectionSummary {
        count: plan.actions.len(),
        estimate: plan.selected_estimate.clone(),
        provider_reported: plan.selected_provider_reported,
        provider_actions: plan.selected_provider_actions,
        unknown_actions: plan.selected_unknown_actions,
    })
}
fn summary_values(summary: &SelectionSummary) -> String {
    format!(
        "Selected {} · {} · provider estimates {} ({} actions){}",
        summary.count,
        crate::format_interval(&summary.estimate),
        crate::human_bytes(summary.provider_reported),
        summary.provider_actions,
        if summary.unknown_actions > 0 {
            format!(" · {} unknown estimates", summary.unknown_actions)
        } else {
            String::new()
        }
    )
}
pub fn policy_label(
    row: &cleanrs_core::targets::PlannedTarget,
    level: cleanrs_core::targets::CleanupLevel,
) -> String {
    match row.policy_status(level) {
        PolicyStatus::Eligible => "eligible".into(),
        PolicyStatus::Refused(reason) => format!("refused: {reason}"),
        PolicyStatus::Informational => "refused: informational only".into(),
        PolicyStatus::NeedsLevel(level) => format!("needs {level:?}").to_lowercase(),
        PolicyStatus::ExplicitOnly => "explicit only".into(),
        PolicyStatus::NotSelectedByLevel => "not selected by level".into(),
        PolicyStatus::NotStale(days) => format!("not stale ({days}d)"),
        PolicyStatus::UnknownAge => "age unknown".into(),
        PolicyStatus::Skipped(reason) => reason.into(),
    }
}
pub fn operation(target: &cleanrs_core::targets::DetectedTarget) -> (String, &'static str) {
    match &target.operation {
        PlannedOperation::DirectDelete {
            path,
            children_only,
            ..
        } => (
            path.display().to_string(),
            if *children_only {
                "delete children"
            } else {
                "delete directory"
            },
        ),
        PlannedOperation::Command { command } => (
            serde_json::to_string(&command_argv(command)).expect("argv serializable"),
            "provider command",
        ),
        PlannedOperation::Informational => (
            target
                .path
                .as_ref()
                .map_or_else(|| target.description.clone(), |p| p.display().to_string()),
            "informational",
        ),
    }
}
pub fn render(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let Some(targets) = app.targets() else {
        return;
    };
    let rows = app.target_rows();
    let range = app.visual_keys();
    let cursor = rows
        .iter()
        .position(|r| Some(&r.candidate_key) == targets.highlighted.as_ref());
    let details = cursor.and_then(|i| rows.get(i)).map(|row| {
        let target = &row.target;
        let (exact, kind) = operation(target);
        // Optional explanations must not leave a dangling separator in any
        // usage-source variant (including provider and unknown estimates).
        let confidence_parts = match &target.usage {
            UsageEstimate::Filesystem { reclaimable, .. } => vec![
                format!("{:?}", reclaimable.confidence),
                reclaimable.explanation.clone().unwrap_or_default(),
            ],
            UsageEstimate::ProviderReported {
                confidence,
                explanation,
                ..
            } => vec![
                format!("{confidence:?}"),
                "provider reported".into(),
                explanation.clone(),
            ],
            UsageEstimate::Unknown { explanation } => vec!["unknown".into(), explanation.clone()],
        };
        let confidence = confidence_parts
            .into_iter()
            .filter(|part| !part.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let reason = if targets.selected.contains(&row.candidate_key) {
            format!("selected · {}", policy_label(row, targets.level))
        } else {
            policy_label(row, targets.level)
        };
        Paragraph::new(vec![
            Line::styled(format!("Target: {}", target.id), theme.accent()),
            Line::styled(exact, theme.accent()),
            Line::from(format!(
                "{kind} · age {} / threshold {}d · {reason}",
                target
                    .age_days
                    .map_or_else(|| "unknown".into(), |d| format!("{d}d")),
                target.stale_after_days
            )),
            Line::from(format!(
                "rebuild: {} · recoverability: {}",
                target.properties.rebuild_cost.map_or_else(
                    || "unknown".into(),
                    |cost| format!("{cost:?}").to_lowercase()
                ),
                match target.properties.recoverability {
                    cleanrs_core::targets::Recoverability::Generated => "generated",
                    cleanrs_core::targets::Recoverability::Downloadable => "downloadable",
                    cleanrs_core::targets::Recoverability::UserData => "user data",
                }
            )),
            Line::from(format!(
                "Occupied {} · {}",
                crate::terminal::occupied_display(target),
                target
                    .provider
                    .context
                    .as_deref()
                    .unwrap_or("local storage")
            )),
            Line::from(target.consequence.clone()),
            Line::from(format!(
                "Reclaimable {} · confidence {confidence}",
                crate::terminal::target_usage_display(target).0
            )),
        ])
        .wrap(Wrap { trim: false })
    });
    let footer_height = if frame.area().width < 100 { 4 } else { 3 };
    // Reserve enough space for the table header and a few rows. Measure wrapped
    // details so long target names and provider explanations can use extra lines.
    let details_height = details
        .as_ref()
        .map_or(0, |details| {
            details
                .line_count(frame.area().width)
                .min(u16::MAX as usize) as u16
        })
        .min(frame.area().height.saturating_sub(3 + 4 + footer_height));
    let areas = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(details_height),
        Constraint::Length(footer_height),
    ])
    .split(frame.area());
    let found = targets.baseline.as_ref().map_or(0, |p| p.targets.len());
    let metadata = format!(
        " level: {} · scope: {} · {found} found",
        format!("{:?}", targets.level).to_lowercase(),
        if targets.system { "system" } else { "folder" }
    );
    let header = Layout::horizontal([
        Constraint::Min(10),
        Constraint::Length(Line::raw(&metadata).width() as u16),
    ])
    .split(areas[0]);
    frame.render_widget(
        Paragraph::new(super::truncate(
            &format!(
                "Targets {}",
                if targets.system {
                    "system".into()
                } else {
                    app.target_scope.as_ref().unwrap().display().to_string()
                }
            ),
            header[0].width as usize,
        ))
        .style(theme.accent()),
        header[0],
    );
    frame.render_widget(Paragraph::new(metadata).style(theme.accent()), header[1]);
    frame.render_widget(
        Paragraph::new(if targets.planning {
            if targets.reviewing {
                format!(
                    "{} building reviewed plan… · {}",
                    super::spinner(app.tick),
                    targets.activity
                )
            } else {
                format!("{} {}", super::spinner(app.tick), targets.activity)
            }
        } else if targets.baseline.is_none() {
            "idle · detection unavailable · press r to refresh".into()
        } else {
            format!("idle · detected {}", super::freshness(targets.age()))
        })
        .style(if targets.planning {
            theme.accent()
        } else if targets.baseline.is_none() || super::stale(targets.age()) {
            theme.warning()
        } else {
            theme.dim()
        }),
        areas[1],
    );
    frame.render_widget(
        Paragraph::new(if let Some((message, error)) = &app.status {
            Line::styled(
                message.as_str(),
                if *error {
                    theme.error()
                } else {
                    theme.warning()
                },
            )
        } else {
            Line::from(format!(
                "{}filter: /{} · sort: {}",
                if targets.visual.is_some() {
                    "VISUAL · "
                } else {
                    ""
                },
                targets.filter,
                if targets.sort == crate::tui::app::Sort::Size {
                    "reclaimable"
                } else {
                    targets.sort.label()
                }
            ))
        }),
        areas[2],
    );
    let path_width = (frame.area().width.saturating_sub(60) / 2).max(12);
    let table = Table::new(
        rows.iter().map(|row| {
            let target = &row.target;
            let selected = targets.selected.contains(&row.candidate_key);
            let eligible = matches!(row.selection, PlanSelection::Selected { .. });
            let (mark, style) = if !target.manually_selectable() {
                ("✗", theme.error())
            } else if selected && !eligible {
                (
                    "[!]",
                    if target.explicit_only() {
                        theme.error()
                    } else {
                        theme.warning()
                    },
                )
            } else if selected {
                ("[x]", theme.success())
            } else {
                ("[ ]", theme.dim())
            };
            let path = target
                .project_root
                .as_ref()
                .or(target.path.as_ref())
                .map(|p| {
                    let relative = p
                        .strip_prefix(app.target_scope.as_ref().unwrap())
                        .unwrap_or(p);
                    if relative.as_os_str().is_empty() {
                        ".".into()
                    } else {
                        relative.display().to_string()
                    }
                })
                .unwrap_or_else(|| target.provider.id.clone());
            Row::new(vec![
                Cell::from(mark).style(style),
                Cell::from(crate::terminal::target_usage_display(target).0).style(
                    if target.usage.sortable_bytes() == 0 {
                        theme.dim()
                    } else {
                        ratatui::style::Style::default()
                    },
                ),
                Cell::from(super::truncate(&path, path_width as usize)),
                Cell::from(target.id.clone()),
                Cell::from(
                    target
                        .age_days
                        .map_or_else(|| "—".into(), |d| format!("{d}d")),
                ),
                Cell::from(policy_label(row, targets.level)).style(
                    if !target.manually_selectable() || target.explicit_only() {
                        theme.error()
                    } else if eligible {
                        theme.success()
                    } else {
                        theme.warning()
                    },
                ),
            ])
            .style(if range.contains(&row.candidate_key) {
                theme.accent()
            } else {
                ratatui::style::Style::default()
            })
        }),
        [
            Constraint::Length(3),
            Constraint::Length(if frame.area().width < 100 { 18 } else { 23 }),
            Constraint::Length(path_width),
            Constraint::Length(if frame.area().width < 100 { 14 } else { 17 }),
            Constraint::Length(if frame.area().width < 100 { 4 } else { 5 }),
            Constraint::Min(12),
        ],
    )
    .header(
        Row::new(["", "Reclaimable", "Project path", "Target", "Age", "Status"]).style(theme.dim()),
    )
    .row_highlight_style(theme.focus_background())
    .highlight_symbol("▶ ");
    let mut state = TableState::default().with_selected(cursor);
    frame.render_stateful_widget(table, areas[3], &mut state);
    if let Some(details) = details {
        frame.render_widget(details, areas[4]);
    }
    let total = targets.summary.as_ref().map_or_else(
        || {
            format!(
                "Selected {} · interval {}",
                targets.selected.len(),
                if targets.planning {
                    "pending…"
                } else {
                    "unavailable"
                }
            )
        },
        summary_values,
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(total),
            Line::styled(
                keymap::footer_for(Screen::Targets, frame.area().width as usize),
                theme.dim(),
            ),
        ])
        .wrap(Wrap { trim: false }),
        areas[5],
    );
}
