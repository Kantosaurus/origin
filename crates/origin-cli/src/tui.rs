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
use crate::input::VimMode;
use crate::status::UsageSnapshot;
use crate::suggestions::SuggestionState;
use crate::theme::{self, Theme};

/// An in-flight permission ask surfaced by the daemon (opt-in `/permissions`).
///
/// `Some` while the user is being asked to approve a tool; the next `y`/`n`
/// answers it. Rendered as a prompt above the input card.
#[derive(Debug, Clone)]
pub struct PendingPermission {
    pub id: u64,
    pub tool: String,
    pub args: String,
}

#[derive(Debug, Clone)]
pub struct ScrollLine {
    pub text: String,
    pub fg: u32,
    pub bg: u32,
    pub bold: bool,
    /// When `true`, the line is drawn verbatim with no inline-markdown parsing.
    /// Set for pre-formatted tool/diff/command output so source bytes that
    /// contain `**` or backticks are never reinterpreted as bold/code styling
    /// (a diff must show the literal bytes). Prose (assistant turns) stays
    /// `false` so markdown still renders.
    pub literal: bool,
}

impl ScrollLine {
    const fn styled(text: String, fg: u32, bg: u32, bold: bool) -> Self {
        Self { text, fg, bg, bold, literal: false }
    }

    /// A pre-formatted line drawn verbatim (no markdown parsing). Used for
    /// tool headers, diff rows, and streamed command output.
    const fn verbatim(text: String, fg: u32, bg: u32) -> Self {
        Self { text, fg, bg, bold: false, literal: true }
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

/// Quiet time before the soft "still working…" reassurance tier appears.
///
/// Short enough to answer the "is this still going?" doubt that creeps in after
/// ~10s of a silent spinner, without the alarm of the hard tier.
pub const STALL_SOFT_AFTER: Duration = Duration::from_secs(11);

/// Quiet time before the hard "no daemon activity" alarm (with the interrupt
/// hint). Was 60s — a full silent minute is well past the doubt threshold, so
/// the watchdog fired too late to be reassuring.
pub const STALL_WARN_AFTER: Duration = Duration::from_secs(28);

/// Which stall notice (if any) to show after `quiet` seconds of daemon silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallTier {
    /// Gentle reassurance — a long turn may just be thinking. No interrupt hint.
    Soft(u64),
    /// Sustained silence — likely wedged; surface the Ctrl+C interrupt hint.
    Hard(u64),
}

/// Pure stall decision: classify `quiet` against the soft/hard thresholds.
///
/// `None` below `soft`, `Soft` between, `Hard` at/above `hard`. Kept free of
/// `Instant` so it is deterministically testable.
#[must_use]
pub fn stall_tier(quiet: Duration, soft: Duration, hard: Duration) -> Option<StallTier> {
    let secs = quiet.as_secs();
    if quiet >= hard {
        Some(StallTier::Hard(secs))
    } else if quiet >= soft {
        Some(StallTier::Soft(secs))
    } else {
        None
    }
}

/// Pure desktop-notification gate (aider L107 OS-notification parity).
///
/// Returns whether a turn-completion desktop notification should fire. Two
/// inputs gate it: `enabled` (the resolved opt-in flag — `ORIGIN_NOTIFY_DESKTOP=1`
/// or a config flag) and `succeeded` (whether the turn ended cleanly). A failed
/// turn already surfaces an error line, so we only chime on success. Default
/// (`enabled == false`) ⇒ `false` ⇒ no spawn ⇒ byte-identical.
#[must_use]
pub const fn should_notify(enabled: bool, succeeded: bool) -> bool {
    enabled && succeeded
}

/// Whether the opt-in desktop-notification layer is active for this session.
///
/// True when `ORIGIN_NOTIFY_DESKTOP=1` or `config_flag` is set. Mirrors the
/// daemon's `ORIGIN_NOTIFY` opt-in but uses a CLI-specific variable so the two
/// surfaces can be toggled independently. Default-off ⇒ no notification.
#[must_use]
pub fn desktop_notify_enabled(config_flag: bool) -> bool {
    config_flag || std::env::var("ORIGIN_NOTIFY_DESKTOP").as_deref() == Ok("1")
}

/// Fire a best-effort desktop notification for a completed turn.
///
/// Gated by [`should_notify`]; when it returns `false` this is a no-op (no
/// process spawn, no observable effect). Otherwise it builds the OS-native
/// notifier command via [`origin_notify::desktop_command`] and spawns it,
/// swallowing every error — a missing notifier binary must never disturb the
/// session. Returns `true` when a spawn was attempted, for tests/telemetry.
#[must_use]
pub fn notify_turn_complete(enabled: bool, succeeded: bool) -> bool {
    if !should_notify(enabled, succeeded) {
        return false;
    }
    let n = origin_notify::Notification::new("origin", "Turn complete", false);
    let (program, cmd_args) = origin_notify::desktop_command(&n);
    let _ = std::process::Command::new(program).args(cmd_args).spawn();
    true
}

// App-state aggregate: each bool is an independent, unrelated session toggle
// (plan mode, vim, desktop notify, permission prompting). Grouping them into a
// sub-struct would obscure rather than clarify.
#[allow(clippy::struct_excessive_bools)]
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
    /// Stall watchdog: the [`StallTier`] when the render heartbeat has seen no
    /// daemon activity for [`STALL_SOFT_AFTER`]/[`STALL_WARN_AFTER`] during an
    /// in-flight turn. `None` whenever the daemon is producing output or no turn
    /// is running. Rendered as a notice so a quiet/wedged daemon stops looking
    /// like an indefinitely-spinning spinner.
    pub stall: Option<StallTier>,
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
    /// Live "prompt cache went cold" state (jcode parity). Tracks the wall-clock
    /// end of the previous turn and whether any prior turn had a warm cache, so a
    /// new turn whose gap exceeds [`origin_cost::PROMPT_CACHE_TTL_MS`] — or whose
    /// usage reports zero cache reads after a warm turn — flips
    /// `cache_cold` on for that turn. Cleared on the next warm turn. Purely
    /// additive to the status line; byte-identical when warm or unused.
    cache_cold: CacheColdState,
    /// Opt-in vim input mode (aider L107). [`VimMode::Insert`] is the default
    /// and reproduces today's direct-insert composer; the caller only consults
    /// the vim reducer when [`Self::vim_active`] is set, so a default session is
    /// byte-identical. Toggled by the `/vim` composer command or `ORIGIN_VIM=1`.
    pub vim_mode: VimMode,
    /// Whether the vim layer is active this session. `false` ⇒ the vim reducer
    /// is never consulted and input is byte-identical.
    pub vim_active: bool,
    /// Active color preset (aider L107). [`Theme::Default`] reproduces the
    /// legacy "Burnished Copper" constants verbatim, so the default render path
    /// is byte-identical; changed only by the `/theme <name>` composer command.
    pub theme: Theme,
    /// Opt-in desktop-notification flag (aider L107). When set, a best-effort OS
    /// notification fires on successful turn completion via `origin-notify`.
    /// Default `false` ⇒ no spawn ⇒ byte-identical.
    pub notify_desktop: bool,
    /// Opt-in interactive tool-permission prompting. When `true`, each
    /// `PromptRequest` carries `permission_ask`, so the daemon asks before
    /// running `RequiresPermission` tools. Default `false` ⇒ the daemon stays on
    /// auto-allow ⇒ byte-identical. Toggled by the `/permissions` command.
    pub permission_ask: bool,
    /// The pending permission ask, if the daemon is currently waiting on the
    /// user. `Some` ⇒ the next `y`/`n` (or `Esc`) answers it; rendered above the
    /// input card. `None` in the common case.
    pub pending_permission: Option<PendingPermission>,
    /// Scrollback row of the tool-activity line currently showing a `▸`
    /// "running" marker, so [`finish_tool_line`](Self::finish_tool_line) can
    /// flip it to `✔`/`✘` when the tool completes. `None` when no tool is in
    /// flight.
    running_tool_row: Option<usize>,
    /// Whether terminal mouse capture is on. Default `true` (wheel scrolls, but
    /// the terminal's native drag-select/copy is intercepted) — byte-identical
    /// with the historic behaviour. `/mouse off` releases capture so the user
    /// can select & copy; scrollback stays reachable via PageUp/Shift+arrows.
    pub mouse_capture: bool,
}

