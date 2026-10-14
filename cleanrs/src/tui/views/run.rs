use crate::tui::{
    app::{App, RunAction, Screen},
    keymap,
    theme::Theme,
};
use cleanrs_core::execution::{ActionResult, ActionStatus};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    text::Line,
    widgets::{Block, Borders, Gauge, Paragraph, Wrap},
};

pub fn result_text(result: &ActionResult) -> String {
    match result.status {
        ActionStatus::Success => result
            .removed_allocation
            .map(|b| format!("done · removed allocation {}", crate::human_bytes(b)))
            .or_else(|| {
                result
                    .provider_reported
                    .map(|b| format!("done · provider reported {}", crate::human_bytes(b)))
            })
            .unwrap_or_else(|| "done · no byte result reported".into()),
        ActionStatus::ValidationAbort => format!(
            "aborted · changed since scan: {}",
            result.error.as_deref().unwrap_or("validation mismatch")
        ),
        ActionStatus::Failed => format!(
            "failed: {}",
            result.error.as_deref().unwrap_or("unknown reason")
        ),
        ActionStatus::Skipped => format!("skipped: {}", result.error.as_deref().unwrap_or("")),
    }
}
pub fn result_style(result: &ActionResult, theme: Theme) -> ratatui::style::Style {
    match result.status {
        ActionStatus::Success => theme.success(),
        ActionStatus::Skipped => theme.dim(),
        _ => theme.error(),
    }
}
pub fn render(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let Some(run) = &app.run else {
        return;
    };
    let areas = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Percentage(45),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .split(frame.area());
    let done = run
        .actions
        .iter()
        .filter(|a| matches!(a, RunAction::Finished(_)))
        .count();
    let total = run.actions.len();
    frame.render_widget(
        Paragraph::new(format!("Cleanup · run {}", run.plan.run_id)).style(theme.accent()),
        areas[0],
    );
    let current = run
        .actions
        .iter()
        .position(|a| matches!(a, RunAction::Running))
        .map_or(done.min(total), |i| i + 1);
    frame.render_widget(
        Paragraph::new(format!(
            "⠹ action {current}/{total} · {done} completed{}",
            if run.stop.load(std::sync::atomic::Ordering::Acquire) {
                " · stopping after current action"
            } else {
                ""
            }
        )),
        areas[1],
    );
    frame.render_widget(
        Gauge::default()
            .ratio(if total == 0 {
                0.0
            } else {
                done as f64 / total as f64
            })
            .label(format!("{done}/{total}"))
            .gauge_style(theme.success()),
        areas[2],
    );
    let spinner = ["⠋", "⠙", "⠹", "⠸"][app.tick % 4];
    let lines: Vec<_> = run
        .plan
        .actions
        .iter()
        .zip(&run.actions)
        .skip(run.scroll)
        .map(|(action, state)| {
            let (text, style) = match state {
                RunAction::Pending => ("pending".into(), theme.dim()),
                RunAction::Running => (format!("{spinner} running"), theme.accent()),
                RunAction::Finished(result) => (result_text(result), result_style(result, theme)),
            };
            Line::styled(
                format!(
                    "{} · {} · {text}",
                    action.target.id,
                    action
                        .target
                        .path
                        .as_ref()
                        .map_or_else(String::new, |p| p.display().to_string())
                ),
                style,
            )
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::BOTTOM)),
        areas[3],
    );
    let height = areas[4].height.saturating_sub(2) as usize;
    let start = run
        .output
        .len()
        .saturating_sub(height)
        .saturating_sub(run.output_scroll);
    frame.render_widget(
        Paragraph::new(
            run.output
                .iter()
                .skip(start)
                .take(height)
                .map(|l| Line::raw(l.as_str()))
                .collect::<Vec<_>>(),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Provider stdout/stderr · last 200 lines"),
        ),
        areas[4],
    );
    let footer = if run.confirm_stop {
        Line::styled("Stop after the current action? y/n", theme.warning())
    } else {
        Line::styled(
            keymap::footer_for(Screen::Run, frame.area().width as usize),
            theme.dim(),
        )
    };
    frame.render_widget(
        Paragraph::new(vec![
            footer,
            Line::from(app.status.as_ref().map_or("", |s| s.0.as_str())),
        ]),
        areas[5],
    );
}
