use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy)]
pub struct Theme {
    pub colors: bool,
}

impl Theme {
    pub fn detect() -> Self {
        Self {
            colors: std::env::var_os("NO_COLOR").is_none(),
        }
    }

    pub fn accent(self) -> Style {
        self.color(Color::Cyan)
    }
    pub fn warning(self) -> Style {
        self.color(Color::Yellow)
    }
    pub fn error(self) -> Style {
        self.color(Color::Red)
    }
    pub fn success(self) -> Style {
        self.color(Color::Green)
    }
    pub fn dim(self) -> Style {
        if self.colors {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default()
        }
    }
    pub fn focus(self) -> Style {
        if self.colors {
            self.accent().bg(Color::DarkGray)
        } else {
            Style::default().add_modifier(Modifier::REVERSED)
        }
    }
    /// Tables with semantic cell colors focus via background only, preserving
    /// green selections and yellow/red policy warnings on the current row.
    pub fn focus_background(self) -> Style {
        if self.colors {
            Style::default().bg(Color::DarkGray)
        } else {
            Style::default().add_modifier(Modifier::REVERSED)
        }
    }
    fn color(self, color: Color) -> Style {
        if self.colors {
            Style::default().fg(color)
        } else {
            Style::default()
        }
    }
}
