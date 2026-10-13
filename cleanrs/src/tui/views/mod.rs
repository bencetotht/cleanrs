pub mod browser;
pub mod help;
pub mod results;
pub mod review;
pub mod run;
pub mod targets;

use super::{
    app::{App, Screen},
    theme::Theme,
};
use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Block, Borders, Clear, Paragraph},
};
use std::time::Duration;

pub fn stale(age: Duration) -> bool {
    age > Duration::from_secs(600)
}
pub fn freshness(age: Duration) -> String {
    let is_stale = stale(age);
    let seconds = age.as_secs();
    let age = if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h", seconds / 3600)
    };
    format!(
        "{age} ago{}",
        if is_stale {
            " · press r to refresh"
        } else {
            ""
        }
    )
}

pub fn render(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let area = frame.area();
    if area.width < 80 || area.height < 20 {
        let message = "terminal too small (need 80x20)";
        frame.render_widget(
            Paragraph::new(message).centered().style(theme.warning()),
            Rect::new(area.x, area.y + area.height / 2, area.width, 1),
        );
        if app.confirm_quit {
            quit_dialog(frame, theme);
        }
        return;
    }
    let screen = app
        .screens
        .iter()
        .rev()
        .find(|s| **s != Screen::Help)
        .copied()
        .unwrap_or(Screen::Browser);
    match screen {
        Screen::Browser => browser::render(frame, app, theme),
        Screen::Targets => targets::render(frame, app, theme),
        Screen::Review => review::render(frame, app, theme),
        Screen::Run => run::render(frame, app, theme),
        Screen::Results => results::render(frame, app, theme),
        Screen::Help => unreachable!(),
    }
    if app.screen() == Screen::Help {
        help::render(frame, app, theme);
    }
    // Errors remain visible on every screen, including help and results. Any
    // key dismisses in the event loop, while recognized keys still navigate.
    if let Some((message, true)) = &app.status {
        frame.render_widget(
            Paragraph::new(format!(
                "✗ {} · any key dismisses",
                truncate(message, area.width.saturating_sub(22) as usize)
            ))
            .style(theme.error()),
            Rect::new(area.x, area.y + 2, area.width, 1),
        );
    }
    if app.confirm_quit {
        quit_dialog(frame, theme);
    }
}

fn quit_dialog(frame: &mut Frame<'_>, theme: Theme) {
    let area = frame.area();
    let width = area.width.min(56);
    let height = area.height.min(7);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new("Leave the storage browser?\n\n[y] Quit    [n / Esc / Enter] Stay")
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Quit cleanrs? ")
                    .border_style(theme.accent()),
            )
            .wrap(ratatui::widgets::Wrap { trim: false }),
        popup,
    );
}

