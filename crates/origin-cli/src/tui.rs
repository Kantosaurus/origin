//! Composer-driven TUI app state and draw routine.

use std::time::Duration;

use origin_tui::composer::Composer;
use origin_tui::grid::{Attr, Cell, Grid};
use origin_tui::stream_widget::StreamWidget;

use crate::status::{render_line, UsageSnapshot};
use crate::theme;

#[derive(Debug, Clone)]
pub struct ScrollLine {
    pub text: String,
    pub fg: u32,
    pub bg: u32,
    pub bold: bool,
}

impl ScrollLine {
    fn plain(text: String) -> Self {
        Self { text, fg: 0, bg: 0, bold: false }
    }

    fn styled(text: String, fg: u32, bg: u32, bold: bool) -> Self {
        Self { text, fg, bg, bold }
    }
}

#[derive(Debug)]
pub struct App {
    pub scrollback: Vec<ScrollLine>,
    pub input: String,
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

    pub fn set_model(&mut self, model: impl Into<String>) {
        self.usage.model = model.into();
    }

    pub fn add_line(&mut self, prefix: &str, body: &str) {
        let (fg, bold) = match prefix {
            "you> " => (theme::ACCENT, true),
            "error> " => (theme::RED, false),
            "system> " => (theme::MUTED, false),
            "mem> " => (theme::ACCENT_DIM, false),
            "tab> " => (theme::MUTED, false),
            "ok> " => (theme::GREEN, false),
            _ => (theme::BODY, false),
        };
        self.scrollback.push(ScrollLine::styled(
            format!("{prefix}{body}"),
            fg,
            0,
            bold,
        ));
    }

    pub fn add_colored_line(&mut self, text: String, fg: u32, bg: u32) {
        self.scrollback.push(ScrollLine::styled(text, fg, bg, false));
    }

    pub fn add_tool_line(&mut self, text: String) {
        self.scrollback
            .push(ScrollLine::styled(text, theme::YELLOW, 0, true));
    }

    pub fn start_assistant_turn(&mut self) {
        self.current_assistant = Some(String::new());
    }

    pub fn append_to_current_assistant(&mut self, delta: &str) {
        if let Some(buf) = &mut self.current_assistant {
            buf.push_str(delta);
        }
    }

    pub fn finalize_assistant_turn(&mut self, turns: u32) {
        if let Some(text) = self.current_assistant.take() {
            if !text.is_empty() {
                let prefix = format!("origin ({turns} turns)> ");
                let mut lines = text.split('\n');
                if let Some(first) = lines.next() {
                    self.scrollback.push(ScrollLine::styled(
                        format!("{prefix}{first}"),
                        theme::BODY,
                        0,
                        false,
                    ));
                }
                for rest in lines {
                    self.scrollback.push(ScrollLine::styled(
                        rest.to_string(),
                        theme::BODY,
                        0,
                        false,
                    ));
                }
            }
        }
    }

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

            let cols_usize = cols as usize;
            let mut visual_lines: Vec<VisualLine<'_>> = Vec::new();

            for entry in &self.scrollback {
                wrap_into(
                    &entry.text,
                    entry.fg,
                    entry.bg,
                    entry.bold,
                    cols_usize,
                    &mut visual_lines,
                );
            }
            if let Some(buf) = self.current_assistant.as_ref() {
                wrap_into(buf, theme::BODY, 0, false, cols_usize, &mut visual_lines);
            }

            let total = visual_lines.len() as u16;
            let skip = total.saturating_sub(rows) as usize;
            let mut row: u16 = 0;
            for vl in visual_lines.iter().skip(skip) {
                if row >= rows {
                    break;
                }
                write_str_styled(main, row, 0, vl.text, cols, vl.fg, vl.bg, vl.bold);
                row = row.saturating_add(1);
            }
        }
        // Status bar + prompt
        {
            let prompt = composer.prompt_grid();
            let pcols = prompt.cols();
            let prows = prompt.rows();
            // Fill status bar row with surface color
            for c in 0..pcols {
                prompt.put(0, c, Cell::new(' ', 0, theme::SURFACE, Attr::PLAIN));
            }
            if prows >= 2 {
                for c in 0..pcols {
                    prompt.put(1, c, Cell::blank());
                }
            }

            let status_line = render_line(&self.usage);
            write_str_styled(prompt, 0, 0, &status_line, pcols, theme::MUTED, theme::SURFACE, false);

            // Highlight the model name in the status bar with accent color
            if let Some(pos) = status_line.find(&self.usage.model) {
                let col_start = pos as u16;
                for (i, ch) in self.usage.model.chars().enumerate() {
                    let c = col_start + i as u16;
                    if c < pcols {
                        prompt.put(0, c, Cell::new(ch, theme::ACCENT, theme::SURFACE, Attr::BOLD));
                    }
                }
            }

            if prows >= 2 {
                // Copper-colored prompt prefix
                let prefix = "> ";
                write_str_styled(prompt, 1, 0, prefix, pcols, theme::ACCENT, 0, true);
                let prefix_len = prefix.len() as u16;
                write_str_styled(
                    prompt,
                    1,
                    prefix_len,
                    &self.input,
                    pcols.saturating_sub(prefix_len),
                    theme::BRIGHT,
                    0,
                    false,
                );
            }
        }
    }
}

struct VisualLine<'a> {
    text: &'a str,
    fg: u32,
    bg: u32,
    bold: bool,
}

fn wrap_into<'a>(
    text: &'a str,
    fg: u32,
    bg: u32,
    bold: bool,
    cols: usize,
    out: &mut Vec<VisualLine<'a>>,
) {
    for sub in text.split('\n') {
        if cols == 0 {
            continue;
        }
        let chars: Vec<char> = sub.chars().collect();
        if chars.is_empty() {
            out.push(VisualLine { text: "", fg, bg, bold });
        } else {
            let mut start = 0;
            while start < chars.len() {
                let end = (start + cols).min(chars.len());
                let byte_start: usize = chars[..start].iter().map(|c| c.len_utf8()).sum();
                let byte_end: usize = chars[..end].iter().map(|c| c.len_utf8()).sum();
                out.push(VisualLine {
                    text: &sub[byte_start..byte_end],
                    fg,
                    bg,
                    bold,
                });
                start = end;
            }
        }
    }
}

fn write_str_styled(
    grid: &mut Grid,
    row: u16,
    col: u16,
    s: &str,
    max_cols: u16,
    fg: u32,
    bg: u32,
    bold: bool,
) {
    let attr = if bold { Attr::BOLD } else { Attr::PLAIN };
    let mut c = col;
    for ch in s.chars() {
        if c >= max_cols {
            break;
        }
        grid.put(row, c, Cell::new(ch, fg, bg, attr));
        c = c.saturating_add(1);
    }
    if bg != 0 {
        while c < max_cols {
            grid.put(row, c, Cell::new(' ', 0, bg, Attr::PLAIN));
            c = c.saturating_add(1);
        }
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
        let mut app = App::new("anthropic", "claude-opus-4-7");
        app.record_usage(100, 50, 0, 0, std::time::Duration::from_millis(200));
        app.set_model("claude-sonnet-4-6");
        assert_eq!(app.usage.input_tokens, 100);
        assert_eq!(app.usage.output_tokens, 50);
        assert_eq!(app.usage.model, "claude-sonnet-4-6");
    }
}
