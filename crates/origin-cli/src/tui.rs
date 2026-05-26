//! Composer-driven TUI app state and draw routine.
//!
//! Features: unicode-width-aware wrapping, keyboard scrollback,
//! inline markdown (bold, headers), Burnished Copper theme.

use std::time::Duration;

use origin_tui::composer::Composer;
use origin_tui::grid::{Attr, Cell, Grid};
use origin_tui::stream_widget::StreamWidget;
use unicode_width::UnicodeWidthChar;

use crate::status::UsageSnapshot;
use crate::theme;

#[derive(Debug, Clone)]
pub struct ScrollLine {
    pub text: String,
    pub fg: u32,
    pub bg: u32,
    pub bold: bool,
}

impl ScrollLine {
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
    pub scroll_offset: usize,
}

impl App {
    #[must_use]
    pub fn new(provider: &'static str, model: impl Into<String>) -> Self {
        Self {
            scrollback: Vec::new(),
            input: String::new(),
            current_assistant: None,
            usage: UsageSnapshot::new(provider, model),
            scroll_offset: 0,
        }
    }

    pub fn set_model(&mut self, model: impl Into<String>) {
        self.usage.model = model.into();
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(n);
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn add_line(&mut self, prefix: &str, body: &str) {
        match prefix {
            "you> " => {
                self.scrollback.push(ScrollLine::styled(String::new(), 0, 0, false));
                self.scrollback.push(ScrollLine::styled(
                    format!("\u{276F} {body}"),
                    theme::BRIGHT,
                    theme::SURFACE,
                    true,
                ));
                self.scrollback.push(ScrollLine::styled(String::new(), 0, 0, false));
            }
            "error> " => {
                self.scrollback.push(ScrollLine::styled(
                    format!("  \u{2718} {body}"),
                    theme::RED,
                    0,
                    false,
                ));
            }
            "system> " => {
                self.scrollback.push(ScrollLine::styled(
                    format!("  {body}"),
                    theme::MUTED,
                    0,
                    false,
                ));
            }
            "ok> " => {
                self.scrollback.push(ScrollLine::styled(
                    format!("  \u{2714} {body}"),
                    theme::GREEN,
                    0,
                    false,
                ));
            }
            "mem> " => {
                self.scrollback.push(ScrollLine::styled(
                    format!("  {body}"),
                    theme::ACCENT_DIM,
                    0,
                    false,
                ));
            }
            "tab> " => {
                self.scrollback.push(ScrollLine::styled(
                    format!("    {body}"),
                    theme::MUTED,
                    0,
                    false,
                ));
            }
            _ => {
                self.scrollback.push(ScrollLine::styled(
                    format!("  {body}"),
                    theme::BODY,
                    0,
                    false,
                ));
            }
        }
        self.scroll_offset = 0;
    }

    pub fn add_colored_line(&mut self, text: String, fg: u32, bg: u32) {
        self.scrollback.push(ScrollLine::styled(text, fg, bg, false));
    }

    pub fn add_tool_line(&mut self, text: String) {
        self.scrollback
            .push(ScrollLine::styled(text, theme::ACCENT_DIM, 0, false));
    }

    pub fn start_assistant_turn(&mut self) {
        self.current_assistant = Some(String::new());
    }

    pub fn append_to_current_assistant(&mut self, delta: &str) {
        if let Some(buf) = &mut self.current_assistant {
            buf.push_str(delta);
        }
        self.scroll_offset = 0;
    }

    pub fn finalize_assistant_turn(&mut self, _turns: u32) {
        if let Some(text) = self.current_assistant.take() {
            if !text.is_empty() {
                for line in text.split('\n') {
                    let (fg, bold) = md_line_style(line);
                    self.scrollback.push(ScrollLine::styled(
                        format!("  {line}"),
                        fg,
                        0,
                        bold,
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
            let live_buf;
            if let Some(buf) = self.current_assistant.as_ref() {
                live_buf = format!("  {buf}");
                wrap_into(&live_buf, theme::BRIGHT, 0, false, cols_usize, &mut visual_lines);
            }

            let total = visual_lines.len();
            let visible = rows as usize;
            let max_offset = total.saturating_sub(visible);
            let offset = self.scroll_offset.min(max_offset);
            let skip = total.saturating_sub(visible).saturating_sub(offset);

            let mut row: u16 = 0;
            for vl in visual_lines.iter().skip(skip).take(visible) {
                if row >= rows {
                    break;
                }
                render_md_line(main, row, vl.text, cols, vl.fg, vl.bg, vl.bold);
                row = row.saturating_add(1);
            }

            if offset > 0 {
                let indicator = format!(" \u{2191} {offset} more ");
                let start_col = cols.saturating_sub(indicator.len() as u16 + 1);
                write_str_styled(main, 0, start_col, &indicator, cols, theme::ACCENT, theme::SURFACE, false);
            }
        }
        {
            let prompt = composer.prompt_grid();
            let pcols = prompt.cols();
            let prows = prompt.rows();

            for r in 0..prows {
                for c in 0..pcols {
                    prompt.put(r, c, Cell::new(' ', 0, theme::SURFACE, Attr::PLAIN));
                }
            }

            let cost = crate::status::cost_usd(&self.usage);
            let secs = self.usage.elapsed.as_secs_f64();
            let tok_in = format_tokens(self.usage.input_tokens);
            let tok_out = format_tokens(self.usage.output_tokens);

            let status = format!(
                " {} \u{2502} {tok_in} in \u{00B7} {tok_out} out \u{2502} ${cost:.3} \u{00B7} {secs:.1}s",
                self.usage.model,
            );
            write_str_styled(prompt, 0, 0, &status, pcols, theme::DIM, theme::SURFACE, false);

            if let Some(pos) = status.find(&self.usage.model) {
                let col_start = pos as u16;
                for (i, ch) in self.usage.model.chars().enumerate() {
                    let c = col_start + i as u16;
                    if c < pcols {
                        prompt.put(0, c, Cell::new(ch, theme::ACCENT, theme::SURFACE, Attr::PLAIN));
                    }
                }
            }

            if prows >= 2 {
                let arrow = "\u{276F} ";
                write_str_styled(prompt, 1, 0, arrow, pcols, theme::ACCENT, theme::SURFACE, true);
                let arrow_len = char_display_width(arrow);
                write_str_styled(
                    prompt,
                    1,
                    arrow_len,
                    &self.input,
                    pcols.saturating_sub(arrow_len),
                    theme::BRIGHT,
                    theme::SURFACE,
                    false,
                );
            }
        }
    }
}

fn char_display_width(s: &str) -> u16 {
    s.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(1) as u16)
        .sum()
}

fn md_line_style(line: &str) -> (u32, bool) {
    let trimmed = line.trim_start();
    if trimmed.starts_with("# ") || trimmed.starts_with("## ") || trimmed.starts_with("### ") {
        (theme::BRIGHT, true)
    } else if trimmed.starts_with("---") && trimmed.chars().all(|c| c == '-' || c == ' ') {
        (theme::BORDER, false)
    } else if trimmed.starts_with("```") {
        (theme::MUTED, false)
    } else if trimmed.starts_with("> ") {
        (theme::ACCENT_DIM, false)
    } else if trimmed.starts_with("| ") || trimmed.starts_with("|---") || trimmed.starts_with("|:") {
        (theme::BODY, false)
    } else {
        (theme::BODY, false)
    }
}

fn render_md_line(grid: &mut Grid, row: u16, text: &str, max_cols: u16, base_fg: u32, bg: u32, base_bold: bool) {
    let attr_plain = if base_bold { Attr::BOLD } else { Attr::PLAIN };
    let mut col: u16 = 0;
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len && col < max_cols {
        if chars[i] == '*' && i + 1 < len && chars[i + 1] == '*' {
            let close = find_closing(&chars, i + 2, '*', '*');
            if let Some(end) = close {
                i += 2;
                while i < end && col < max_cols {
                    let w = UnicodeWidthChar::width(chars[i]).unwrap_or(1) as u16;
                    if col + w > max_cols {
                        break;
                    }
                    grid.put(row, col, Cell::new(chars[i], theme::BRIGHT, bg, Attr::BOLD));
                    col += w;
                    i += 1;
                }
                i = end + 2;
                continue;
            }
        }
        if chars[i] == '`' && !(i + 1 < len && chars[i + 1] == '`') {
            if let Some(end) = chars[i + 1..].iter().position(|&c| c == '`').map(|p| i + 1 + p) {
                i += 1;
                while i < end && col < max_cols {
                    let w = UnicodeWidthChar::width(chars[i]).unwrap_or(1) as u16;
                    if col + w > max_cols {
                        break;
                    }
                    grid.put(row, col, Cell::new(chars[i], theme::ACCENT, bg, Attr::PLAIN));
                    col += w;
                    i += 1;
                }
                i = end + 1;
                continue;
            }
        }

        let w = UnicodeWidthChar::width(chars[i]).unwrap_or(1) as u16;
        if col + w > max_cols {
            break;
        }
        grid.put(row, col, Cell::new(chars[i], base_fg, bg, attr_plain));
        col += w;
        i += 1;
    }

    if bg != 0 {
        while col < max_cols {
            grid.put(row, col, Cell::new(' ', 0, bg, Attr::PLAIN));
            col += 1;
        }
    }
}

fn find_closing(chars: &[char], start: usize, c1: char, c2: char) -> Option<usize> {
    let mut j = start;
    while j + 1 < chars.len() {
        if chars[j] == c1 && chars[j + 1] == c2 {
            return Some(j);
        }
        j += 1;
    }
    None
}

fn format_tokens(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
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
            continue;
        }
        let mut start_idx = 0;
        let mut col_width = 0usize;
        let mut end_idx = 0;
        while end_idx < chars.len() {
            let w = UnicodeWidthChar::width(chars[end_idx]).unwrap_or(1);
            if col_width + w > cols {
                let byte_start: usize = chars[..start_idx].iter().map(|c| c.len_utf8()).sum();
                let byte_end: usize = chars[..end_idx].iter().map(|c| c.len_utf8()).sum();
                out.push(VisualLine {
                    text: &sub[byte_start..byte_end],
                    fg,
                    bg,
                    bold,
                });
                start_idx = end_idx;
                col_width = 0;
            }
            col_width += w;
            end_idx += 1;
        }
        if start_idx < chars.len() {
            let byte_start: usize = chars[..start_idx].iter().map(|c| c.len_utf8()).sum();
            out.push(VisualLine {
                text: &sub[byte_start..],
                fg,
                bg,
                bold,
            });
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
        let w = UnicodeWidthChar::width(ch).unwrap_or(1) as u16;
        if c + w > max_cols {
            break;
        }
        grid.put(row, c, Cell::new(ch, fg, bg, attr));
        c += w;
    }
    if bg != 0 {
        while c < max_cols {
            grid.put(row, c, Cell::new(' ', 0, bg, Attr::PLAIN));
            c += 1;
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

    #[test]
    fn wrap_respects_unicode_width() {
        let mut lines = Vec::new();
        wrap_into("ab\u{276F}cd", 0, 0, false, 4, &mut lines);
        assert_eq!(lines.len(), 2, "wide char should cause wrap at col 4");
    }
}