/// Truncate by terminal cell width, including wide Unicode filenames.
pub fn truncate(text: &str, width: usize) -> String {
    use ratatui::text::Span;
    if Span::raw(text).width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut output = String::new();
    let mut used = 0;
    for c in text.chars() {
        let size = Span::raw(c.to_string()).width();
        if used + size > width - 1 {
            break;
        }
        output.push(c);
        used += size;
    }
    output.push('…');
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::{
        app::Action,
        worker::{Row, Summary, WorkerEvent},
    };
    use cleanrs_core::{
        accounting::{Confidence, ReclaimEstimate},
        fs::{NodeFlags, NodeKind},
    };
    use ratatui::{Terminal, backend::TestBackend, style::Color};

    fn fixture(pending: bool) -> App {
        let mut app = App::new("/fixture".into());
        app.on_worker(WorkerEvent::Listed {
            path: "/fixture".into(),
            revision: 0,
            total: 8192,
            volume: "Free 42.0 GiB · APFS".into(),
            rows: vec![Row {
                path: "/fixture/project".into(),
                name: "project".into(),
                kind: NodeKind::Directory,
                modified: 0,
                own_flags: NodeFlags::empty(),
                summary: Summary {
                    allocated: 8192,
                    apparent: 16384,
                    targets: 2,
                    target_id: None,
                    flags: NodeFlags::DATALESS | NodeFlags::MOUNT_BOUNDARY,
                    incomplete: true,
                    measured: false,
                },
                pending,
                source: None,
                estimate: (!pending).then_some(ReclaimEstimate {
                    floor: Some(4096),
                    ceiling: Some(8192),
                    confidence: Confidence::Medium,
                    explanation: Some("cloned or hardlinked blocks widen the interval".into()),
                }),
            }],
        });
        app
    }
    fn buffer(app: &App, colors: bool, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render(frame, app, Theme { colors }))
            .unwrap();
        terminal.backend().buffer().clone()
    }
    fn text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    }
    #[test]
    fn partial_and_finished_browser_symbols_survive_no_color() {
        for colors in [false, true] {
            let partial = buffer(&fixture(true), colors, 100, 24);
            let display = text(&partial);
            assert!(display.contains("scanning…"));
            assert!(display.contains("0/1 children scanned"));
            assert!(display.contains("⠋ …"));
            assert!(display.contains("project/"));
            assert!(display.contains("◆ 2 targets"));
            assert!(display.contains("▶"));
            let finished = buffer(&fixture(false), colors, 100, 24);
            let display = text(&finished);
            assert!(display.contains("idle · scanned 0s ago"));
            assert!(display.contains("8.0 KiB"));
            assert!(display.contains("100%"));
            if !colors {
                assert!(
                    finished
                        .content
                        .iter()
                        .all(|cell| cell.fg == Color::Reset && cell.bg == Color::Reset)
                );
            } else {
                assert!(finished.content.iter().any(|cell| cell.fg == Color::Cyan));
            }
        }
    }
    #[test]
    fn info_panel_and_help_use_shared_accounting_and_binding_text() {
        for colors in [false, true] {
            let mut pending = fixture(true);
            pending.handle(Action::Info);
            assert!(
                text(&buffer(&pending, colors, 100, 30))
                    .contains("Allocated pending · Apparent pending")
            );
            let mut boundary = fixture(false);
            let row = &mut boundary
                .directories
                .get_mut(&boundary.current)
                .unwrap()
                .rows[0];
            row.estimate = None;
            row.own_flags = NodeFlags::MOUNT_BOUNDARY;
            row.summary.allocated = 0;
            boundary.handle(Action::Info);
            assert!(
                text(&buffer(&boundary, colors, 100, 30))
                    .contains("Allocated unknown · Apparent unknown")
            );
            let mut app = fixture(false);
            app.handle(Action::Info);
            let display = text(&buffer(&app, colors, 100, 30));
            assert!(display.contains("Immediately-private lower bound: 4.0 KiB"));
            assert!(display.contains("confidence Medium"));
            assert!(display.contains("cloned or hardlinked blocks"));
            app.handle(Action::Help);
            let display = text(&buffer(&app, colors, 100, 30));
            assert!(display.contains("Browser help"));
            assert!(display.contains("Ctrl-c"));
            assert!(display.contains("First row"));
            for binding in crate::tui::keymap::bindings(Screen::Browser) {
                assert!(display.contains(binding.label));
            }
        }
    }
    #[test]
    fn browser_badges_pluralize_and_identify_target_directory_rows() {
        let mut app = fixture(false);
        app.directories.get_mut(&app.current).unwrap().rows[0]
            .summary
            .targets = 1;
        assert!(text(&buffer(&app, false, 100, 24)).contains("◆ 1 target"));
        assert!(!text(&buffer(&app, false, 100, 24)).contains("◆ 1 targets"));
        app.directories.get_mut(&app.current).unwrap().rows[0]
            .summary
            .target_id = Some("node.modules".into());
        assert!(text(&buffer(&app, false, 100, 24)).contains("◆ node.modules"));
    }
    #[test]
    fn eighty_columns_keep_sizes_and_truncate_long_names_with_directory_suffix() {
        let mut app = fixture(false);
        app.directories.get_mut(&app.current).unwrap().rows[0].name =
            "long-directory-name".repeat(6);
        let display = text(&buffer(&app, false, 80, 24));
        assert!(display.contains("8.0 KiB"));
        assert!(display.contains("…/"));
        for badge in ["⚠cov", "⚠mount", "⚠cloud", "◆ 2 targets"] {
            assert!(display.contains(badge), "missing {badge}");
        }
        assert!(display.contains("q quit"));
        assert_eq!(truncate("日本語abcdef", 7), "日本語…");
    }
}
