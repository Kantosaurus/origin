// SPDX-License-Identifier: Apache-2.0
//! Composer-driven TUI app state and draw routine.
//!
//! Features: unicode-width-aware wrapping, keyboard scrollback,
//! inline markdown (bold, headers, code), heading hierarchy,
//! code block backgrounds, side panel rendering.

use std::time::{Duration, Instant};

use origin_tui::composer::{Composer, PROMPT_ROWS};
use origin_tui::grid::{Attr, Cell, Grid};
use origin_tui::stream_widget::StreamWidget;
use origin_tui::widgets::plan_panel::PlanLine;
use unicode_width::UnicodeWidthChar;

use crate::autocomplete::CompletionSources;
use crate::status::UsageSnapshot;
use crate::suggestions::SuggestionState;
use crate::theme;

#[derive(Debug, Clone)]
pub struct ScrollLine {
    pub text: String,
    pub fg: u32,
    pub bg: u32,
    pub bold: bool,
}

impl ScrollLine {
    const fn styled(text: String, fg: u32, bg: u32, bold: bool) -> Self {
        Self { text, fg, bg, bold }
    }
}

/// Foreground/background/bold triple for a single styled-text write. Bundled so
/// [`write_str_styled`] takes one style parameter instead of three positional
/// color/flag arguments.
#[derive(Clone, Copy)]
struct Style {
    fg: u32,
    bg: u32,
    bold: bool,
}

const SPINNER_FRAMES: &[char] = &[
    '\u{280B}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283C}', '\u{2834}', '\u{2826}', '\u{2827}',
    '\u{2807}', '\u{280F}',
];
const SPINNER_INTERVAL_MS: u64 = 80;

/// Reserve a single row of breathing room below the scrollback so the last line
/// of output never sits flush against the input card. `finalize_assistant_turn`
/// also appends a trailing blank line after each LLM message, giving 2 rows of
/// separation for persistent content while only costing 1 visible row.
const INPUT_GAP_ROWS: u16 = 1;

#[derive(Debug)]
pub struct Spinner {
    pub active: bool,
    start: Instant,
}

impl Spinner {
    fn new() -> Self {
        Self {
            active: false,
            start: Instant::now(),
        }
    }

    pub fn start(&mut self) {
        self.active = true;
        self.start = Instant::now();
    }

    pub fn stop(&mut self) {
        self.active = false;
    }

    fn frame_char(&self) -> char {
        if !self.active {
            return ' ';
        }
        let elapsed = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let idx = (elapsed / SPINNER_INTERVAL_MS) as usize % SPINNER_FRAMES.len();
        SPINNER_FRAMES[idx]
    }
}

/// How long the daemon may go without emitting a single stream event during an
/// in-flight turn before the status line surfaces a stall warning.
///
/// Deliberately generous: extended-thinking turns and long non-streaming tools
/// can be quiet for a while, so this only fires on genuine, sustained silence.
pub const STALL_WARN_AFTER: Duration = Duration::from_secs(60);

/// Pure stall decision.
///
/// Given how long the turn has been quiet (no new daemon events) and the
/// warning threshold, return `Some(seconds_quiet)` to warn, or `None`. Kept
/// free of `Instant` so it is deterministically testable.
#[must_use]
pub fn stall_seconds(quiet: Duration, threshold: Duration) -> Option<u64> {
    if quiet >= threshold {
        Some(quiet.as_secs())
    } else {
        None
    }
}

