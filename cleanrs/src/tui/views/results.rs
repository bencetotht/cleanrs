use crate::tui::{
    app::{App, Screen},
    keymap,
    theme::Theme,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    text::Line,
    widgets::{Paragraph, Wrap},
};

pub fn render(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let Some(run) = &app.run else {
        return;
    };
    let areas = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(4),
        Constraint::Min(2),
        Constraint::Length(2),
    ])
    .split(frame.area());
    frame.render_widget(
        Paragraph::new(format!("Results · run {}", run.plan.run_id)).style(theme.accent()),
        areas[0],
    );
    frame.render_widget(
        Paragraph::new(format!(
            "idle · {}",
            run.summary
                .as_ref()
                .map_or("execution error", |s| s.outcome.as_str())
        )),
        areas[1],
    );
    let mut removed = 0_u64;
    let mut reported = 0_u64;
    let mut failures = 0;
    let mut aborts = 0;
    let mut skips = 0;
    let mut lines = Vec::new();
    if let Some(summary) = &run.summary {
        for action in &summary.actions {
            removed = removed.saturating_add(action.removed_allocation.unwrap_or(0));
            reported = reported.saturating_add(action.provider_reported.unwrap_or(0));
            match action.status.as_str() {
                "failed" => failures += 1,
                "validation_abort" => aborts += 1,
                "skipped" => skips += 1,
                _ => {}
            }
            // Match history's evidence vocabulary, preserving planning skips.
            let evidence = action
                .removed_allocation
                .map(|b| format!("removed {} allocation", crate::human_bytes(b)))
                .or_else(|| {
                    action
                        .provider_reported
                        .map(|b| format!("provider reported {}", crate::human_bytes(b)))
                })
                .or_else(|| action.error.clone())
                .unwrap_or_else(|| action.status.clone());
            lines.push(Line::styled(
                format!("{} · {} · {evidence}", action.target_id, action.status),
                match action.status.as_str() {
                    "success" => theme.success(),
                    "skipped" => theme.dim(),
                    _ => theme.error(),
                },
            ));
        }
    }
    if let Some(error) = &run.error {
        lines.push(Line::styled(error.clone(), theme.error()));
    }
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!(
                "Removed allocation {} · provider reported {}",
                crate::human_bytes(removed),
                crate::human_bytes(reported)
            )),
            Line::from(format!(
                "Failures {failures} · validation_abort {aborts} · skipped {skips}"
            )),
            Line::styled(
                "Allocation removed is not observed APFS free-space recovery.",
                theme.dim(),
            ),
        ]),
        areas[2],
    );
    frame.render_widget(
        Paragraph::new(lines.into_iter().skip(run.scroll).collect::<Vec<_>>())
            .wrap(Wrap { trim: false }),
        areas[3],
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from("cleanrs history shows details"),
            Line::from(keymap::footer_for(
                Screen::Results,
                frame.area().width as usize,
            )),
        ])
        .style(theme.dim()),
        areas[4],
    );
}
