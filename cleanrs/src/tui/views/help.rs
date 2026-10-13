use crate::tui::{
    app::{App, Screen},
    keymap,
    theme::Theme,
};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

pub fn render(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let area = frame.area();
    let width = area.width.min(76);
    let screen = app
        .screens
        .iter()
        .rev()
        .find(|s| **s != Screen::Help)
        .copied()
        .unwrap_or(Screen::Browser);
    let mut rows: Vec<_> = keymap::bindings(screen)
        .map(|binding| {
            Line::from(vec![
                Span::styled(format!("{:<22}", binding.label), theme.accent()),
                Span::raw(keymap::help_for(binding, screen)),
            ])
        })
        .collect();
    rows.push(Line::styled(
        "⚠cov incomplete · ⚠mount boundary · ⚠cloud dataless/synced · ⚠link firmlink",
        theme.dim(),
    ));
    let height = area.height.min(rows.len() as u16 + 2);
    let overlay = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, overlay);
    let max_scroll = rows.len().saturating_sub(height.saturating_sub(2) as usize);
    frame.render_widget(
        Paragraph::new(
            rows.into_iter()
                .skip(app.help_scroll.min(max_scroll))
                .collect::<Vec<_>>(),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("{screen:?} help · j/k scroll · ? / q / Esc close")),
        ),
        overlay,
    );
}