#[derive(Debug)]
pub struct App {
    pub scrollback: Vec<ScrollLine>,
    pub input: String,
    pub cursor: usize,
    pub current_assistant: Option<String>,
    pub usage: UsageSnapshot,
    pub scroll_offset: usize,
    pub suggestions: SuggestionState,
    pub sources: CompletionSources,
    pub workflow: String,
    pub spinner: Spinner,
    /// `Some(start)` while a prompt turn is in flight. The status line adds
    /// `start.elapsed()` to `usage.elapsed` so seconds tick live during a
    /// turn without waiting for the final reply.
    pub turn_started: Option<Instant>,
    /// Bug #4: one-line status indicator for the active goal. `Some(s)`
    /// while a goal is running; `None` when cleared. Rendered above the
    /// input card by `draw`.
    pub goal_status: Option<String>,
    /// Stall watchdog: `Some(seconds_quiet)` when the render heartbeat has seen
    /// no daemon activity for [`STALL_WARN_AFTER`] during an in-flight turn.
    /// `None` whenever the daemon is producing output or no turn is running.
    /// Rendered as a high-visibility notice so a wedged daemon stops looking
    /// like an indefinitely-spinning spinner.
    pub stall_secs: Option<u64>,
    /// Session reasoning-effort level (`fast`/`low`/`medium`/`high`/`max`) as a
    /// canonical wire token, or `None` to leave the provider wire unchanged.
    /// Seeded from the startup `--effort` flag and mutated mid-session by the
    /// `/effort <level>` and `/fast` composer commands. Sent on every
    /// `PromptRequest`. *Closes: claude-code `/effort`+`/fast` (interactive).*
    pub effort: Option<String>,
    /// Active output style (Explanatory / Learning / Concise), or `None` for the
    /// default voice. Set by the `/output-style <name>` composer command; its
    /// system suffix is sent on every `PromptRequest` (in the `system` field) so
    /// the model adopts the style. *Closes: claude-code output styles.*
    pub output_style: Option<origin_outputstyle::Style>,
    /// Queued mid-turn steering hints (gemini model steering). The `/steer
    /// <text>` composer command pushes a hint here; the next real prompt drains
    /// the queue and merges the hints (in `<steering>` markers) ahead of the
    /// user's text. Empty ⇒ the prompt is sent unchanged. *Closes: gemini model
    /// steering (the queue+merge wire).*
    pub steering: origin_steering::SteeringQueue,
    /// Read-only "plan mode" (gemini Plan Mode). When `true`, every subsequent
    /// `PromptRequest` carries `read_only`, so the daemon denies all mutating
    /// tools for that turn. Toggled by the `/plan` composer command.
    pub plan_mode: bool,
    /// Multimodal attachments staged by `/attach <file>` for the next prompt
    /// (interactive parity with headless `origin run --attach`). Drained into
    /// the next `PromptRequest.attachments`; empty ⇒ text-only. *Closes: the
    /// interactive half of aider/gemini/claude image+PDF input.*
    pub pending_attachments: Vec<origin_multimodal::ContentBlock>,
    /// Extra workspace roots for this session (cline multi-root), seeded from
    /// the startup `--root` flags and sent on every `PromptRequest`. Empty ⇒
    /// single-root behaviour.
    pub workspace_roots: Vec<String>,
}

const BANNER: &[&str] = &[
    " \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2588}\u{2557}   \u{2588}\u{2588}\u{2557}",
    "\u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2550}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2550}\u{2550}\u{255D} \u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557}  \u{2588}\u{2588}\u{2551}",
    "\u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2554}\u{255D}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}  \u{2588}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2554}\u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2551}",
    "\u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}   \u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}\u{255A}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2551}",
    "\u{255A}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2554}\u{255D}\u{2588}\u{2588}\u{2551}  \u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}\u{255A}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2554}\u{255D}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551} \u{255A}\u{2588}\u{2588}\u{2588}\u{2588}\u{2551}",
    " \u{255A}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{255D} \u{255A}\u{2550}\u{255D}  \u{255A}\u{2550}\u{255D}\u{255A}\u{2550}\u{255D} \u{255A}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{255D} \u{255A}\u{2550}\u{255D}\u{255A}\u{2550}\u{255D}  \u{255A}\u{2550}\u{2550}\u{2550}\u{255D}",
];

impl App {
    #[must_use]
    pub fn new(provider: &'static str, model: impl Into<String>, sources: CompletionSources) -> Self {
        Self {
            scrollback: Vec::new(),
            input: String::new(),
            cursor: 0,
            current_assistant: None,
            usage: UsageSnapshot::new(provider, model),
            scroll_offset: 0,
            suggestions: SuggestionState::default(),
            sources,
            workflow: "Code".to_string(),
            spinner: Spinner::new(),
            turn_started: None,
            goal_status: None,
            stall_secs: None,
            effort: None,
            output_style: None,
            steering: origin_steering::SteeringQueue::new(),
            plan_mode: false,
            pending_attachments: Vec::new(),
            workspace_roots: Vec::new(),
        }
    }

    /// Start the live turn timer. Called when a user submission begins.
    pub fn start_turn_timer(&mut self) {
        self.turn_started = Some(Instant::now());
    }

    /// Stop the live timer and fold the elapsed delta into `usage.elapsed`
    /// so the status line transitions seamlessly from "ticking" to the
    /// final accumulated total.
    pub fn stop_turn_timer(&mut self) {
        if let Some(start) = self.turn_started.take() {
            self.usage.elapsed += start.elapsed();
        }
        // No turn in flight => no stall possible; clear any lingering notice.
        self.stall_secs = None;
    }