/// State backing the live cache-cold status-line nudge. All times are
/// wall-clock milliseconds (`SystemTime` since the Unix epoch); this lives in
/// the CLI, not a workflow, so real time is fine.
#[derive(Debug, Default)]
struct CacheColdState {
    /// Wall-clock ms at which the previous turn ended, or `None` before any turn.
    last_turn_end_ms: Option<u64>,
    /// Wall-clock ms at which the in-flight turn started. Used to measure the
    /// idle gap against `last_turn_end_ms`. `None` between turns.
    turn_start_ms: Option<u64>,
    /// Cumulative `cache_read` tokens observed at the moment the in-flight turn
    /// started, so the turn's own cache reads can be isolated as a delta.
    cache_read_at_start: u32,
    /// `true` once any turn has been served from a warm cache (`cache_read > 0`).
    /// Gates the "zero cache reads ⇒ cold" arm so a session's very first
    /// cache-write turn is not misreported as cold.
    had_prior_warm: bool,
    /// Whether the *current/most-recent* turn started against a cold cache. This
    /// is the bit the status line renders.
    cold: bool,
}

/// Current wall-clock time in milliseconds since the Unix epoch. Saturates to
/// `0` on the impossible pre-epoch case rather than panicking; this only feeds a
/// best-effort idle-gap heuristic, so a degraded clock at worst suppresses the
/// nudge.
fn now_wallclock_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
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
            stall: None,
            effort: None,
            output_style: None,
            steering: origin_steering::SteeringQueue::new(),
            plan_mode: false,
            pending_attachments: Vec::new(),
            workspace_roots: Vec::new(),
            cache_cold: CacheColdState::default(),
            vim_mode: VimMode::Insert,
            vim_active: false,
            theme: Theme::Default,
            notify_desktop: false,
            permission_ask: false,
            pending_permission: None,
            running_tool_row: None,
            mouse_capture: true,
        }
    }

    /// Apply a `/permissions [on|off]` toggle, returning the new state. No
    /// argument flips the current state; `on`/`off` set it explicitly.
    pub fn set_permission_ask(&mut self, arg: &str) -> bool {
        self.permission_ask = match arg.trim() {
            "on" => true,
            "off" => false,
            _ => !self.permission_ask,
        };
        self.permission_ask
    }

    /// Apply a `/mouse [on|off]` toggle, returning the new capture state. No
    /// argument flips; `on`/`off` set it explicitly. The caller is responsible
    /// for issuing the matching `EnableMouseCapture`/`DisableMouseCapture`.
    pub fn set_mouse_capture(&mut self, arg: &str) -> bool {
        self.mouse_capture = match arg.trim() {
            "on" => true,
            "off" => false,
            _ => !self.mouse_capture,
        };
        self.mouse_capture
    }

    /// Start the live turn timer. Called when a user submission begins.
    pub fn start_turn_timer(&mut self) {
        self.turn_started = Some(Instant::now());
        // Snapshot the wall-clock start and the cumulative cache-read counter so
        // `stop_turn_timer` can measure this turn's idle gap and isolate its own
        // cache reads for the cold-cache nudge.
        self.cache_cold.turn_start_ms = Some(now_wallclock_ms());
        self.cache_cold.cache_read_at_start = self.usage.cache_read_input_tokens;
    }

    /// Stop the live timer and fold the elapsed delta into `usage.elapsed`
    /// so the status line transitions seamlessly from "ticking" to the
    /// final accumulated total.
    pub fn stop_turn_timer(&mut self) {
        if let Some(start) = self.turn_started.take() {
            self.usage.elapsed += start.elapsed();
        }
        // No turn in flight => no stall possible; clear any lingering notice.
        self.stall = None;
        // A streaming tool (e.g. Bash) may not signal completion explicitly;
        // resolve its running marker to ✔ now that the turn has ended.
        self.finish_tool_line(true);
        self.evaluate_cache_cold();
    }

    /// Decide whether the just-finished turn started against a cold prompt cache
    /// and update the live nudge state, using the real wall clock for the turn
    /// end. Thin wrapper over [`Self::evaluate_cache_cold_at`] so the decision is
    /// deterministically testable.
    fn evaluate_cache_cold(&mut self) {
        self.evaluate_cache_cold_at(now_wallclock_ms());
    }

    /// Core of the cache-cold decision with an explicit `now_ms` for the turn
    /// end. Reuses `origin_cost::is_cache_cold` so the TUI surface and the cost
    /// meter share one decision. Purely additive: when warm (or no turn ran) the
    /// rendered status line is unchanged.
    fn evaluate_cache_cold_at(&mut self, now_ms: u64) {
        let Some(start_ms) = self.cache_cold.turn_start_ms.take() else {
            return;
        };
        let turn_cache_read = self
            .usage
            .cache_read_input_tokens
            .saturating_sub(self.cache_cold.cache_read_at_start);
        let cold = origin_cost::is_cache_cold(
            self.cache_cold.last_turn_end_ms,
            start_ms,
            u64::from(turn_cache_read),
            self.cache_cold.had_prior_warm,
        );
        if turn_cache_read > 0 {
            self.cache_cold.had_prior_warm = true;
        }
        self.cache_cold.cold = cold;
        self.cache_cold.last_turn_end_ms = Some(now_ms);
    }

    /// Whether the most-recent turn started against a cold prompt cache — the bit
    /// the status line renders as a brief nudge. Exposed for tests and the
    /// renderer.
    #[must_use]
    pub const fn cache_cold(&self) -> bool {
        self.cache_cold.cold
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
                .push(ScrollLine::styled(padded, self.palette().accent_dim, 0, false));
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
            .push(ScrollLine::styled(padded_tip, self.palette().muted, 0, false));
    }

    /// Wipe the in-session TUI view and restore the just-launched look, so
    /// `/clear` leaves the terminal as if origin had only just started.
    ///
    /// Drops all scrollback rows, any half-rendered assistant turn, the goal
    /// indicator, and resets the scroll position before re-painting the
    /// startup banner. Persistent/session config carried on `App` (effort,
    /// output style, theme, workspace roots, …) is deliberately left intact —
    /// `/clear` resets the *conversation view*, not the session's settings.
    pub fn reset_to_login(&mut self, cols: u16, rows: u16) {
        self.scrollback.clear();
        self.current_assistant = None;
        self.goal_status = None;
        self.stall = None;
        self.scroll_offset = 0;
        self.push_banner(cols, rows);
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
                    self.palette().user,
                    0,
                    true,
                ));
                self.scrollback
                    .push(ScrollLine::styled(String::new(), 0, 0, false));
            }
            "error> " => {
                self.scrollback.push(ScrollLine::styled(
                    format!("  \u{2718} {body}"),
                    self.palette().red,
                    0,
                    false,
                ));
            }
            "system> " => {
                self.scrollback
                    .push(ScrollLine::styled(format!("  {body}"), self.palette().muted, 0, false));
            }
            "ok> " => {
                self.scrollback.push(ScrollLine::styled(
                    format!("  \u{2714} {body}"),
                    self.palette().green,
                    0,
                    false,
                ));
            }
            "mem> " => {
                self.scrollback.push(ScrollLine::styled(
                    format!("  {body}"),
                    self.palette().accent_dim,
                    0,
                    false,
                ));
            }
            "tab> " => {
                self.scrollback
                    .push(ScrollLine::styled(format!("    {body}"), self.palette().muted, 0, false));
            }
            _ => {
                self.scrollback
                    .push(ScrollLine::styled(format!("  {body}"), self.palette().body, 0, false));
            }
        }
        self.scroll_offset = 0;
    }

    pub fn add_colored_line(&mut self, text: String, fg: u32, bg: u32) {
        // Pre-formatted (tool output, diff rows, streamed command lines): drawn
        // verbatim so embedded `**`/backticks aren't reinterpreted as markdown.
        self.scrollback.push(ScrollLine::verbatim(text, fg, bg));
    }

    /// Bug #4: update the one-line goal status indicator. `None` clears it
    /// (rendered as no goal row above the input card).
    pub fn set_goal_status_line(&mut self, status: Option<String>) {
        self.goal_status = status;
    }

    /// Handle a `/theme <name>` composer command (aider L107 theme preset).
    ///
    /// On a recognised name, switches the active [`Theme`] and returns `true`;
    /// an unknown name leaves the theme unchanged and returns `false` so the
    /// caller can surface a usage hint. The default theme is unchanged unless
    /// this is called, so the default render path stays byte-identical.
    pub fn set_theme_by_name(&mut self, name: &str) -> bool {
        Theme::parse(name).is_some_and(|t| {
            self.theme = t;
            true
        })
    }

    /// The palette for the active theme — the named colors the renderer reads
    /// when a non-default theme is in effect.
    #[must_use]
    pub const fn palette(&self) -> theme::Palette {
        theme::palette(self.theme)
    }

    /// Toggle the opt-in vim input layer (aider L107), returning the new active
    /// state. Enabling always starts in [`VimMode::Normal`] (vim convention);
    /// disabling resets to [`VimMode::Insert`] so the composer is immediately
    /// back to byte-identical direct insert.
    pub fn toggle_vim(&mut self) -> bool {
        self.vim_active = !self.vim_active;
        self.vim_mode = if self.vim_active {
            VimMode::Normal
        } else {
            VimMode::Insert
        };
        self.vim_active
    }

    /// Apply a [`crate::input::VimAction`] to the input buffer/cursor.
    ///
    /// This is the (impure) mutation the pure `vim_key` reducer feeds: mode
    /// switches update [`Self::vim_mode`]; motions move [`Self::cursor`] within
    /// the current buffer. Returns whether the event was consumed by the vim
    /// layer (`false` ⇒ [`crate::input::VimAction::Pass`], so the caller runs
    /// its normal handling). Cursor moves are clamped to the buffer; word
    /// motions step over runs of non-whitespace.
    pub fn apply_vim_action(&mut self, action: crate::input::VimAction) -> bool {
        use crate::input::VimAction as A;
        if action == A::Pass {
            return false;
        }
        let len = self.input.chars().count();
        // Resolve the action into an optional cursor target and an optional
        // mode switch, so each effect is expressed once and clippy sees no two
        // arms with identical bodies. `None` cursor ⇒ leave the cursor put.
        let (cursor, mode): (Option<usize>, Option<VimMode>) = match action {
            A::Pass => (None, None),
            A::SwitchMode(m) => (None, Some(m)),
            A::InsertHere | A::BeginCommand => (None, Some(VimMode::Insert)),
            A::AppendAfter => (Some((self.cursor + 1).min(len)), Some(VimMode::Insert)),
            A::InsertLineStart => (Some(0), Some(VimMode::Insert)),
            A::AppendLineEnd => (Some(len), Some(VimMode::Insert)),
            A::MoveLeft | A::MoveUp => (Some(self.cursor.saturating_sub(1)), None),
            A::MoveRight | A::MoveDown => (Some((self.cursor + 1).min(len)), None),
            A::LineStart | A::WordBack => (Some(0), None),
            A::LineEnd | A::WordForward => (Some(len), None),
        };
        if let Some(c) = cursor {
            self.cursor = c;
        }
        if let Some(m) = mode {
            self.vim_mode = m;
        }
        true
    }

    pub fn add_tool_line(&mut self, text: String) {
        self.scrollback.push(ScrollLine::verbatim(text, self.palette().tool, 0));
    }

    /// Push a tool-activity line with a leading `▸` "running" marker, recording
    /// its row so [`finish_tool_line`](Self::finish_tool_line) can flip it to
    /// `✔`/`✘`. If a previous tool is still marked running (e.g. a streaming
    /// Bash whose completion isn't signalled explicitly), assume it finished OK
    /// before starting the new one — so at most one `▸` is ever visible.
    pub fn start_tool_line(&mut self, text: &str) {
        self.finish_tool_line(true);
        self.running_tool_row = Some(self.scrollback.len());
        self.scrollback
            .push(ScrollLine::verbatim(format!("  \u{25B8} {text}"), self.palette().tool, 0));
        self.scroll_offset = 0;
    }

    /// Flip the tracked running tool line's `▸` to `✔` (ok) or `✘` (failure, in
    /// red) and stop tracking it. No-op when no tool line is being tracked, so
    /// it is safe to call on every result and at turn end.
    pub fn finish_tool_line(&mut self, ok: bool) {
        let Some(row) = self.running_tool_row.take() else {
            return;
        };
        // Resolve the color before the mutable scrollback borrow below.
        let red = self.palette().red;
        if let Some(line) = self.scrollback.get_mut(row) {
            let glyph = if ok { '\u{2714}' } else { '\u{2718}' };
            line.text = line.text.replacen('\u{25B8}', &glyph.to_string(), 1);
            if !ok {
                line.fg = red;
            }
        }
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

    /// The text buffered for the in-flight assistant turn, if any.
    ///
    /// Lets the caller fire the (async) `MessageDisplay` shell hook on the
    /// rendered text *before* it acquires the `App` lock and calls
    /// [`finalize_assistant_turn`](Self::finalize_assistant_turn), passing the
    /// resulting action in. `None` ⇒ no turn buffered ⇒ nothing to render.
    #[must_use]
    pub fn current_assistant_text(&self) -> Option<&str> {
        self.current_assistant.as_deref()
    }

    /// Flush the buffered assistant turn into scrollback.
    ///
    /// No `MessageDisplay` hook action: equivalent to
    /// [`finalize_assistant_turn_with_action`](Self::finalize_assistant_turn_with_action)
    /// with `None`, so the active output style alone decides the render.
    pub fn finalize_assistant_turn(&mut self, turns: u32) {
        self.finalize_assistant_turn_with_action(turns, None);
    }

    pub fn finalize_assistant_turn_with_action(
        &mut self,
        _turns: u32,
        hook_action: Option<&origin_outputstyle::DisplayAction>,
    ) {
        if let Some(raw) = self.current_assistant.take() {
            // claude-code MessageDisplay: a `MessageDisplay` shell hook (when one
            // fired and returned an action) decides the rendered text outright;
            // otherwise the active output style may rewrite or hide it. No hook
            // *and* no style (or the default) ⇒ identity ⇒ `Some(raw)` unchanged,
            // so rendering is byte-identical. `None` suppresses the message.
            let Some(text) =
                origin_outputstyle::resolve_display(&raw, self.output_style, hook_action)
            else {
                return;
            };
            if !text.is_empty() {
                let mut in_code_block = false;
                for line in text.split('\n') {
                    let trimmed = line.trim_start();
                    if trimmed.starts_with("```") {
                        in_code_block = !in_code_block;
                        self.scrollback.push(ScrollLine::styled(
                            format!("  {line}"),
                            self.palette().muted,
                            if in_code_block { self.palette().code_bg } else { 0 },
                            false,
                        ));
                        continue;
                    }
                    if in_code_block {
                        self.scrollback.push(ScrollLine::styled(
                            format!("  {line}"),
                            self.palette().code_fg,
                            self.palette().code_bg,
                            false,
                        ));
                    } else if let Some(task) = crate::markdown_tasks::render_gfm_task_line(line) {
                        // claude-code L147 (GFM task-list rendering): `- [ ]` /
                        // `- [x]` lines render with a checkbox glyph. Pure
                        // fall-through: non-task lines yield `None` and keep the
                        // byte-identical default styling below.
                        self.scrollback
                            .push(ScrollLine::styled(format!("  {task}"), self.palette().body, 0, false));
                    } else {
                        let (fg, bold) = md_line_style(line, self.palette());
                        // Strip ATX heading markers (`## `) — the color/weight
                        // from `md_line_style` already conveys the hierarchy, so
                        // the literal hashes are clutter. Non-headings unchanged.
                        let rendered =
                            strip_heading_markers(line).unwrap_or_else(|| line.to_string());
                        self.scrollback
                            .push(ScrollLine::styled(format!("  {rendered}"), fg, 0, bold));
                    }
                }
                // Trailing blank line so the next user turn (or the input
                // card) has visible separation from this response.
                self.scrollback
                    .push(ScrollLine::styled(String::new(), 0, 0, false));
            }
            // aider L107 OS-notification: best-effort desktop chime on turn
            // completion. Gated by the opt-in flag (default-off ⇒ no spawn ⇒
            // byte-identical) and best-effort — a missing notifier never
            // disturbs the session. `succeeded == true`: reaching this arm means
            // the turn produced a (possibly empty-rendered) assistant reply.
            let _ = notify_turn_complete(self.notify_desktop, true);
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

            // Snapshot the active palette once per frame; every chrome helper
            // reads it (via CardLayout or a `pal` arg), so a `/theme` switch
            // re-themes the chrome immediately. Default ⇒ the legacy constants.
            let pal = self.palette();

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
                    entry.literal,
                    cols_usize,
                    &mut visual_lines,
                );
            }
            let live_buf;
            if let Some(buf) = self.current_assistant.as_ref() {
                live_buf = format!("  {buf}");
                // Live assistant text is prose → markdown-parsed (literal=false).
                wrap_into(&live_buf, pal.body, 0, false, false, cols_usize, &mut visual_lines);
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
                render_scroll_line(main, row, vl, cols, pal);
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
                palette: pal,
            };

            draw_scroll_indicator(main, &layout, offset);
            self.draw_notices(main, &layout);
            draw_input_card_bg(main, &layout);
            self.draw_input_text(main, &layout, &wrapped);
            self.draw_status_line(main, &layout);
            draw_keybind_hint(main, &layout, self.spinner.active || self.goal_status.is_some());
            self.draw_suggestions_popup(main, &layout);
        }
        clear_prompt_grid(composer.prompt_grid(), self.palette());
    }

    /// Render the goal-status indicator and stall-watchdog notice on the
    /// breathing-room row just above the input card. The stall notice (when
    /// active) overpaints the goal status — a stall is the more urgent signal.
    fn draw_notices(&self, main: &mut Grid, layout: &CardLayout) {
        // Highest-priority notice: a pending permission ask blocks the turn, so
        // it overpaints the goal/stall row. The user answers with y / n.
        if let Some(ref ask) = self.pending_permission {
            let status_row = layout.at_bottom.saturating_sub(1);
            if status_row < layout.rows {
                let msg = format!(
                    "\u{26A0} Allow {} {}?  y = allow \u{00B7} n = deny",
                    ask.tool, ask.args
                );
                write_str_styled(
                    main,
                    status_row,
                    layout.cl.saturating_add(2),
                    &msg,
                    layout.cr,
                    Style {
                        fg: layout.palette.yellow,
                        bg: 0,
                        bold: true,
                    },
                );
            }
            return;
        }
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
                        fg: layout.palette.accent,
                        bg: 0,
                        bold: false,
                    },
                );
            }
        }

        // Stall watchdog notice. A soft tier reassures ("still working…") after
        // a short quiet; the hard tier alarms (red + interrupt hint) on
        // sustained silence so a wedged daemon reads as "possibly stuck" instead
        // of an indefinitely-spinning spinner. Takes the row the goal status
        // would use — a stall is the more urgent signal.
        if let Some(tier) = self.stall {
            let status_row = layout.at_bottom.saturating_sub(1);
            if status_row < layout.rows {
                let (msg, fg) = match tier {
                    StallTier::Soft(secs) => {
                        (format!("\u{2026} still working\u{2026} {secs}s"), layout.palette.muted)
                    }
                    StallTier::Hard(secs) => (
                        format!("\u{26A0} no daemon activity for {secs}s \u{2014} Ctrl+C to interrupt"),
                        layout.palette.red,
                    ),
                };
                write_str_styled(
                    main,
                    status_row,
                    layout.cl.saturating_add(2),
                    &msg,
                    layout.cr,
                    Style { fg, bg: 0, bold: false },
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
                        fg: layout.palette.muted,
                        bg: layout.palette.surface_raised,
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
                        fg: layout.palette.bright,
                        bg: layout.palette.surface_raised,
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
                            fg: layout.palette.dim,
                            bg: layout.palette.surface_raised,
                            bold: false,
                        },
                    );
                }
            }
        }
    }

    /// Render the status line as ordered styled spans: the spinner glyph and
    /// workflow lead in accent, a Thinking/Responding phase follows while a turn
    /// is in flight, the live cost and elapsed are body-bright (the numbers a
    /// user watches), token counts are muted, and the static model name is dim —
    /// so the line has a focal point instead of a flat, near-invisible DIM wall.
    fn draw_status_line(&self, main: &mut Grid, layout: &CardLayout) {
        if layout.r_status >= layout.rows {
            return;
        }
        let cost = crate::status::cost_usd(&self.usage);
        let live_elapsed = self
            .turn_started
            .map_or(self.usage.elapsed, |t| self.usage.elapsed + t.elapsed());
        let secs = live_elapsed.as_secs_f64();
        let tok_in = format_tokens(self.usage.input_tokens);
        let tok_out = format_tokens(self.usage.output_tokens);
        let phase = turn_phase(self.spinner.active, self.current_assistant.as_deref());
        let tokens = format!("{tok_in}\u{2191} {tok_out}\u{2193}");
        let pal = layout.palette;
        let spans = status_spans(
            pal,
            &self.workflow,
            phase,
            &self.usage.model,
            &tokens,
            cost,
            secs,
            self.cache_cold.cold,
        );

        let mut c = layout.cs;
        // The animated spinner glyph leads in accent while a turn is in flight.
        if self.spinner.active {
            if c < layout.cr {
                main.put(
                    layout.r_status,
                    c,
                    Cell::new(self.spinner.frame_char(), pal.accent, pal.surface_raised, Attr::PLAIN),
                );
            }
            c = c.saturating_add(2);
        }
        for (i, span) in spans.iter().enumerate() {
            if i > 0 {
                let sep = Style {
                    fg: pal.dim,
                    bg: pal.surface_raised,
                    bold: false,
                };
                c = write_span(main, layout.r_status, c, " \u{00B7} ", layout.cr, sep);
            }
            let span_style = Style {
                fg: span.fg,
                bg: pal.surface_raised,
                bold: span.bold,
            };
            c = write_span(main, layout.r_status, c, &span.text, layout.cr, span_style);
            if c >= layout.cr {
                break;
            }
        }
        // Fill the remainder with the raised surface so the status line spans the
        // full card width like the input rows above it.
        while c < layout.cr {
            main.put(layout.r_status, c, Cell::new(' ', 0, pal.surface_raised, Attr::PLAIN));
            c = c.saturating_add(1);
        }
    }

    /// Render the autocomplete suggestions popup directly above the input card.
    /// Shows a scrolling window of up to [`suggestions::MAX_VISIBLE`] candidates
    /// over the full match list, with the selected row highlighted, so every
    /// skill is reachable by arrowing through the list.
    fn draw_suggestions_popup(&self, main: &mut Grid, layout: &CardLayout) {
        let total = self.suggestions.candidates.len();
        if total == 0 {
            return;
        }
        let win = crate::suggestions::MAX_VISIBLE;
        let offset = crate::suggestions::scroll_offset(total, self.suggestions.selected);
        let visible = total.saturating_sub(offset).min(win);
        let count = clamp_u16(visible);
        let popup_bottom = layout.r_top.saturating_sub(1);
        let popup_top = popup_bottom.saturating_sub(count);
        let selected = self.suggestions.selected;
        let more_above = offset > 0;
        let more_below = offset + visible < total;
        for (row, candidate) in self
            .suggestions
            .candidates
            .iter()
            .enumerate()
            .skip(offset)
            .take(visible)
        {
            let i = row - offset;
            let r = popup_top + clamp_u16(i);
            if r >= popup_bottom || r >= layout.rows {
                break;
            }
            for c in layout.cl..layout.cr.min(layout.cols) {
                main.put(r, c, Cell::new(' ', 0, layout.palette.surface_raised, Attr::PLAIN));
            }
            let (ind_fg, txt_fg) = if row == selected {
                (layout.palette.accent, layout.palette.body)
            } else {
                (layout.palette.muted, layout.palette.muted)
            };
            // Indicator column: selection arrow takes priority; otherwise
            // show scroll hints on the top/bottom rows when the list overflows.
            let ind = if row == selected {
                " \u{25B8} "
            } else if i == 0 && more_above {
                " \u{2191} "
            } else if i + 1 == visible && more_below {
                " \u{2193} "
            } else {
                "   "
            };
            let ps = layout.cl + 1;
            write_str_styled(
                main,
                r,
                ps,
                ind,
                layout.cr,
                Style {
                    fg: ind_fg,
                    bg: layout.palette.surface_raised,
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
                    bg: layout.palette.surface_raised,
                    bold: false,
                },
            );
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
    /// Active theme palette, snapshotted once per frame so every chrome helper
    /// taking `&CardLayout` re-themes without a separate parameter.
    palette: theme::Palette,
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
                fg: layout.palette.accent,
                bg: layout.palette.surface_raised,
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
            main.put(r, c, Cell::new(' ', 0, layout.palette.surface_raised, Attr::PLAIN));
        }
        if layout.cl < layout.cols {
            main.put(
                r,
                layout.cl,
                Cell::new('\u{2503}', layout.palette.accent, layout.palette.surface_raised, Attr::PLAIN),
            );
        }
    }
}

