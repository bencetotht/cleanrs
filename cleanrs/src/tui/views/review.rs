use crate::tui::{
    app::{App, Screen, review_group},
    keymap,
    theme::Theme,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

pub fn lines(app: &App, theme: Theme) -> Vec<Line<'static>> {
    let Some(plan) = &app.reviewed else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    let mut number = 0;
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
            format!(
                "{} · {} {}",
                match group {
                    0 => "Selected by level",
                    1 => "Manually selected — not eligible by default",
                    _ => "Explicit-only / user data manually selected",
                },
                actions.len(),
                if actions.len() == 1 {
                    "action"
                } else {
                    "actions"
                }
            ),
            style.add_modifier(Modifier::BOLD),
        ));
        if group == 1 {
            lines.push(Line::styled(format!("These items would be skipped at level {} (reason shown per row). You selected them explicitly; they will be deleted.", format!("{:?}", plan.level).to_lowercase()), style));
        } else if group == 2 {
            lines.push(Line::styled("WARNING: User data or explicit-only state. It may be irreplaceable. You selected it explicitly; permanent deletion cannot be undone.", style));
        }
        lines.push(Line::styled("─".repeat(48), theme.dim()));
        for action in actions {
            number += 1;
            let (exact, kind) = super::targets::operation(&action.target);
            let reason = match &action.selection_source {
                cleanrs_core::targets::SelectionSource::Manual {
                    overrode: Some(reason),
                } => reason.as_str(),
                _ => "policy-eligible",
            };
            lines.push(Line::styled(
                format!(
                    "{number:02}  {} · {} · {reason}",
                    action.target.id,
                    crate::terminal::usage_display(&action.target.usage).0
                ),
                style,
            ));
            lines.push(Line::from(vec![
                Span::styled(format!("    {kind}: "), theme.dim()),
                Span::raw(exact),
            ]));
            let (_, confidence) = crate::terminal::usage_display(&action.target.usage);
            let explanation = match &action.target.usage {
                cleanrs_core::targets::UsageEstimate::Filesystem { reclaimable, .. } => {
                    reclaimable.explanation.as_deref().unwrap_or("")
                }
                cleanrs_core::targets::UsageEstimate::ProviderReported { explanation, .. }
                | cleanrs_core::targets::UsageEstimate::Unknown { explanation } => explanation,
            };
            lines.push(Line::styled(
                format!(
                    "    Confidence: {}{}",
                    confidence.map_or_else(|| "unknown".into(), |c| format!("{c:?}")),
                    if explanation.is_empty() {
                        String::new()
                    } else {
                        format!(" · {explanation}")
                    }
                ),
                theme.dim(),
            ));
            lines.push(Line::from(vec![
                Span::styled("    After cleanup: ", theme.dim()),
                Span::raw(action.target.consequence.clone()),
            ]));
            lines.push(Line::raw(""));
        }
        lines.push(Line::raw(""));
    }
    lines
}
pub fn scroll_limit(app: &App, width: u16, height: usize) -> usize {
    Paragraph::new(lines(app, Theme { colors: false }))
        .wrap(Wrap { trim: false })
        .line_count(width)
        .saturating_sub(height)
}

pub fn render(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let Some(plan) = &app.reviewed else {
        return;
    };
    let areas = Layout::vertical([
        Constraint::Length(5),
        Constraint::Min(3),
        Constraint::Length(4),
    ])
    .split(frame.area());
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                "Review permanent cleanup",
                theme.accent().add_modifier(Modifier::BOLD),
            ),
            Line::styled(
                super::truncate(
                    &format!(
                        "Scope: {} · level {}",
                        plan.scope.display(),
                        format!("{:?}", plan.level).to_lowercase()
                    ),
                    areas[0].width as usize,
                ),
                theme.dim(),
            ),
            Line::from(super::targets::summary(plan)),
            Line::styled("Check each action below before confirming.", theme.dim()),
        ])
        .wrap(Wrap { trim: false }),
        areas[0],
    );
    let scroll = app.review_scroll.min(scroll_limit(
        app,
        areas[1].width.saturating_sub(2),
        areas[1].height.saturating_sub(2) as usize,
    ));
    frame.render_widget(
        Paragraph::new(lines(app, theme))
            .scroll((scroll.min(u16::MAX as usize) as u16, 0))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme.dim())
                    .title(" Planned actions · ↑/↓ scroll "),
            ),
        areas[1],
    );
    let mut footer = vec![
        Line::styled("Permanent deletion — not moved to Trash", theme.error()),
        Line::from("Press y to confirm · n to return to selection"),
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