    /// A cheap fingerprint of everything a daemon stream event can change
    /// (scrollback rows, the in-flight assistant buffer, token counters). The
    /// render heartbeat compares this across ticks: if it stays unchanged for
    /// [`STALL_WARN_AFTER`] while a turn is active, the daemon has gone silent —
    /// a possible stall. The animating spinner frame is intentionally excluded
    /// so a silent-but-spinning UI still registers as "no activity".
    #[must_use]
    pub fn activity_signature(&self) -> u64 {
        const P: u64 = 1_099_511_628_211; // FNV prime, used only for mixing
        let mut s = self.scrollback.len() as u64;
        s = s
            .wrapping_mul(P)
            .wrapping_add(self.current_assistant.as_ref().map_or(0, String::len) as u64);
        s = s
            .wrapping_mul(P)
            .wrapping_add(u64::from(self.usage.output_tokens));
        s = s.wrapping_mul(P).wrapping_add(u64::from(self.usage.input_tokens));
        s
    }

    /// Apply a streaming usage delta. Mirrors `record_usage` but takes no
    /// elapsed value — used while a turn is in flight so the token counts
    /// and cost in the status line update as events stream in.
    pub fn record_usage_tokens(
        &mut self,
        input_tokens: u32,
        output_tokens: u32,
        cache_read: u32,
        cache_write: u32,
    ) {
        self.usage.input_tokens = self.usage.input_tokens.saturating_add(input_tokens);
        self.usage.output_tokens = self.usage.output_tokens.saturating_add(output_tokens);
        self.usage.cache_read_input_tokens = self.usage.cache_read_input_tokens.saturating_add(cache_read);
        self.usage.cache_creation_input_tokens =
            self.usage.cache_creation_input_tokens.saturating_add(cache_write);
    }

    pub fn recompute_suggestions(&mut self) {
        self.suggestions = crate::suggestions::suggest(&self.input, &self.sources);
    }

