//! Ratatui baseline TUI: scrollback + prompt input + live assistant buffer.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::screen::split_main_input;

#[derive(Debug)]
pub struct App {
    pub scrollback: Vec<String>,
    pub input: String,
    /// `Some` for the duration of an in-flight assistant turn — the live
    /// stream relay appends `TextDelta`s here, and `finalize_assistant_turn`
    /// commits the buffer to scrollback as a single line.
    pub current_assistant: Option<String>,
}

impl App {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            scrollback: Vec::new(),
            input: String::new(),
            current_assistant: None,
        }
    }

    pub fn add_line(&mut self, prefix: &str, body: &str) {
        self.scrollback.push(format!("{prefix}{body}"));
    }

    /// Begin a new assistant turn — `append_to_current_assistant` deltas
    /// accumulate into the in-flight buffer until `finalize_assistant_turn`.
    pub fn start_assistant_turn(&mut self) {
        self.current_assistant = Some(String::new());
    }

    pub fn append_to_current_assistant(&mut self, delta: &str) {
        if let Some(buf) = &mut self.current_assistant {
            buf.push_str(delta);
        }
    }

    /// Commit the in-flight assistant buffer to scrollback under the standard
    /// prefix and clear the live buffer.
    pub fn finalize_assistant_turn(&mut self, turns: u32) {
        if let Some(text) = self.current_assistant.take() {
            self.scrollback.push(format!("origin ({turns} turns)> {text}"));
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

pub fn draw(f: &mut Frame<'_>, app: &App) {
    let (main, prompt) = split_main_input(f.area());

    let mut lines: Vec<Line> = app
        .scrollback
        .iter()
        .map(|s| Line::from(Span::raw(s.clone())))
        .collect();
    if let Some(buf) = app.current_assistant.as_ref() {
        lines.push(Line::from(Span::raw(format!("origin> {buf}"))));
    }
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
