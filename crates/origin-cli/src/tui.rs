//! Composer-driven TUI app state and draw routine.

use std::time::Duration;

use origin_tui::composer::Composer;
use origin_tui::grid::{Cell, Grid};
use origin_tui::stream_widget::StreamWidget;

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
        elapsed: Duration,
    ) {
        self.usage.input_tokens = self.usage.input_tokens.saturating_add(input_tokens);
        self.usage.output_tokens = self.usage.output_tokens.saturating_add(output_tokens);
        self.usage.cache_read_input_tokens = self.usage.cache_read_input_tokens.saturating_add(cache_read);
        self.usage.cache_creation_input_tokens =
            self.usage.cache_creation_input_tokens.saturating_add(cache_write);
        self.usage.elapsed += elapsed;
    }

    /// Render the current app state into the composer's grids.
    ///
    /// `widget` is threaded through for future per-delta streaming use;
    /// this method does direct cell writes for simplicity.
    pub fn draw(&self, composer: &mut Composer, widget: &mut StreamWidget) {
        let _ = widget; // future: live-delta cursor mgmt
                        // Main pane
        {
            let main = composer.main_grid();
            let cols = main.cols();
            let rows = main.rows();
            // Clear
            for r in 0..rows {
                for c in 0..cols {
                    main.put(r, c, Cell::blank());
                }
            }
            let mut row: u16 = 0;
            for line in &self.scrollback {
                if row >= rows {
                    break;
                }
                write_str(main, row, 0, line, cols);
                row = row.saturating_add(1);
            }
            if let Some(buf) = self.current_assistant.as_ref() {
                if row < rows {
                    write_str(main, row, 0, buf, cols);
                }
            }
        }
        // Prompt bar
        {
            let prompt = composer.prompt_grid();
            let pcols = prompt.cols();
            let prows = prompt.rows();
            for r in 0..prows {
                for c in 0..pcols {
                    prompt.put(r, c, Cell::blank());
                }
            }
            let status_line = render_line(&self.usage);
            write_str(prompt, 0, 0, &status_line, pcols);
            // Input echo on next row, prefixed "> "
            let mut input_line = String::from("> ");
            input_line.push_str(&self.input);
            if prows >= 2 {
                write_str(prompt, 1, 0, &input_line, pcols);
            }
        }
    }
}

fn write_str(grid: &mut Grid, row: u16, col: u16, s: &str, max_cols: u16) {
    let mut c = col;
    for ch in s.chars() {
        if c >= max_cols {
            break;
        }
        grid.put(row, c, Cell::glyph(ch));
        c = c.saturating_add(1);
    }
}