    pub fn push_banner(&mut self, cols: u16, rows: u16) {
        let main_rows = rows.saturating_sub(PROMPT_ROWS) as usize;
        let content_height = BANNER.len() + 4;
        let card_height = 4usize;
        let group = content_height + 2 + card_height;
        let top_pad = main_rows.saturating_sub(group) / 2;

        for _ in 0..top_pad {
            self.scrollback
                .push(ScrollLine::styled(String::new(), 0, 0, false));
        }
        for line in BANNER {
            let w = char_display_width(line) as usize;
            let pad = (cols as usize).saturating_sub(w) / 2;
            let padded = format!("{:>width$}{line}", "", width = pad);
            self.scrollback
                .push(ScrollLine::styled(padded, theme::ACCENT_DIM, 0, false));
        }
        for _ in 0..3 {
            self.scrollback
                .push(ScrollLine::styled(String::new(), 0, 0, false));
        }
        let tip = "\u{25CF} Tip  Type / to browse skills and workflows";
        let tw = char_display_width(tip) as usize;
        let tpad = (cols as usize).saturating_sub(tw) / 2;
        let padded_tip = format!("{:>width$}{tip}", "", width = tpad);
        self.scrollback
            .push(ScrollLine::styled(padded_tip, theme::MUTED, 0, false));
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
                self.scrollback
                    .push(ScrollLine::styled(String::new(), 0, 0, false));
                self.scrollback.push(ScrollLine::styled(
                    format!("\u{276F} {body}"),
                    theme::USER,
                    0,
                    true,
                ));
                self.scrollback
                    .push(ScrollLine::styled(String::new(), 0, 0, false));
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
                self.scrollback
                    .push(ScrollLine::styled(format!("  {body}"), theme::MUTED, 0, false));
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
                self.scrollback
                    .push(ScrollLine::styled(format!("    {body}"), theme::MUTED, 0, false));
            }
            _ => {
                self.scrollback
                    .push(ScrollLine::styled(format!("  {body}"), theme::BODY, 0, false));
            }
        }
        self.scroll_offset = 0;
    }

    pub fn add_colored_line(&mut self, text: String, fg: u32, bg: u32) {
        self.scrollback.push(ScrollLine::styled(text, fg, bg, false));
    }

    /// Bug #4: update the one-line goal status indicator. `None` clears it
    /// (rendered as no goal row above the input card).
    pub fn set_goal_status_line(&mut self, status: Option<String>) {
        self.goal_status = status;
    }

    pub fn add_tool_line(&mut self, text: String) {
        self.scrollback
            .push(ScrollLine::styled(text, theme::TOOL, 0, false));
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
                let mut in_code_block = false;
                for line in text.split('\n') {
                    let trimmed = line.trim_start();
                    if trimmed.starts_with("```") {
                        in_code_block = !in_code_block;
                        self.scrollback.push(ScrollLine::styled(
                            format!("  {line}"),
                            theme::MUTED,
                            if in_code_block { theme::CODE_BG } else { 0 },
                            false,
                        ));
                        continue;
                    }
                    if in_code_block {
                        self.scrollback.push(ScrollLine::styled(
                            format!("  {line}"),
                            theme::CODE_FG,
                            theme::CODE_BG,
                            false,
                        ));
                    } else {
                        let (fg, bold) = md_line_style(line);
                        self.scrollback
                            .push(ScrollLine::styled(format!("  {line}"), fg, 0, bold));
                    }
                }
                // Trailing blank line so the next user turn (or the input
                // card) has visible separation from this response.
                self.scrollback
                    .push(ScrollLine::styled(String::new(), 0, 0, false));
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

            let card_w = 75u16.min(cols.saturating_sub(4));
            let cl = cols.saturating_sub(card_w) / 2;
            let cr = cl + card_w;
            let cs = cl + 2;
            let text_w = cr.saturating_sub(cs) as usize;

            let wrapped = wrap_input_lines(&self.input, text_w);
            let line_count = clamp_u16(wrapped.len()).min(6);
            let card_h = line_count + 1;
            let card_total = card_h + 2;
            let at_bottom = rows.saturating_sub(card_total);
            let scrollback_limit = at_bottom.saturating_sub(INPUT_GAP_ROWS) as usize;

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
                wrap_into(&live_buf, theme::BODY, 0, false, cols_usize, &mut visual_lines);
            }

            let total = visual_lines.len();
            let visible = scrollback_limit;
            let max_offset = total.saturating_sub(visible);
            let offset = self.scroll_offset.min(max_offset);
            let skip = total.saturating_sub(visible).saturating_sub(offset);

            let mut row: u16 = 0;
            for vl in visual_lines.iter().skip(skip).take(visible) {
                if row >= at_bottom {
                    break;
                }
                render_md_line(main, row, vl.text, cols, vl.fg, vl.bg, vl.bold);
                row = row.saturating_add(1);
            }

            let r_top = row.saturating_add(2).min(at_bottom);
            let r_status = r_top + line_count;
            let r_cap = r_status + 1;
            let layout = CardLayout {
                cols,
                rows,
                cl,
                cr,
                cs,
                at_bottom,
                r_top,
                r_status,
                r_cap,
            };

            draw_scroll_indicator(main, &layout, offset);
            self.draw_notices(main, &layout);
            draw_input_card_bg(main, &layout);
            self.draw_input_text(main, &layout, &wrapped);
            self.draw_status_line(main, &layout);
            draw_keybind_hint(main, &layout);
            self.draw_suggestions_popup(main, &layout);
        }
        clear_prompt_grid(composer.prompt_grid());
    }

    /// Render the goal-status indicator and stall-watchdog notice on the
    /// breathing-room row just above the input card. The stall notice (when
    /// active) overpaints the goal status — a stall is the more urgent signal.
    fn draw_notices(&self, main: &mut Grid, layout: &CardLayout) {
        // Bug #4: render the one-line goal-status indicator (when set)
        // on the breathing-room row just above the input card. Centered
        // inside the card width so it visually associates with the
        // current operation rather than the scrollback above.
        if let Some(ref status) = self.goal_status {
            let status_row = layout.at_bottom.saturating_sub(1);
            if status_row < layout.rows {
                let pad = layout.cl.saturating_add(2);
                write_str_styled(
                    main,
                    status_row,
                    pad,
                    status,
                    layout.cr,
                    Style {
                        fg: theme::ACCENT,
                        bg: 0,
                        bold: false,
                    },
                );
            }
        }

        // Stall watchdog notice. When the render heartbeat has seen no
        // daemon activity for `STALL_WARN_AFTER`, surface a high-visibility
        // warning so a wedged daemon reads as "possibly stuck" instead of an
        // indefinitely-spinning spinner. Takes the row the goal status would
        // use — a stall is the more urgent signal.
        if let Some(secs) = self.stall_secs {
            let status_row = layout.at_bottom.saturating_sub(1);
            if status_row < layout.rows {
                let msg = format!("\u{26A0} no daemon activity for {secs}s \u{2014} Ctrl+C to interrupt");
                write_str_styled(
                    main,
                    status_row,
                    layout.cl.saturating_add(2),
                    &msg,
                    layout.cr,
                    Style {
                        fg: theme::RED,
                        bg: 0,
                        bold: false,
                    },
                );
            }
        }
    }

    /// Render the input card's text: the muted placeholder when empty, or the
    /// (last six) wrapped input lines plus the ghost-suggestion completion.
    fn draw_input_text(&self, main: &mut Grid, layout: &CardLayout, wrapped: &[&str]) {
        if self.input.is_empty() && self.current_assistant.is_none() {
            if layout.r_top < layout.rows {
                write_str_styled(
                    main,
                    layout.r_top,
                    layout.cs,
                    "Ask anything...",
                    layout.cr,
                    Style {
                        fg: theme::MUTED,
                        bg: theme::SURFACE_RAISED,
                        bold: false,
                    },
                );
            }
        } else if !self.input.is_empty() {
            let vis_start = if wrapped.len() > 6 { wrapped.len() - 6 } else { 0 };
            for (i, line) in wrapped[vis_start..].iter().enumerate() {
                let r = layout.r_top + clamp_u16(i);
                if r >= layout.r_status || r >= layout.rows {
                    break;
                }
                write_str_styled(
                    main,
                    r,
                    layout.cs,
                    line,
                    layout.cr,
                    Style {
                        fg: theme::BRIGHT,
                        bg: theme::SURFACE_RAISED,
                        bold: false,
                    },
                );
                if vis_start + i == wrapped.len() - 1 && !self.suggestions.ghost.is_empty() {
                    let gc = layout.cs + char_display_width(line);
                    write_str_styled(
                        main,
                        r,
                        gc,
                        &self.suggestions.ghost,
                        layout.cr,
                        Style {
                            fg: theme::DIM,
                            bg: theme::SURFACE_RAISED,
                            bold: false,
                        },
                    );
                }
            }
        }
    }

    /// Render the status line: spinner frame, workflow name, model, token
    /// counts, cost, and elapsed seconds, with the spinner glyph and workflow
    /// label overpainted in their accent colors.
    fn draw_status_line(&self, main: &mut Grid, layout: &CardLayout) {
        if layout.r_status < layout.rows {
            let cost = crate::status::cost_usd(&self.usage);
            let live_elapsed = self
                .turn_started
                .map_or(self.usage.elapsed, |t| self.usage.elapsed + t.elapsed());
            let secs = live_elapsed.as_secs_f64();
            let tok_in = format_tokens(self.usage.input_tokens);
            let tok_out = format_tokens(self.usage.output_tokens);

            let (prefix, status) = if self.spinner.active {
                let sc = self.spinner.frame_char();
                (
                    Some(sc),
                    format!(
                        "{sc} {} \u{00B7} {} \u{00B7} {tok_in}\u{2191} {tok_out}\u{2193} \u{00B7} ${cost:.3} \u{00B7} {secs:.1}s",
                        self.workflow, self.usage.model,
                    ),
                )
            } else {
                (
                    None,
                    format!(
                        "{} \u{00B7} {} \u{00B7} {tok_in}\u{2191} {tok_out}\u{2193} \u{00B7} ${cost:.3} \u{00B7} {secs:.1}s",
                        self.workflow, self.usage.model,
                    ),
                )
            };
            write_str_styled(
                main,
                layout.r_status,
                layout.cs,
                &status,
                layout.cr,
                Style {
                    fg: theme::DIM,
                    bg: theme::SURFACE_RAISED,
                    bold: false,
                },
            );
            if let Some(sc) = prefix {
                if let Some(pos) = status.find(sc) {
                    let c = layout.cs + clamp_u16(pos);
                    if c < layout.cr {
                        main.put(
                            layout.r_status,
                            c,
                            Cell::new(sc, theme::ACCENT, theme::SURFACE_RAISED, Attr::PLAIN),
                        );
                    }
                }
            }
            let wf_offset = if prefix.is_some() { 2u16 } else { 0u16 };
            for (i, ch) in self.workflow.chars().enumerate() {
                let c = layout.cs + wf_offset + clamp_u16(i);
                if c < layout.cr {
                    main.put(
                        layout.r_status,
                        c,
                        Cell::new(ch, theme::ACCENT, theme::SURFACE_RAISED, Attr::BOLD),
                    );
                }
            }
        }
    }

    /// Render the autocomplete suggestions popup directly above the input card,
    /// showing up to six candidates with the selected row highlighted.
    fn draw_suggestions_popup(&self, main: &mut Grid, layout: &CardLayout) {
        if !self.suggestions.candidates.is_empty() {
            let count = clamp_u16(self.suggestions.candidates.len().min(6));
            let popup_bottom = layout.r_top.saturating_sub(1);
            let popup_top = popup_bottom.saturating_sub(count);
            let selected = self.suggestions.selected;
            for (i, candidate) in self.suggestions.candidates.iter().take(6).enumerate() {
                let r = popup_top + clamp_u16(i);
                if r >= popup_bottom || r >= layout.rows {
                    break;
                }
                for c in layout.cl..layout.cr.min(layout.cols) {
                    main.put(r, c, Cell::new(' ', 0, theme::SURFACE_RAISED, Attr::PLAIN));
                }
                let (ind_fg, txt_fg) = if i == selected {
                    (theme::ACCENT, theme::BODY)
                } else {
                    (theme::MUTED, theme::MUTED)
                };
                let ind = if i == selected { " \u{25B8} " } else { "   " };
                let ps = layout.cl + 1;
                write_str_styled(
                    main,
                    r,
                    ps,
                    ind,
                    layout.cr,
                    Style {
                        fg: ind_fg,
                        bg: theme::SURFACE_RAISED,
                        bold: false,
                    },
                );
                let ind_w = char_display_width(ind);
                write_str_styled(
                    main,
                    r,
                    ps + ind_w,
                    candidate,
                    layout.cr,
                    Style {
                        fg: txt_fg,
                        bg: theme::SURFACE_RAISED,
                        bold: false,
                    },
                );
            }
        }
    }
}

