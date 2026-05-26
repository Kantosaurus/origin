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

    /// Replace the model name shown on the status line. Used by the
    /// `/model <name>` slash command to reflect the new active model
    /// without resetting the running token / cost counters.
    pub fn set_model(&mut self, model: impl Into<String>) {
        self.usage.model = model.into();
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
        let _ = widget;
        {
            let main = composer.main_grid();
            let cols = main.cols();
            let rows = main.rows();
            for r in 0..rows {
                for c in 0..cols {
                    main.put(r, c, Cell::blank());
                }
            }

            let mut visual_lines: Vec<&str> = Vec::new();
            for entry in &self.scrollback {
                for sub in entry.split('\n') {
                    if cols > 0 {
                        let chars: Vec<char> = sub.chars().collect();
                        if chars.is_empty() {
                            visual_lines.push("");
                        } else {
                            let mut start = 0;
                            while start < chars.len() {
                                let end = (start + cols as usize).min(chars.len());
                                let byte_start = chars[..start].iter().map(|c| c.len_utf8()).sum::<usize>();
                                let byte_end = chars[..end].iter().map(|c| c.len_utf8()).sum::<usize>();
                                visual_lines.push(&sub[byte_start..byte_end]);
                                start = end;
                            }
                        }
                    }
                }
            }
            if let Some(buf) = self.current_assistant.as_ref() {
                for sub in buf.split('\n') {
                    if cols > 0 {
                        let chars: Vec<char> = sub.chars().collect();
                        if chars.is_empty() {
                            visual_lines.push("");
                        } else {
                            let mut start = 0;
                            while start < chars.len() {
                                let end = (start + cols as usize).min(chars.len());
                                let byte_start = chars[..start].iter().map(|c| c.len_utf8()).sum::<usize>();
                                let byte_end = chars[..end].iter().map(|c| c.len_utf8()).sum::<usize>();
                                visual_lines.push(&sub[byte_start..byte_end]);
                                start = end;
                            }
                        }
                    }
                }
            }

            let total = visual_lines.len() as u16;
            let skip = total.saturating_sub(rows) as usize;
            let mut row: u16 = 0;
            for vl in visual_lines.iter().skip(skip) {
                if row >= rows {
                    break;
                }
                write_str(main, row, 0, vl, cols);
                row = row.saturating_add(1);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_model_updates_usage_snapshot() {
        let mut app = App::new("anthropic", "claude-opus-4-7");
        assert_eq!(app.usage.model, "claude-opus-4-7");
        app.set_model("claude-sonnet-4-6");
        assert_eq!(app.usage.model, "claude-sonnet-4-6");
    }

    #[test]
    fn set_model_does_not_reset_token_counters() {
        // Accumulated usage must survive a model swap — otherwise the
        // status bar would zero out mid-session every time the user runs
        // `/model`, which is misleading. (Pricing is per-model lookup,
        // so the cost reading after a swap reflects new model's rates
        // applied to the running token totals — that's intentional.)
        let mut app = App::new("anthropic", "claude-opus-4-7");
        app.record_usage(100, 50, 0, 0, std::time::Duration::from_millis(200));
        app.set_model("claude-sonnet-4-6");
        assert_eq!(app.usage.input_tokens, 100);
        assert_eq!(app.usage.output_tokens, 50);
        assert_eq!(app.usage.model, "claude-sonnet-4-6");
    }
}