/// The right-aligned keybind hint segments. While a turn is in flight the
/// trailing `ctrl+c quit` becomes `ctrl+c interrupt`, matching the reducer
/// (which remaps Ctrl+C to Interrupt during a turn) — so the hint never claims
/// it will quit at the exact moment it will actually interrupt.
const fn keybind_hint_parts(in_flight: bool, pal: theme::Palette) -> [(&'static str, u32); 6] {
    let last_label = if in_flight { " interrupt" } else { " quit" };
    [
        ("shift+enter", pal.body),
        (" newline  ", pal.muted),
        ("tab", pal.body),
        (" skills  ", pal.muted),
        ("ctrl+c", pal.body),
        (last_label, pal.muted),
    ]
}

/// Render the keybind hint line beneath the input card (and the card's bottom
/// accent corner), right-aligned within the card width.
fn draw_keybind_hint(main: &mut Grid, layout: &CardLayout, in_flight: bool) {
    if layout.r_cap < layout.rows {
        if layout.cl < layout.cols {
            main.put(
                layout.r_cap,
                layout.cl,
                Cell::new('\u{2579}', layout.palette.accent, 0, Attr::PLAIN),
            );
        }
        let hint_parts = keybind_hint_parts(in_flight, layout.palette);
        let total_hw: u16 = hint_parts.iter().map(|(s, _)| char_display_width(s)).sum();
        let mut hc = layout.cr.saturating_sub(total_hw);
        for (text, fg) in &hint_parts {
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
fn clear_prompt_grid(prompt: &mut Grid, pal: theme::Palette) {
    for r in 0..prompt.rows() {
        for c in 0..prompt.cols() {
            prompt.put(r, c, Cell::new(' ', 0, pal.surface, Attr::PLAIN));
        }
    }
}

// Bug #4: implement the `goal_render::GoalRender` sink directly on `App`
// so `main.rs::call_daemon`'s event arm becomes a one-liner pass-through
// instead of a duplicated match on every Goal* variant.
impl crate::goal_render::GoalRender for App {
    fn push_colored(&mut self, text: String, fg: u32, _bg: u32) {
        // Goal/agent progress lines are pre-formatted status text → verbatim.
        self.scrollback.push(ScrollLine::verbatim(text, fg, 0));
        self.scroll_offset = 0;
    }
    fn set_goal_status(&mut self, status: Option<String>) {
        self.goal_status = status;
    }
}

/// Render plan steps and a vertical divider into the side panel grid.
pub fn draw_side(side: &mut Grid, plan_lines: &[PlanLine], pal: theme::Palette) {
    let cols = side.cols();
    let rows = side.rows();

    for r in 0..rows {
        for c in 0..cols {
            side.put(r, c, Cell::new(' ', 0, pal.panel_bg, Attr::PLAIN));
        }
    }

    for r in 0..rows {
        side.put(
            r,
            0,
            Cell::new('\u{2502}', pal.border, pal.panel_bg, Attr::PLAIN),
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
                fg: pal.muted,
                bg: pal.panel_bg,
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
            fg: pal.panel_header,
            bg: pal.panel_bg,
            bold: true,
        },
    );

    for c in 1..cols {
        side.put(
            1,
            c,
            Cell::new('\u{2500}', pal.border, pal.panel_bg, Attr::PLAIN),
        );
    }
    side.put(
        1,
        0,
        Cell::new('\u{251C}', pal.border, pal.panel_bg, Attr::PLAIN),
    );

    // cline L171: render the plan as a live focus-chain checkbox todo list.
    // The checkbox marker (`[ ]`/`[~]`/`[x]`) carries the GFM-style state; the
    // existing status glyph stays as a colored leading dot so progress reads at
    // a glance.
    let checklist = render_focus_chain(plan_lines);
    let mut row: u16 = 2;
    for (pl, checkbox) in plan_lines.iter().zip(&checklist) {
        if row >= rows {
            break;
        }
        let glyph_fg = match pl.status_glyph {
            '\u{25CB}' => pal.muted,
            '\u{25D0}' => pal.accent,
            '\u{25CF}' => pal.green,
            '\u{2715}' => pal.red,
            _ => pal.body,
        };
        side.put(
            row,
            2,
            Cell::new(pl.status_glyph, glyph_fg, pal.panel_bg, Attr::PLAIN),
        );
        write_str_styled(
            side,
            row,
            4,
            checkbox,
            cols.saturating_sub(4),
            Style {
                fg: pal.body,
                bg: pal.panel_bg,
                bold: false,
            },
        );
        row += 1;
    }
}

/// Checkbox state for one focus-chain row, derived from a plan step's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskState {
    /// Not started.
    Pending,
    /// Currently being worked.
    InProgress,
    /// Complete (or cancelled — both render as a filled box).
    Done,
}

impl TaskState {
    /// GFM-style three-state checkbox marker for this row.
    const fn marker(self) -> &'static str {
        match self {
            Self::Pending => "[ ]",
            Self::InProgress => "[~]",
            Self::Done => "[x]",
        }
    }
}

/// Map a plan-panel status glyph (`○`/`◐`/`●`/`✕`) to a checkbox state.
///
/// Mirrors `origin_tui::widgets::plan_panel::status_glyph`: pending is open,
/// in-progress is half, done and cancelled are filled. Unknown glyphs are
/// treated as pending so a future status never panics the renderer.
const fn task_state_for_glyph(glyph: char) -> TaskState {
    match glyph {
        '\u{25D0}' => TaskState::InProgress,
        '\u{25CF}' | '\u{2715}' => TaskState::Done,
        _ => TaskState::Pending,
    }
}

/// Render the active plan as a live focus-chain checklist (cline L171).
///
/// Each plan step becomes a GFM-style checkbox line — `[ ]` pending, `[~]`
/// in-progress, `[x]` done — in plan (Logoot) order, with the step body
/// appended. When no step carries an explicit non-pending status (the plan
/// hasn't reported progress yet), a reasonable focus is derived: the first
/// step is treated as in-progress and the rest stay pending, so the panel
/// always highlights one active item. Returns an empty `Vec` for an empty
/// plan, so the caller renders nothing when there is no plan.
#[must_use]
pub fn render_focus_chain(plan_lines: &[PlanLine]) -> Vec<String> {
    if plan_lines.is_empty() {
        return Vec::new();
    }
    let mut states: Vec<TaskState> = plan_lines
        .iter()
        .map(|pl| task_state_for_glyph(pl.status_glyph))
        .collect();

    // Derive a focus when the plan reports no explicit progress: with every
    // step pending, promote the first to in-progress so one item reads as
    // active. A plan that already marks any step keeps its reported states.
    if states.iter().all(|s| *s == TaskState::Pending) {
        if let Some(first) = states.first_mut() {
            *first = TaskState::InProgress;
        }
    }

    states
        .iter()
        .zip(plan_lines)
        .map(|(state, pl)| format!("{} {}", state.marker(), pl.content))
        .collect()
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

/// If `line` is an ATX markdown heading (`# `/`## `/`### ` after optional
/// leading whitespace), return the text with the `#` markers and the one
/// following space removed. Hierarchy is conveyed by [`md_line_style`]'s color
/// and weight, so the literal hashes are visual clutter. Non-headings (and
/// `#hashtag` with no space, or 4+ hashes) return `None` and render verbatim.
fn strip_heading_markers(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    // `#` is ASCII so byte-indexing at `hashes` is a valid char boundary.
    if (1..=3).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
        Some(trimmed[hashes + 1..].to_string())
    } else {
        None
    }
}

fn md_line_style(line: &str, pal: theme::Palette) -> (u32, bool) {
    let trimmed = line.trim_start();
    if trimmed.starts_with("### ") {
        (pal.h3, true)
    } else if trimmed.starts_with("## ") {
        (pal.h2, true)
    } else if trimmed.starts_with("# ") {
        (pal.h1, true)
    } else if trimmed.starts_with("---") && trimmed.chars().all(|c| c == '-' || c == ' ') {
        (pal.rule, false)
    } else if trimmed.starts_with("```") {
        (pal.muted, false)
    } else if trimmed.starts_with("> ") {
        (pal.accent_dim, false)
    } else {
        (pal.body, false)
    }
}

/// One styled segment of the status line.
struct StatusSpan {
    text: String,
    fg: u32,
    bold: bool,
}

/// The status phase label while a turn is active: `"Thinking"` before any
/// assistant text has streamed, `"Responding"` once it has. `None` when idle —
/// so a long pre-token think no longer looks identical to streaming or a stall.
fn turn_phase(spinner_active: bool, assistant: Option<&str>) -> Option<&'static str> {
    if !spinner_active {
        return None;
    }
    if assistant.is_none_or(str::is_empty) {
        Some("Thinking")
    } else {
        Some("Responding")
    }
}

/// Build the ordered status-line segments (excluding the animated spinner glyph,
/// which the caller prepends). Pure, for testability. Hierarchy: workflow leads
/// in accent+bold, an optional phase follows in dimmed accent, the model name is
/// dim, token counts muted, and the live cost/elapsed are body-bright. A cold
/// nudge, when present, trails in yellow.
#[allow(clippy::too_many_arguments)] // each is a distinct status segment; bundling would obscure
fn status_spans(
    pal: theme::Palette,
    workflow: &str,
    phase: Option<&str>,
    model: &str,
    tokens: &str,
    cost: f64,
    secs: f64,
    cache_cold: bool,
) -> Vec<StatusSpan> {
    let mut spans = vec![StatusSpan {
        text: workflow.to_string(),
        fg: pal.accent,
        bold: true,
    }];
    if let Some(p) = phase {
        spans.push(StatusSpan {
            text: p.to_string(),
            fg: pal.accent_dim,
            bold: false,
        });
    }
    spans.push(StatusSpan {
        text: model.to_string(),
        fg: pal.dim,
        bold: false,
    });
    spans.push(StatusSpan {
        text: tokens.to_string(),
        fg: pal.muted,
        bold: false,
    });
    spans.push(StatusSpan {
        text: format!("${cost:.3}"),
        fg: pal.body,
        bold: false,
    });
    spans.push(StatusSpan {
        text: format!("{secs:.1}s"),
        fg: pal.body,
        bold: false,
    });
    if cache_cold {
        spans.push(StatusSpan {
            text: "\u{29D7} cache cold".to_string(),
            fg: pal.yellow,
            bold: false,
        });
    }
    spans
}

/// Write `s` at (`row`, `col`) on the raised-surface background and return the
/// next free column. Unlike [`write_str_styled`] it does not bg-fill to the row
/// end, so spans can be chained left-to-right.
fn write_span(grid: &mut Grid, row: u16, col: u16, s: &str, max_cols: u16, style: Style) -> u16 {
    let attr = if style.bold { Attr::BOLD } else { Attr::PLAIN };
    let mut c = col;
    for ch in s.chars() {
        let w = char_cell_width(ch);
        if w == 0 {
            continue;
        }
        if c + w > max_cols {
            break;
        }
        grid.put(row, c, Cell::new(ch, style.fg, style.bg, attr));
        c += w;
    }
    c
}

/// Render one wrapped visual line into the grid.
///
/// Literal lines (pre-formatted tool/diff/command output) are written verbatim
/// so `**`/backticks survive; prose lines go through the inline-markdown
/// renderer so `**bold**` and `` `code` `` style correctly.
fn render_scroll_line(grid: &mut Grid, row: u16, vl: &VisualLine<'_>, cols: u16, pal: theme::Palette) {
    let style = Style {
        fg: vl.fg,
        bg: vl.bg,
        bold: vl.bold,
    };
    if vl.literal {
        write_str_styled(grid, row, 0, vl.text, cols, style);
    } else {
        render_md_line(grid, row, vl.text, cols, style, pal);
    }
}

fn render_md_line(grid: &mut Grid, row: u16, text: &str, max_cols: u16, style: Style, pal: theme::Palette) {
    let base_fg = style.fg;
    let bg = style.bg;
    let attr_plain = if style.bold { Attr::BOLD } else { Attr::PLAIN };
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
                    grid.put(row, col, Cell::new(chars[i], pal.bright, bg, Attr::BOLD));
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
                        Cell::new(chars[i], pal.code_fg, pal.code_bg, Attr::PLAIN),
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

/// Summary line for a diff truncated to `shown` of `total` rows, or `None` when
/// nothing was elided (`total <= shown`).
///
/// Lets the tool view render the first `shown` changed rows then one muted line
/// instead of dumping a 2000-line `Write` and burying the conversation. The
/// 2-space indent nests it under the tool header.
#[must_use]
pub fn diff_elision_summary(total: usize, shown: usize) -> Option<String> {
    if total <= shown {
        return None;
    }
    let hidden = total - shown;
    Some(format!("  \u{2026} +{hidden} more diff lines ({total} total)"))
}

struct VisualLine<'a> {
    text: &'a str,
    fg: u32,
    bg: u32,
    bold: bool,
    /// Carries [`ScrollLine::literal`] through wrapping so the draw loop knows
    /// whether to markdown-parse this row or write it verbatim.
    literal: bool,
}

fn wrap_into<'a>(
    text: &'a str,
    fg: u32,
    bg: u32,
    bold: bool,
    literal: bool,
    cols: usize,
    out: &mut Vec<VisualLine<'a>>,
) {
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
                literal,
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
                    literal,
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
                literal,
            });
        }
    }
}