/// Shared input-card geometry computed once in [`App::draw`] and threaded into
/// each card-rendering helper, so they take a single `&CardLayout` instead of a
/// long list of positional `u16` coordinates.
struct CardLayout {
    cols: u16,
    rows: u16,
    cl: u16,
    cr: u16,
    cs: u16,
    at_bottom: u16,
    r_top: u16,
    r_status: u16,
    r_cap: u16,
}

/// Render the "N more" scrollback indicator in the top-right corner when the
/// viewport is scrolled up.
fn draw_scroll_indicator(main: &mut Grid, layout: &CardLayout, offset: usize) {
    if offset > 0 {
        let indicator = format!(" \u{2191} {offset} more ");
        // Position by DISPLAY width, not byte length: the `\u{2191}` (↑)
        // arrow is 3 UTF-8 bytes but one terminal column, so `.len()`
        // would push the indicator two columns too far left.
        let indicator_w: usize = indicator
            .chars()
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(1))
            .sum();
        let start_col = layout
            .cols
            .saturating_sub(clamp_u16(indicator_w).saturating_add(1));
        write_str_styled(
            main,
            0,
            start_col,
            &indicator,
            layout.cols,
            Style {
                fg: theme::ACCENT,
                bg: theme::SURFACE_RAISED,
                bold: false,
            },
        );
    }
}

