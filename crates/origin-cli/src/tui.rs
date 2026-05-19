//! Ratatui baseline TUI: scrollback + prompt input + live assistant buffer.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::screen::split_main_input_status;
use crate::status::{render_line, UsageSnapshot};

#[derive(Debug)]
pub struct App {
    pub scrollback: Vec<String>,
    pub input: String,
    /// `Some` for the duration of an in-flight assistant turn — the live
    /// stream relay appends `TextDelta`s here, and `finalize_assistant_turn`
    /// commits the buffer to scrollback as a single line.
    pub current_assistant: Option<String>,
    pub usage: UsageSnapshot,
}

impl App {
    #[must_use]
    pub fn new(provider: &'static str, model: impl Into<String>) -> Self {
        Self {
            scrollback: Vec::new(),
            input: String::new(),
            current_assistant: None,
            usage: UsageSnapshot::new(provider, model),
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

    /// Accumulate one batch of token usage + wallclock elapsed into the
    /// running status snapshot. Called once per `Submit` cycle after
    /// `call_daemon` returns.
    pub fn record_usage(
        &mut self,
        input_tokens: u32,
        output_tokens: u32,
        cache_read: u32,
        cache_write: u32,
        elapsed: std::time::Duration,
    ) {
        self.usage.input_tokens = self.usage.input_tokens.saturating_add(input_tokens);
        self.usage.output_tokens = self.usage.output_tokens.saturating_add(output_tokens);
        self.usage.cache_read_input_tokens = self.usage.cache_read_input_tokens.saturating_add(cache_read);
        self.usage.cache_creation_input_tokens =
            self.usage.cache_creation_input_tokens.saturating_add(cache_write);
        self.usage.elapsed += elapsed;
    }
}

pub fn draw(f: &mut Frame<'_>, app: &App) {
    let (main, prompt, status) = split_main_input_status(f.area());

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

    let s = render_line(&app.usage);
    let status_p = Paragraph::new(Line::from(Span::raw(s)));
    f.render_widget(status_p, status);
}
