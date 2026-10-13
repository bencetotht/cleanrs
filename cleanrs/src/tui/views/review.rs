use crate::tui::{
    app::{App, Screen, review_group},
    keymap,
    theme::Theme,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    text::Line,
    widgets::{Block, Borders, Paragraph, Wrap},
};

pub fn lines(app: &App, theme: Theme) -> Vec<Line<'static>> {
    let Some(plan) = &app.reviewed else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    if let Some(notice) = &app.review_notice {
        lines.push(Line::styled(notice.clone(), theme.warning()));
        lines.push(Line::raw(""));
    }
    for group in 0..3 {
        let actions: Vec<_> = plan
            .actions
            .iter()
            .filter(|a| review_group(&a.selection_source, a.target.explicit_only()) == group)
            .collect();
        if actions.is_empty() {
            continue;
        }
        let style = match group {
            0 => theme.success(),
            1 => theme.warning(),
            _ => theme.error(),
        };
        lines.push(Line::styled(
            match group {
                0 => "Selected by level",
                1 => "Manually selected — not eligible by default",
                _ => "Explicit-only / user data manually selected",
            },
            style,
        ));
        if group == 1 {
            lines.push(Line::styled(format!("These items would be skipped at level {} (reason shown per row). You selected them explicitly; they will be deleted.", format!("{:?}", plan.level).to_lowercase()), style));
        } else if group == 2 {
            lines.push(Line::styled("WARNING: User data or explicit-only state. It may be irreplaceable. You selected it explicitly; permanent deletion cannot be undone.", style));
        }
        for action in actions {
            let (exact, kind) = super::targets::operation(&action.target);
            let reason = match &action.selection_source {
                cleanrs_core::targets::SelectionSource::Manual {
                    overrode: Some(reason),
                } => reason.as_str(),
                _ => "policy-eligible",
            };
            lines.push(Line::styled(
                format!(
                    "{} · {} · {reason}",
                    action.target.id,
                    crate::terminal::usage_display(&action.target.usage).0
                ),
                style,
            ));
            lines.push(Line::from(format!("{kind}: {exact}")));
            lines.push(Line::from(action.target.consequence.clone()));
        }
        lines.push(Line::raw(""));
    }
    lines
}
pub fn render(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let Some(plan) = &app.reviewed else {
        return;
    };
    let areas = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(5),
    ])
    .split(frame.area());
    frame.render_widget(
        Paragraph::new("Review permanent cleanup · idle").style(theme.accent()),
        areas[0],
    );
    let rows = lines(app, theme);
    let scroll = app.review_scroll.min(rows.len().saturating_sub(1));
    frame.render_widget(
        Paragraph::new(rows.into_iter().skip(scroll).collect::<Vec<_>>())
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL)),
        areas[1],
    );
    let mut footer = vec![
        Line::from(super::targets::summary(plan)),
        Line::styled("Permanent deletion — not moved to Trash", theme.error()),
        Line::styled(
            keymap::footer_for(Screen::Review, frame.area().width as usize),
            theme.dim(),
        ),
    ];
    if let Some((message, error)) = &app.status {
        footer.push(Line::styled(
            message.clone(),
            if *error {
                theme.error()
            } else {
                theme.warning()
            },
        ));
    }
    frame.render_widget(Paragraph::new(footer).wrap(Wrap { trim: false }), areas[2]);
}