/// Paint the raised-surface background of the input card and its left accent
/// rule, spanning the card rows.
fn draw_input_card_bg(main: &mut Grid, layout: &CardLayout) {
    for r in layout.r_top..=layout.r_status.min(layout.rows.saturating_sub(1)) {
        for c in layout.cl..layout.cr.min(layout.cols) {
            main.put(r, c, Cell::new(' ', 0, theme::SURFACE_RAISED, Attr::PLAIN));
        }
        if layout.cl < layout.cols {
            main.put(
                r,
                layout.cl,
                Cell::new('\u{2503}', theme::ACCENT, theme::SURFACE_RAISED, Attr::PLAIN),
            );
        }
    }
}

/// Render the keybind hint line beneath the input card (and the card's bottom
/// accent corner), right-aligned within the card width.
fn draw_keybind_hint(main: &mut Grid, layout: &CardLayout) {
    if layout.r_cap < layout.rows {
        if layout.cl < layout.cols {
            main.put(
                layout.r_cap,
                layout.cl,
                Cell::new('\u{2579}', theme::ACCENT, 0, Attr::PLAIN),
            );
        }
        let hint_parts: &[(&str, u32)] = &[
            ("shift+enter", theme::BODY),
            (" newline  ", theme::MUTED),
            ("tab", theme::BODY),
            (" skills  ", theme::MUTED),
            ("ctrl+c", theme::BODY),
            (" quit", theme::MUTED),
        ];
        let total_hw: u16 = hint_parts.iter().map(|(s, _)| char_display_width(s)).sum();
        let mut hc = layout.cr.saturating_sub(total_hw);
        for (text, fg) in hint_parts {
            let tw = char_display_width(text);
            write_str_styled(
                main,
                layout.r_cap,
                hc,
                text,
                hc + tw,
                Style {
                    fg: *fg,
                    bg: 0,
                    bold: false,
                },
            );
            hc += tw;
        }
    }
}

/// Clear the (unused) prompt grid to the base surface color. The composer keeps
/// a second grid for a separate prompt region; the TUI renders everything into
/// the main grid, so this just blanks it each frame.
fn clear_prompt_grid(prompt: &mut Grid) {
    for r in 0..prompt.rows() {
        for c in 0..prompt.cols() {
            prompt.put(r, c, Cell::new(' ', 0, theme::SURFACE, Attr::PLAIN));
        }
    }
}

