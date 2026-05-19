//! Ratatui baseline TUI: scrollback + prompt input.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::screen::split_main_input;

#[derive(Debug)]
pub struct App {
    pub scrollback: Vec<String>,
    pub input: String,
}

impl App {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            scrollback: Vec::new(),
            input: String::new(),
        }
    }

    pub fn add_line(&mut self, prefix: &str, body: &str) {
        self.scrollback.push(format!("{prefix}{body}"));
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

pub fn draw(f: &mut Frame<'_>, app: &App) {
    let (main, prompt) = split_main_input(f.area());

    let lines: Vec<Line> = app
        .scrollback
        .iter()
        .map(|s| Line::from(Span::raw(s.clone())))
        .collect();
    let scroll = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title("origin"));
    f.render_widget(scroll, main);

    let input = Paragraph::new(Line::from(vec![
        Span::raw("> "),
        Span::styled(app.input.clone(), Style::default().add_modifier(Modifier::BOLD)),
    ]))
    .block(Block::default().borders(Borders::ALL));
    f.render_widget(input, prompt);
}
