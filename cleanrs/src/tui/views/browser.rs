use super::truncate;
use crate::tui::{
    app::{App, Screen},
    keymap,
    theme::Theme,
};
use cleanrs_core::fs::{NodeFlags, NodeKind};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row as TableRow, Table, TableState},
};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn render(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let directory = app.directory();
    let info_height = if app.info { 8 } else { 0 };
    let areas = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(2),
        Constraint::Length(info_height),
        Constraint::Length(2),
    ])
    .split(frame.area());
    let header = Layout::horizontal([
        Constraint::Min(10),
        Constraint::Length(directory.volume.len() as u16 + 1),
    ])
    .split(areas[0]);
    let path = format!("cleanrs  {}", app.current.display());
    frame.render_widget(
        Paragraph::new(truncate(&path, header[0].width as usize)).style(theme.accent()),
        header[0],
    );
    frame.render_widget(
        Paragraph::new(directory.volume.as_str()).style(theme.dim()),
        header[1],
    );
    let spinner = SPINNER[app.tick % SPINNER.len()];
    let activity = if directory.busy() {
        let mut dirs = 0_u64;
        let mut entries = 0_u64;
        let mut bytes = 0_u64;
        for progress in directory.progress.values() {
            dirs += progress.directories;
            entries += progress.entries;
            bytes = bytes.saturating_add(progress.allocated_bytes);
        }
        let complete = directory.rows.iter().filter(|row| !row.pending).count();
        format!(
            "{spinner} loading / scanning… {dirs} dirs · {entries} entries · {} · {complete}/{} children scanned",
            crate::human_bytes(bytes),
            directory.rows.len()
        )
    } else {
        format!(
            "idle · {} {} · allocated {}{}",
            if directory.cached {
                "cached scan"
            } else {
                "scanned"
            },
            super::freshness(directory.age()),
            crate::human_bytes(directory.total),
            if directory.coverage_error {
                " · ⚠ incomplete coverage"
            } else {
                ""
            }
        )
    };
    frame.render_widget(
        Paragraph::new(activity).style(if directory.busy() {
            theme.accent()
        } else if super::stale(directory.age()) {
            theme.warning()
        } else {
            theme.success()
        }),
        areas[1],
    );
    let status = if let Some((status, error)) = &app.status {
        Line::styled(
            status.as_str(),
            if *error {
                theme.error()
            } else {
                theme.warning()
            },
        )
    } else {
        Line::from(vec![
            Span::styled(
                format!("sort: {} · ◆ target counts are hints", app.sort.label()),
                theme.dim(),
            ),
            Span::raw(format!(
                "  {}{}",
                if app.filtering {
                    "filter (Enter confirms, Esc clears): /"
                } else {
                    "filter: /"
                },
                app.filter
            )),
        ])
    };
    frame.render_widget(Paragraph::new(status), areas[2]);
    let rows = app.visible();
    let selected = rows
        .iter()
        .position(|row| Some(&row.path) == directory.selected.as_ref());
    let name_width = areas[3].width.saturating_sub(68).max(12) as usize;
    let table_rows = rows.iter().map(|row| {
        let unknown = row.kind == NodeKind::Directory
            && !row.summary.measured
            && row.source.is_none()
            && row.estimate.is_none();
        let size = if row.pending {
            format!("{spinner} …")
        } else if row.kind == NodeKind::Directory
            && !row.summary.measured
            && row.source.is_none()
            && row.estimate.is_none()
        {
            "unknown".into()
        } else {
            crate::human_bytes(row.summary.allocated)
        };
        let ratio = if directory.total > 0 {
            row.summary.allocated as f64 / directory.total as f64
        } else {
            0.0
        };
        let bar = "█".repeat((ratio * 10.0).round().clamp(0.0, 10.0) as usize);
        let mut badges = Vec::new();
        if row.pending
            && row.summary.targets == 0
            && !row.summary.incomplete
            && !row
                .summary
                .flags
                .intersects(NodeFlags::MOUNT_BOUNDARY | NodeFlags::DATALESS | NodeFlags::FIRMLINK)
        {
            badges.push(Span::styled("scanning ", theme.dim()));
        }
        if row.summary.incomplete {
            badges.push(Span::styled("⚠cov ", theme.dim()));
        }
        if row.summary.flags.contains(NodeFlags::MOUNT_BOUNDARY) {
            badges.push(Span::styled("⚠mount ", theme.dim()));
        }
        if row.summary.flags.contains(NodeFlags::FIRMLINK) {
            badges.push(Span::styled("⚠link ", theme.dim()));
        }
        if row.summary.flags.contains(NodeFlags::DATALESS) || cloud_path(&row.path) {
            badges.push(Span::styled("⚠cloud ", theme.dim()));
        }
        if let Some(id) = &row.summary.target_id {
            badges.push(Span::styled(format!("◆ {id} "), theme.dim()));
        } else if row.summary.targets > 0 {
            badges.push(Span::styled(
                format!(
                    "◆ {} {} ",
                    row.summary.targets,
                    if row.summary.targets == 1 {
                        "target"
                    } else {
                        "targets"
                    }
                ),
                theme.dim(),
            ));
        }
        let name = if row.kind == NodeKind::Directory {
            format!("{}/", truncate(&row.name, name_width.saturating_sub(1)))
        } else {
            truncate(&row.name, name_width)
        };
        TableRow::new(vec![
            Cell::from(size).style(if row.pending || row.summary.allocated == 0 {
                theme.dim()
            } else {
                ratatui::style::Style::default()
            }),
            Cell::from(bar).style(theme.accent()),
            Cell::from(if row.pending || unknown {
                "…".into()
            } else {
                format!("{:3.0}%", ratio * 100.0)
            }),
            Cell::from(name).style(if row.kind == NodeKind::Directory {
                theme.accent()
            } else {
                ratatui::style::Style::default()
            }),
            Cell::from(Line::from(badges)),
        ])
    });
    let table = Table::new(
        table_rows,
        [
            Constraint::Length(11),
            Constraint::Length(10),
            Constraint::Length(4),
            Constraint::Length(name_width as u16),
            Constraint::Min(12),
        ],
    )
    .column_spacing(1)
    .row_highlight_style(theme.focus())
    .highlight_symbol("▶ ")
    .block(Block::default().borders(Borders::TOP | Borders::BOTTOM));
    let height = areas[3].height.saturating_sub(2).max(1) as usize;
    let mut state = TableState::default()
        .with_selected(selected)
        .with_offset(selected.unwrap_or(0).saturating_sub(height / 2));
    frame.render_stateful_widget(table, areas[3], &mut state);
    if app.info {
        let mut lines = Vec::new();
        if let Some(row) = app.highlighted() {
            lines.push(Line::styled(
                truncate(
                    &row.path.display().to_string(),
                    areas[4].width.saturating_sub(2) as usize,
                ),
                theme.accent(),
            ));
            let unavailable = row.kind == NodeKind::Directory
                && !row.summary.measured
                && row.source.is_none()
                && row.estimate.is_none();
            lines.push(Line::raw(if unavailable {
                format!(
                    "Allocated {} · Apparent {}",
                    if row.pending { "pending" } else { "unknown" },
                    if row.pending { "pending" } else { "unknown" }
                )
            } else {
                format!(
                    "Allocated {} · Apparent {}",
                    crate::human_bytes(row.summary.allocated),
                    crate::human_bytes(row.summary.apparent)
                )
            }));
            if let Some(estimate) = &row.estimate {
                lines.push(Line::raw(format!(
                    "Immediately-private lower bound: {}",
                    estimate
                        .floor
                        .map_or_else(|| "unknown".into(), crate::human_bytes)
                )));
                lines.push(Line::raw(format!(
                    "Reclaimable {} · confidence {:?}",
                    crate::format_interval(estimate),
                    estimate.confidence
                )));
                lines.push(Line::styled(
                    estimate
                        .explanation
                        .as_deref()
                        .unwrap_or("bounds agree within filesystem tolerance"),
                    theme.dim(),
                ));
            } else {
                lines.push(Line::styled(
                    if row.pending {
                        "Accounting pending: subtree still scanning"
                    } else if row.source.is_some() {
                        "Accounting pending…"
                    } else if row.summary.measured {
                        "Reclaimable unavailable · open folder or refresh for detailed accounting"
                    } else {
                        "Accounting unknown: subtree not traversed"
                    },
                    theme.dim(),
                ));
            }
            lines.push(Line::styled(
                "Read-only browser · target hints are not an authoritative cleanup plan",
                theme.dim(),
            ));
        } else {
            lines.push(Line::raw("No highlighted entry"));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .block(Block::default().borders(Borders::TOP))
                .wrap(ratatui::widgets::Wrap { trim: false }),
            areas[4],
        );
    }
    frame.render_widget(
        Paragraph::new(keymap::footer_for(
            Screen::Browser,
            frame.area().width as usize,
        ))
        .style(theme.dim())
        .wrap(ratatui::widgets::Wrap { trim: true }),
        areas[5],
    );
}

fn cloud_path(path: &std::path::Path) -> bool {
    // Hint only; mutation safety remains exclusively in the shared pipeline.
    path.components()
        .any(|part| part.as_os_str() == "CloudStorage" || part.as_os_str() == "Mobile Documents")
}