// Bug #4: implement the `goal_render::GoalRender` sink directly on `App`
// so `main.rs::call_daemon`'s event arm becomes a one-liner pass-through
// instead of a duplicated match on every Goal* variant.
impl crate::goal_render::GoalRender for App {
    fn push_colored(&mut self, text: String, fg: u32, _bg: u32) {
        self.scrollback.push(ScrollLine::styled(text, fg, 0, false));
        self.scroll_offset = 0;
    }
    fn set_goal_status(&mut self, status: Option<String>) {
        self.goal_status = status;
    }
}

/// Render plan steps and a vertical divider into the side panel grid.
pub fn draw_side(side: &mut Grid, plan_lines: &[PlanLine]) {
    let cols = side.cols();
    let rows = side.rows();

    for r in 0..rows {
        for c in 0..cols {
            side.put(r, c, Cell::new(' ', 0, theme::PANEL_BG, Attr::PLAIN));
        }
    }

    for r in 0..rows {
        side.put(
            r,
            0,
            Cell::new('\u{2502}', theme::BORDER, theme::PANEL_BG, Attr::PLAIN),
        );
    }

    if plan_lines.is_empty() {
        let label = " Plan";
        write_str_styled(
            side,
            0,
            1,
            label,
            cols.saturating_sub(1),
            Style {
                fg: theme::MUTED,
                bg: theme::PANEL_BG,
                bold: false,
            },
        );
        return;
    }

    let header = " Plan";
    write_str_styled(
        side,
        0,
        1,
        header,
        cols.saturating_sub(1),
        Style {
            fg: theme::PANEL_HEADER,
            bg: theme::PANEL_BG,
            bold: true,
        },
    );

    for c in 1..cols {
        side.put(
            1,
            c,
            Cell::new('\u{2500}', theme::BORDER, theme::PANEL_BG, Attr::PLAIN),
        );
    }
    side.put(
        1,
        0,
        Cell::new('\u{251C}', theme::BORDER, theme::PANEL_BG, Attr::PLAIN),
    );

    let mut row: u16 = 2;
    for pl in plan_lines {
        if row >= rows {
            break;
        }
        let glyph_fg = match pl.status_glyph {
            '\u{25CB}' => theme::MUTED,
            '\u{25D0}' => theme::ACCENT,
            '\u{25CF}' => theme::GREEN,
            '\u{2715}' => theme::RED,
            _ => theme::BODY,
        };
        side.put(
            row,
            2,
            Cell::new(pl.status_glyph, glyph_fg, theme::PANEL_BG, Attr::PLAIN),
        );
        write_str_styled(
            side,
            row,
            4,
            &pl.content,
            cols.saturating_sub(4),
            Style {
                fg: theme::BODY,
                bg: theme::PANEL_BG,
                bold: false,
            },
        );
        row += 1;
    }
}

/// Display width of a single char in terminal cells (`0`-width control chars
/// count as 1). Bounded to `u16`; no real glyph width approaches the clamp.
fn char_cell_width(c: char) -> u16 {
    u16::try_from(UnicodeWidthChar::width(c).unwrap_or(1)).unwrap_or(1)
}

/// Saturating `usize -> u16` for terminal geometry (rows/cols/indices). The
/// clamp is unreachable for real terminals but keeps the conversion both
/// panic-free and free of `cast_possible_truncation`.
fn clamp_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

fn char_display_width(s: &str) -> u16 {
    s.chars().map(char_cell_width).sum()
}

fn wrap_input_lines(text: &str, width: usize) -> Vec<&str> {
    if text.is_empty() {
        return vec![""];
    }
    let mut lines = Vec::new();
    for segment in text.split('\n') {
        if segment.is_empty() || width == 0 {
            lines.push(segment);
            continue;
        }
        let chars: Vec<char> = segment.chars().collect();
        let mut start = 0;
        let mut col_w = 0usize;
        for (idx, &ch) in chars.iter().enumerate() {
            let w = UnicodeWidthChar::width(ch).unwrap_or(1);
            if col_w + w > width && start < idx {
                let bs: usize = chars[..start].iter().map(|c| c.len_utf8()).sum();
                let be: usize = chars[..idx].iter().map(|c| c.len_utf8()).sum();
                lines.push(&segment[bs..be]);
                start = idx;
                col_w = 0;
            }
            col_w += w;
        }
        let bs: usize = chars[..start].iter().map(|c| c.len_utf8()).sum();
        lines.push(&segment[bs..]);
    }
    lines
}

fn md_line_style(line: &str) -> (u32, bool) {
    let trimmed = line.trim_start();
    if trimmed.starts_with("### ") {
        (theme::H3, true)
    } else if trimmed.starts_with("## ") {
        (theme::H2, true)
    } else if trimmed.starts_with("# ") {
        (theme::H1, true)
    } else if trimmed.starts_with("---") && trimmed.chars().all(|c| c == '-' || c == ' ') {
        (theme::RULE, false)
    } else if trimmed.starts_with("```") {
        (theme::MUTED, false)
    } else if trimmed.starts_with("> ") {
        (theme::ACCENT_DIM, false)
    } else {
        (theme::BODY, false)
    }
}