fn write_str_styled(grid: &mut Grid, row: u16, col: u16, s: &str, max_cols: u16, style: Style) {
    let attr = if style.bold { Attr::BOLD } else { Attr::PLAIN };
    let mut c = col;
    for ch in s.chars() {
        let w = char_cell_width(ch);
        // Zero-width combining marks (e.g. a base char + U+0301) get no cell of
        // their own — emitting one would overwrite the base glyph or drift the
        // rest of the row. Skip them so the base stays intact.
        if w == 0 {
            continue;
        }
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
        wrap_into("ab\u{276F}cd", 0, 0, false, false, 4, &mut lines);
        assert_eq!(lines.len(), 2, "wide char should cause wrap at col 4");
    }

    #[test]
    fn tool_and_diff_lines_are_literal() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.add_colored_line("**not bold**".to_string(), theme::BODY, 0);
        assert!(
            matches!(app.scrollback.last(), Some(l) if l.literal),
            "tool/diff/command output must be drawn verbatim"
        );
        app.add_tool_line("[Bash] echo **x**".to_string());
        assert!(matches!(app.scrollback.last(), Some(l) if l.literal));
    }

    #[test]
    fn tool_line_marks_running_then_done() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.start_tool_line("[Write] src/x.rs");
        assert!(
            app.scrollback.last().is_some_and(|l| l.text.contains('\u{25B8}')),
            "running line shows the ▸ marker"
        );
        app.finish_tool_line(true);
        assert!(
            app.scrollback
                .last()
                .is_some_and(|l| l.text.contains('\u{2714}') && !l.text.contains('\u{25B8}')),
            "completed-ok shows ✔ and no ▸"
        );
    }

    #[test]
    fn tool_line_failure_is_cross_and_red() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.start_tool_line("[Bash] false");
        app.finish_tool_line(false);
        assert!(
            app.scrollback.last().is_some_and(|l| l.text.contains('\u{2718}') && l.fg == theme::RED),
            "failure shows ✘ in red"
        );
    }

    #[test]
    fn starting_next_tool_resolves_previous_running_marker() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.start_tool_line("[Bash] first");
        app.start_tool_line("[Bash] second");
        let ticks = app.scrollback.iter().filter(|l| l.text.contains('\u{2714}')).count();
        let arrows = app.scrollback.iter().filter(|l| l.text.contains('\u{25B8}')).count();
        assert_eq!(ticks, 1, "previous tool resolved to ✔");
        assert_eq!(arrows, 1, "only the current tool still shows ▸");
    }

    #[test]
    fn diff_elision_summary_only_when_truncated() {
        assert_eq!(diff_elision_summary(10, 40), None, "small diff: no summary");
        assert_eq!(diff_elision_summary(40, 40), None, "exactly at cap: no summary");
        let s = diff_elision_summary(2000, 40).expect("large diff summarized");
        assert!(s.contains("1960"), "hidden count: {s}");
        assert!(s.contains("2000 total"), "total count: {s}");
    }

    #[test]
    fn finish_tool_line_without_running_is_noop() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.finish_tool_line(true);
        assert!(app.scrollback.is_empty(), "no tool line, nothing happens");
    }

    #[test]
    fn assistant_prose_lines_are_not_literal() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.start_assistant_turn();
        app.append_to_current_assistant("**bold** prose");
        app.finalize_assistant_turn(0);
        assert!(
            app.scrollback
                .iter()
                .any(|l| !l.literal && l.text.contains("bold")),
            "assistant prose must stay markdown-parsed (literal=false)"
        );
    }

    #[test]
    fn verbatim_line_keeps_markdown_glyphs_but_prose_parses_them() {
        let mut g = Grid::new(12, 1);
        let lit = VisualLine {
            text: "**x**",
            fg: theme::BODY,
            bg: 0,
            bold: false,
            literal: true,
        };
        render_scroll_line(&mut g, 0, &lit, 12, theme::Palette::default());
        assert_eq!(g.get(0, 0).glyph, u32::from('*'), "literal line keeps leading *");

        let mut g2 = Grid::new(12, 1);
        let prose = VisualLine {
            text: "**x**",
            fg: theme::BODY,
            bg: 0,
            bold: false,
            literal: false,
        };
        render_scroll_line(&mut g2, 0, &prose, 12, theme::Palette::default());
        assert_eq!(
            g2.get(0, 0).glyph,
            u32::from('x'),
            "prose line parses **bold**, dropping the markers"
        );
    }

    #[test]
    fn strip_heading_markers_strips_leading_hashes() {
        assert_eq!(strip_heading_markers("# Title").as_deref(), Some("Title"));
        assert_eq!(strip_heading_markers("## Section").as_deref(), Some("Section"));
        assert_eq!(strip_heading_markers("### Sub").as_deref(), Some("Sub"));
        assert_eq!(strip_heading_markers("plain text"), None);
        assert_eq!(strip_heading_markers("#hashtag"), None);
        assert_eq!(strip_heading_markers("#### four"), None);
    }

    #[test]
    fn finalize_strips_heading_hashes_from_scrollback() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.start_assistant_turn();
        app.append_to_current_assistant("## Heading\nbody");
        app.finalize_assistant_turn(0);
        assert!(
            app.scrollback.iter().any(|l| l.text == "  Heading"),
            "heading hashes stripped, got {:?}",
            app.scrollback.iter().map(|l| l.text.clone()).collect::<Vec<_>>()
        );
        assert!(
            !app.scrollback.iter().any(|l| l.text.contains("##")),
            "no literal ## remains"
        );
    }

    #[test]
    fn turn_phase_distinguishes_thinking_and_responding() {
        assert_eq!(turn_phase(false, None), None);
        assert_eq!(turn_phase(false, Some("hi")), None);
        assert_eq!(turn_phase(true, None), Some("Thinking"));
        assert_eq!(turn_phase(true, Some("")), Some("Thinking"));
        assert_eq!(turn_phase(true, Some("partial")), Some("Responding"));
    }

    #[test]
    fn status_spans_have_legibility_hierarchy() {
        let spans = status_spans(theme::Palette::default(), "Code", Some("Thinking"), "claude", "1.2k\u{2191} 300\u{2193}", 0.84, 3.4, false);
        assert_eq!(spans[0].text, "Code");
        assert_eq!(spans[0].fg, theme::ACCENT);
        assert!(spans[0].bold, "workflow leads bold");
        assert!(spans.iter().any(|s| s.text == "Thinking" && s.fg == theme::ACCENT_DIM));
        assert!(spans.iter().any(|s| s.text == "claude" && s.fg == theme::DIM), "model is dim");
        assert!(spans.iter().any(|s| s.text == "$0.840" && s.fg == theme::BODY), "cost is body-bright");
        assert!(spans.iter().any(|s| s.text == "3.4s" && s.fg == theme::BODY), "elapsed is body-bright");
        assert!(spans.iter().any(|s| s.text.contains('\u{2191}') && s.fg == theme::MUTED), "tokens muted");
        assert!(!spans.iter().any(|s| s.text.contains("cache cold")));
    }

    #[test]
    fn status_spans_append_cold_nudge_in_yellow() {
        let spans = status_spans(theme::Palette::default(), "Code", None, "m", "0\u{2191} 0\u{2193}", 0.0, 0.0, true);
        assert!(
            spans.last().is_some_and(|s| s.text.contains("cache cold") && s.fg == theme::YELLOW),
            "cold nudge trails in yellow"
        );
    }

    #[test]
    fn keybind_hint_shows_interrupt_while_in_flight() {
        let idle = keybind_hint_parts(false, theme::Palette::default());
        assert!(idle.iter().any(|(t, _)| *t == " quit"));
        assert!(!idle.iter().any(|(t, _)| t.contains("interrupt")));
        let busy = keybind_hint_parts(true, theme::Palette::default());
        assert!(busy.iter().any(|(t, _)| *t == " interrupt"));
        assert!(!busy.iter().any(|(t, _)| *t == " quit"));
    }

    #[test]
    fn zero_width_combining_mark_keeps_base_glyph() {
        // "e" + U+0301 (combining acute): the mark must not overwrite the base
        // 'e' nor shift the following 'x'.
        let mut g = Grid::new(8, 1);
        write_str_styled(
            &mut g,
            0,
            0,
            "e\u{0301}x",
            8,
            Style {
                fg: theme::BODY,
                bg: 0,
                bold: false,
            },
        );
        assert_eq!(g.get(0, 0).glyph, u32::from('e'), "base glyph preserved");
        assert_eq!(g.get(0, 1).glyph, u32::from('x'), "next char not drifted");
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
    fn stall_tier_classifies_quiet_into_soft_and_hard() {
        let soft = Duration::from_secs(11);
        let hard = Duration::from_secs(28);
        assert_eq!(stall_tier(Duration::from_secs(5), soft, hard), None, "below soft: nothing");
        assert_eq!(
            stall_tier(Duration::from_secs(11), soft, hard),
            Some(StallTier::Soft(11)),
            "at soft threshold"
        );
        assert_eq!(
            stall_tier(Duration::from_secs(27), soft, hard),
            Some(StallTier::Soft(27)),
            "between thresholds stays soft"
        );
        assert_eq!(
            stall_tier(Duration::from_secs(28), soft, hard),
            Some(StallTier::Hard(28)),
            "at hard threshold escalates"
        );
        assert_eq!(stall_tier(Duration::from_secs(90), soft, hard), Some(StallTier::Hard(90)));
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
        app.stall = Some(StallTier::Hard(90));
        app.stop_turn_timer();
        assert_eq!(app.stall, None, "ending a turn must clear the stall notice");
    }

    #[test]
    fn reset_to_login_wipes_conversation_and_restores_banner() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        // Simulate an in-flight session: scrollback, a half-rendered turn,
        // a goal indicator, a stall notice, and a scrolled-up viewport.
        app.add_line("you> ", "hello");
        app.add_line("ok> ", "did a thing");
        app.current_assistant = Some("partial reply".to_string());
        app.goal_status = Some("goal active".to_string());
        app.stall = Some(StallTier::Hard(42));
        app.scroll_offset = 7;

        app.reset_to_login(80, 24);

        // The banner is re-pushed, so scrollback is non-empty but contains
        // only freshly-painted launch rows — none of the conversation lines.
        assert!(
            !app.scrollback.is_empty(),
            "reset must re-paint the startup banner"
        );
        assert!(
            !app
                .scrollback
                .iter()
                .any(|l| l.text.contains("hello") || l.text.contains("did a thing")),
            "conversation rows must be gone after reset"
        );
        assert_eq!(app.current_assistant, None, "half-rendered turn cleared");
        assert_eq!(app.goal_status, None, "goal indicator cleared");
        assert_eq!(app.stall, None, "stall notice cleared");
        assert_eq!(app.scroll_offset, 0, "viewport snapped back to bottom");

        // A fresh launch produces the same view as the reset one.
        let mut fresh = App::new("anthropic", "m", CompletionSources::default());
        fresh.push_banner(80, 24);
        let reset_text: Vec<&String> = app.scrollback.iter().map(|l| &l.text).collect();
        let fresh_text: Vec<&String> = fresh.scrollback.iter().map(|l| &l.text).collect();
        assert_eq!(
            reset_text, fresh_text,
            "reset_to_login must match a just-launched banner view"
        );
    }

    // Drive one turn through the cache-cold state machine with explicit
    // wall-clock times so the time-gap arm is deterministic. `cache_read` is the
    // tokens the daemon reported served from cache during the turn.
    fn run_turn(app: &mut App, start_ms: u64, end_ms: u64, cache_read: u32) {
        app.cache_cold.turn_start_ms = Some(start_ms);
        app.cache_cold.cache_read_at_start = app.usage.cache_read_input_tokens;
        app.record_usage_tokens(0, 0, cache_read, 0);
        app.evaluate_cache_cold_at(end_ms);
    }

    #[test]
    fn cache_cold_first_turn_is_warm() {
        let mut app = App::new("anthropic", "claude-sonnet-4-6", CompletionSources::default());
        assert!(!app.cache_cold(), "no turn yet => warm");
        run_turn(&mut app, 0, 1_000, 0);
        assert!(!app.cache_cold(), "first turn has no prior cache to expire");
    }

    #[test]
    fn cache_cold_gap_beyond_ttl_is_cold() {
        let mut app = App::new("anthropic", "claude-sonnet-4-6", CompletionSources::default());
        // Warm turn establishes a prior cache.
        run_turn(&mut app, 0, 1_000, 5_000);
        assert!(!app.cache_cold());
        // Next turn starts well after the TTL => cold.
        let start = 1_000 + origin_cost::PROMPT_CACHE_TTL_MS + 1;
        run_turn(&mut app, start, start + 500, 5_000);
        assert!(app.cache_cold(), "idle gap beyond TTL must flag cold");
    }

    #[test]
    fn cache_cold_gap_within_ttl_with_reads_is_warm() {
        let mut app = App::new("anthropic", "claude-sonnet-4-6", CompletionSources::default());
        run_turn(&mut app, 0, 1_000, 5_000);
        // Next turn starts within the TTL and reads from cache => warm.
        let start = 1_000 + origin_cost::PROMPT_CACHE_TTL_MS - 1;
        run_turn(&mut app, start, start + 500, 5_000);
        assert!(!app.cache_cold(), "quick follow-up with cache reads stays warm");
    }

    // A `PlanLine` fixture with the given status glyph and body. `id`/`holder`
    // do not affect focus-chain rendering, so they are filler.
    fn plan_line(glyph: char, body: &str) -> PlanLine {
        PlanLine {
            id: origin_plan::StepId::from_u128(0),
            indent: 0,
            status_glyph: glyph,
            content: body.to_string(),
            holder: None,
        }
    }

    #[test]
    fn focus_chain_empty_plan_is_empty() {
        assert!(render_focus_chain(&[]).is_empty());
    }

    #[test]
    fn focus_chain_maps_explicit_statuses() {
        // Pending ○, in-progress ◐, done ●, cancelled ✕.
        let lines = [
            plan_line('\u{25CF}', "first"),
            plan_line('\u{25D0}', "second"),
            plan_line('\u{25CB}', "third"),
            plan_line('\u{2715}', "fourth"),
        ];
        let chain = render_focus_chain(&lines);
        assert_eq!(
            chain,
            vec![
                "[x] first".to_string(),
                "[~] second".to_string(),
                "[ ] third".to_string(),
                "[x] fourth".to_string(),
            ],
            "explicit statuses map to checkbox markers in plan order"
        );
    }

    #[test]
    fn focus_chain_derives_focus_when_all_pending() {
        // No reported progress => promote the first step to in-progress so one
        // item reads as the active focus; the rest stay pending.
        let lines = [
            plan_line('\u{25CB}', "alpha"),
            plan_line('\u{25CB}', "beta"),
            plan_line('\u{25CB}', "gamma"),
        ];
        let chain = render_focus_chain(&lines);
        assert_eq!(
            chain,
            vec![
                "[~] alpha".to_string(),
                "[ ] beta".to_string(),
                "[ ] gamma".to_string(),
            ],
            "all-pending plan derives the first step as in-progress"
        );
    }

    #[test]
    fn focus_chain_preserves_order_and_does_not_derive_when_progress_exists() {
        // A done step means progress is reported, so no derivation kicks in and
        // the lone pending step stays pending.
        let lines = [
            plan_line('\u{25CF}', "done one"),
            plan_line('\u{25CB}', "pending two"),
        ];
        let chain = render_focus_chain(&lines);
        assert_eq!(
            chain,
            vec!["[x] done one".to_string(), "[ ] pending two".to_string()],
            "reported progress suppresses the all-pending derivation"
        );
    }

    #[test]
    fn should_notify_off_by_default() {
        // Default gate (disabled) never fires, regardless of outcome.
        assert!(!should_notify(false, true));
        assert!(!should_notify(false, false));
    }

    #[test]
    fn should_notify_fires_only_on_enabled_success() {
        assert!(should_notify(true, true));
        // Enabled but the turn failed => no chime (error line already shown).
        assert!(!should_notify(true, false));
    }

    #[test]
    fn notify_turn_complete_no_spawn_when_disabled() {
        // Disabled => returns false (no process spawn attempted).
        assert!(!notify_turn_complete(false, true));
    }

    #[test]
    fn default_app_is_byte_identical_opt_ins_off() {
        // The opt-in TUI/config layers must all default OFF so a fresh App is
        // unchanged from before this workstream.
        let app = App::new("anthropic", "m", CompletionSources::default());
        assert_eq!(app.theme, Theme::Default);
        assert!(!app.vim_active);
        assert_eq!(app.vim_mode, VimMode::Insert);
        assert!(!app.notify_desktop);
        assert!(!app.permission_ask, "permission prompting defaults off");
        assert!(app.pending_permission.is_none());
        assert_eq!(app.palette(), theme::palette(Theme::Default));
    }

    #[test]
    fn set_permission_ask_toggles_and_sets() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        assert!(!app.permission_ask, "default off");
        assert!(app.set_permission_ask(""), "no arg flips on");
        assert!(app.permission_ask);
        assert!(!app.set_permission_ask(""), "no arg flips back off");
        assert!(app.set_permission_ask("on"), "explicit on");
        assert!(!app.set_permission_ask("off"), "explicit off");
    }

    #[test]
    fn set_mouse_capture_defaults_on_and_toggles() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        assert!(app.mouse_capture, "mouse capture on by default (byte-identical)");
        assert!(!app.set_mouse_capture(""), "no arg flips off");
        assert!(app.set_mouse_capture(""), "no arg flips back on");
        assert!(!app.set_mouse_capture("off"), "explicit off");
        assert!(app.set_mouse_capture("on"), "explicit on");
    }

    #[test]
    fn chrome_is_byte_identical_for_default_then_re_themes() {
        use origin_tui::composer::Composer;
        use origin_tui::stream_widget::{Rect, StreamWidget};

        // Draw `app` and return the first chrome cell painted in the active
        // `surface_raised` (the input-card background), with its coordinate.
        fn card_bg(app: &App) -> (u16, u16) {
            let mut composer = Composer::new(60, 12);
            let mut widget = StreamWidget::new(Rect { row: 0, col: 0, cols: 60, rows: 6 });
            app.draw(&mut composer, &mut widget);
            let want = app.palette().surface_raised;
            let grid = composer.main_grid();
            for r in 0..grid.rows() {
                for c in 0..grid.cols() {
                    if grid.get(r, c).bg == want {
                        return (r, c);
                    }
                }
            }
            panic!("no surface_raised chrome cell found");
        }

        let mut app = App::new("anthropic", "m", CompletionSources::default());
        let (row, col) = card_bg(&app);
        // Default: that cell equals the legacy constant — chrome is byte-identical.
        {
            let mut composer = Composer::new(60, 12);
            let mut widget = StreamWidget::new(Rect { row: 0, col: 0, cols: 60, rows: 6 });
            app.draw(&mut composer, &mut widget);
            assert_eq!(
                composer.main_grid().get(row, col).bg,
                theme::SURFACE_RAISED,
                "Default chrome must be byte-identical to the legacy constant"
            );
        }
        // Switch to a distinctly different theme; the SAME cell must re-theme.
        assert!(app.set_theme_by_name("high-contrast"));
        let hc = theme::palette(Theme::HighContrast).surface_raised;
        assert_ne!(hc, theme::SURFACE_RAISED, "HighContrast must differ from Default");
        let mut composer = Composer::new(60, 12);
        let mut widget = StreamWidget::new(Rect { row: 0, col: 0, cols: 60, rows: 6 });
        app.draw(&mut composer, &mut widget);
        assert_eq!(
            composer.main_grid().get(row, col).bg,
            hc,
            "switching theme must re-theme the chrome"
        );
    }

    #[test]
    fn set_theme_by_name_switches_and_rejects_unknown() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        assert!(app.set_theme_by_name("dark"));
        assert_eq!(app.theme, Theme::Dark);
        assert_eq!(app.palette(), theme::palette(Theme::Dark));
        // Unknown name leaves the theme untouched.
        assert!(!app.set_theme_by_name("chartreuse"));
        assert_eq!(app.theme, Theme::Dark);
    }

    #[test]
    fn toggle_vim_flips_active_and_mode() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        assert!(app.toggle_vim());
        assert!(app.vim_active);
        assert_eq!(app.vim_mode, VimMode::Normal, "enabling vim starts in Normal");
        assert!(!app.toggle_vim());
        assert!(!app.vim_active);
        assert_eq!(app.vim_mode, VimMode::Insert, "disabling resets to Insert");
    }

    #[test]
    fn apply_vim_action_moves_cursor_and_switches_mode() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.input = "hello".to_string();
        app.cursor = 2;
        app.vim_mode = VimMode::Normal;
        // h moves left, clamped at 0.
        assert!(app.apply_vim_action(crate::input::VimAction::MoveLeft));
        assert_eq!(app.cursor, 1);
        // $ jumps to end (char count).
        assert!(app.apply_vim_action(crate::input::VimAction::LineEnd));
        assert_eq!(app.cursor, 5);
        // i switches to Insert and is consumed.
        assert!(app.apply_vim_action(crate::input::VimAction::SwitchMode(VimMode::Insert)));
        assert_eq!(app.vim_mode, VimMode::Insert);
        // Pass is not consumed.
        assert!(!app.apply_vim_action(crate::input::VimAction::Pass));
    }

    #[test]
    fn cache_cold_zero_reads_after_warm_is_cold_then_clears() {
        let mut app = App::new("anthropic", "claude-sonnet-4-6", CompletionSources::default());
        // Warm turn first.
        run_turn(&mut app, 0, 1_000, 5_000);
        assert!(!app.cache_cold());
        // Quick follow-up but the daemon reported zero cache reads => cold.
        run_turn(&mut app, 1_100, 1_600, 0);
        assert!(app.cache_cold(), "zero cache reads after a warm turn is cold");
        // The next warm turn clears the nudge.
        run_turn(&mut app, 1_700, 2_200, 5_000);
        assert!(!app.cache_cold(), "a warm turn clears the cold marker");
    }
}