fn render_md_line(
    grid: &mut Grid,
    row: u16,
    text: &str,
    max_cols: u16,
    base_fg: u32,
    bg: u32,
    base_bold: bool,
) {
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
                    let w = char_cell_width(chars[i]);
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
                    let w = char_cell_width(chars[i]);
                    if col + w > max_cols {
                        break;
                    }
                    grid.put(
                        row,
                        col,
                        Cell::new(chars[i], theme::CODE_FG, theme::CODE_BG, Attr::PLAIN),
                    );
                    col += w;
                    i += 1;
                }
                i = end + 1;
                continue;
            }
        }

        let w = char_cell_width(chars[i]);
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

const fn find_closing(chars: &[char], start: usize, c1: char, c2: char) -> Option<usize> {
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
        format!("{:.1}M", f64::from(n) / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", f64::from(n) / 1_000.0)
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

fn wrap_into<'a>(text: &'a str, fg: u32, bg: u32, bold: bool, cols: usize, out: &mut Vec<VisualLine<'a>>) {
    for sub in text.split('\n') {
        if cols == 0 {
            continue;
        }
        let chars: Vec<char> = sub.chars().collect();
        if chars.is_empty() {
            out.push(VisualLine {
                text: "",
                fg,
                bg,
                bold,
            });
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

fn write_str_styled(grid: &mut Grid, row: u16, col: u16, s: &str, max_cols: u16, style: Style) {
    let attr = if style.bold { Attr::BOLD } else { Attr::PLAIN };
    let mut c = col;
    for ch in s.chars() {
        let w = char_cell_width(ch);
        if c + w > max_cols {
            break;
        }
        grid.put(row, c, Cell::new(ch, style.fg, style.bg, attr));
        c += w;
    }
    if style.bg != 0 {
        while c < max_cols {
            grid.put(row, c, Cell::new(' ', 0, style.bg, Attr::PLAIN));
            c += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_model_updates_usage_snapshot() {
        let mut app = App::new("anthropic", "claude-opus-4-7", CompletionSources::default());
        assert_eq!(app.usage.model, "claude-opus-4-7");
        app.set_model("claude-sonnet-4-6");
        assert_eq!(app.usage.model, "claude-sonnet-4-6");
    }

    #[test]
    fn set_model_does_not_reset_token_counters() {
        let mut app = App::new("anthropic", "claude-opus-4-7", CompletionSources::default());
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

    #[test]
    fn wrap_input_lines_single_line() {
        let lines = wrap_input_lines("hello world", 20);
        assert_eq!(lines, vec!["hello world"]);
    }

    #[test]
    fn wrap_input_lines_wraps_at_width() {
        let lines = wrap_input_lines("abcdefghij", 5);
        assert_eq!(lines, vec!["abcde", "fghij"]);
    }

    #[test]
    fn wrap_input_lines_preserves_newlines() {
        let lines = wrap_input_lines("abc\ndef", 10);
        assert_eq!(lines, vec!["abc", "def"]);
    }

    #[test]
    fn wrap_input_lines_empty() {
        let lines = wrap_input_lines("", 10);
        assert_eq!(lines, vec![""]);
    }

    #[test]
    fn stall_seconds_none_below_threshold() {
        assert_eq!(
            stall_seconds(Duration::from_secs(59), Duration::from_secs(60)),
            None
        );
    }

    #[test]
    fn stall_seconds_some_at_and_above_threshold() {
        assert_eq!(
            stall_seconds(Duration::from_secs(60), Duration::from_secs(60)),
            Some(60)
        );
        assert_eq!(
            stall_seconds(Duration::from_secs(125), Duration::from_secs(60)),
            Some(125)
        );
    }

    #[test]
    fn activity_signature_changes_on_new_output() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        let s0 = app.activity_signature();
        app.add_colored_line("hello".to_string(), 0, 0);
        assert_ne!(
            s0,
            app.activity_signature(),
            "new output must change the fingerprint"
        );
    }

    #[test]
    fn activity_signature_changes_on_token_usage() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        let s0 = app.activity_signature();
        app.record_usage_tokens(10, 5, 0, 0);
        assert_ne!(
            s0,
            app.activity_signature(),
            "token deltas must change the fingerprint"
        );
    }

    #[test]
    fn stop_turn_timer_clears_stall_notice() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.start_turn_timer();
        app.stall_secs = Some(90);
        app.stop_turn_timer();
        assert_eq!(app.stall_secs, None, "ending a turn must clear the stall notice");
    }
}
