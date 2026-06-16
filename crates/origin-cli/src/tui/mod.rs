// SPDX-License-Identifier: Apache-2.0
//! Composer-driven TUI app state and draw routine.
//!
//! Features: unicode-width-aware wrapping, keyboard scrollback,
//! inline markdown (bold, headers, code), heading hierarchy,
//! code block backgrounds, side panel rendering.
//!
//! The render layer is decomposed into focused painter submodules (see the TUI
//! rework plan, `docs/superpowers/plans/2026-06-16-tui-rework.md`). [`tokens`]
//! is the single source of colors + glyphs; the painter modules emit
//! [`tokens::RenderRow`]s the draw orchestrator blits into the grid. The
//! painter modules are stubbed in Wave 0 and filled in Wave 1; the live
//! `App::draw` still uses the in-module helpers until Wave 2 wires them in.

pub mod tokens;

mod chrome;
mod codeblock;
mod composer;
mod markdown;
mod palette;
pub mod picker;
mod syntax;
mod toolblock;
mod transcript;

use std::time::{Duration, Instant};

use origin_tui::composer::Composer;
use origin_tui::grid::{Attr, Cell, Grid};
use origin_tui::stream_widget::StreamWidget;
use origin_tui::widgets::plan_panel::PlanLine;
use unicode_width::UnicodeWidthChar;

use crate::autocomplete::CompletionSources;
use crate::editor::Editor;
use crate::input::VimMode;
use crate::keybindings::KeyMap;
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

/// What an [`active_picker`](App::active_picker) interaction resolves back to.
///
/// Either a daemon permission ask (`u64` id) or an `ask_user` structured choice
/// (`String` id). The variant tells the input router which `ClientMessage` to
/// send when the picker confirms/cancels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerSource {
    /// An upgraded permission ask: Allow once / Deny / Always allow `<tool>`.
    Permission { id: u64, tool: String },
    /// An `ask_user` structured choice, correlated by its string id.
    Choice { id: String },
}

/// A live interactive picker: the pure [`picker::PickerState`] plus the
/// [`PickerSource`] that says how its outcome maps back onto the wire.
#[derive(Debug, Clone)]
pub struct PickerSession {
    pub state: picker::PickerState,
    pub source: PickerSource,
}

/// Map a permission-picker option index to the `(allow, always)` decision pair.
///
/// The pair is carried by `ClientMessage::PermissionDecision`. The option order
/// is fixed by [`App::open_permission_picker`]: `0 = Allow once`, `1 = Deny`,
/// `2 = Always allow <tool>`; any other index (defensive) denies. A *cancel*
/// (Esc) is handled by the caller as a deny — see [`permission_cancel`].
///
/// Pure + `const` so the input loop and unit tests share one source of truth.
#[must_use]
pub const fn picker_outcome_to_permission(idx: usize) -> (bool, bool) {
    match idx {
        0 => (true, false),  // Allow once
        2 => (true, true),   // Always allow <tool>
        _ => (false, false), // Deny (index 1) or any unexpected index
    }
}

/// The `(allow, always)` pair for a cancelled permission picker (Esc): treated
/// as a plain deny, never an "always" decision.
#[must_use]
pub const fn permission_cancel() -> (bool, bool) {
    (false, false)
}

/// Map a `PickerOutcome` into the `(selected, custom)` choice-decision shape.
///
/// The pair is carried by `ClientMessage::ChoiceDecision`. A `Cancelled` outcome
/// yields the daemon's "user skipped" signal: an empty `selected` and no custom
/// text.
///
/// Pure so the input loop and unit tests share one source of truth.
#[must_use]
pub fn picker_outcome_to_choice(outcome: &picker::PickerOutcome) -> (Vec<usize>, Option<String>) {
    match outcome {
        picker::PickerOutcome::Selected { indices, custom } => (indices.clone(), custom.clone()),
        picker::PickerOutcome::Cancelled => (Vec::new(), None),
    }
}

/// Which edge a scrollback line aligns to.
///
/// User prompts render `Right` (a warm band hugging the right edge, classic
/// me-right chat); the agent and everything else stays `Left`. Defaulting to
/// `Left` keeps every existing line unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineAlign {
    #[default]
    Left,
    Right,
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
    /// Which edge this line hugs. `Right` only for user prompts; everything
    /// else stays `Left` (the legacy layout).
    pub align: LineAlign,
}

impl ScrollLine {
    const fn styled(text: String, fg: u32, bg: u32, bold: bool) -> Self {
        Self {
            text,
            fg,
            bg,
            bold,
            literal: false,
            align: LineAlign::Left,
        }
    }

    /// A pre-formatted line drawn verbatim (no markdown parsing). Used for
    /// tool headers, diff rows, and streamed command output.
    const fn verbatim(text: String, fg: u32, bg: u32) -> Self {
        Self {
            text,
            fg,
            bg,
            bold: false,
            literal: true,
            align: LineAlign::Left,
        }
    }

    /// A right-aligned prose line (the user's prompt). Drawn verbatim in the warm
    /// `you` tone against the right edge — no inline-markdown parsing, so the
    /// user's literal bytes show as typed.
    const fn styled_right(text: String, fg: u32, bold: bool) -> Self {
        Self {
            text,
            fg,
            bg: 0,
            bold,
            literal: false,
            align: LineAlign::Right,
        }
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

/// Minimum number of text rows the input card reserves, even when the buffer
/// holds a single (or zero) wrapped lines. A one-row card reads as cramped —
/// the caret sits flush against the top and bottom borders. Reserving a few
/// rows gives the composer visible breathing room without growing the card as
/// the user types (it only matters below this floor).
const MIN_INPUT_ROWS: u16 = 3;

/// Hard cap on the input card's text rows. Beyond this the card stops growing
/// and the buffer scrolls internally (only the last `MAX_INPUT_ROWS` wrapped
/// lines render), so a long paste can't swallow the scrollback above.
const MAX_INPUT_ROWS: u16 = 6;

/// Hard cap on retained scrollback rows. A multi-hour session (esp. one with
/// long streamed Bash output) would otherwise grow `scrollback` without bound —
/// leaking RSS and making the per-frame wrap O(whole history), which degrades
/// the CI-gated keystroke latency. Trimmed in batches (see [`SCROLLBACK_SLACK`])
/// so the O(n) front-drain is rare.
const MAX_SCROLLBACK: usize = 5000;

/// Trim hysteresis: only trim once `scrollback` exceeds `MAX_SCROLLBACK +
/// SCROLLBACK_SLACK`, dropping back to `MAX_SCROLLBACK`. Keeps the front-drain
/// infrequent (once per `SCROLLBACK_SLACK` new rows) rather than per push.
const SCROLLBACK_SLACK: usize = 1000;

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

    pub const fn stop(&mut self) {
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

/// Which stall notice (if any) to show after `quiet` seconds of daemon silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallTier {
    /// Gentle reassurance — a long turn may just be thinking. No interrupt hint.
    Soft(u64),
}

/// Pure stall decision: `Soft` once `quiet` reaches `soft`, otherwise `None`.
///
/// Kept free of `Instant` so it is deterministically testable. There is no
/// hard/alarm tier — a slow turn reads as "still working", never as an error.
#[must_use]
pub fn stall_tier(quiet: Duration, soft: Duration) -> Option<StallTier> {
    if quiet >= soft {
        Some(StallTier::Soft(quiet.as_secs()))
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

/// A click-drag text selection over the rendered screen.
///
/// In 0-based screen-cell coordinates `(row, col)`: `anchor` is where the drag
/// started; `head` is the current/release position. Either ordering is valid —
/// consumers normalize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (u16, u16),
    pub head: (u16, u16),
}

impl Selection {
    /// `(top_left, bottom_right)` endpoints, so callers don't care whether the
    /// user dragged up-and-left or down-and-right.
    #[must_use]
    pub fn normalized(self) -> ((u16, u16), (u16, u16)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// A zero-width selection (a plain click with no drag) selects nothing.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.anchor == self.head
    }
}

// App-state aggregate: each bool is an independent, unrelated session toggle
// (plan mode, vim, desktop notify, permission prompting). Grouping them into a
// sub-struct would obscure rather than clarify.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
pub struct App {
    pub scrollback: Vec<ScrollLine>,
    /// Cursor-aware input editor (mid-buffer edit, Home/End, prompt history).
    pub input: Editor,
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
    /// daemon activity for [`STALL_SOFT_AFTER`] during an in-flight turn. `None`
    /// whenever the daemon is producing output or no turn is running. Rendered as
    /// a gentle "still working…" notice so a quiet daemon stops looking like an
    /// indefinitely-spinning spinner.
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
    /// The live interactive picker, if one is open. Drives both the upgraded
    /// permission ask (Allow once / Deny / Always allow `<tool>`) and `ask_user`
    /// structured choices through one reusable [`picker`] component. `Some` ⇒
    /// keys route to [`picker::reduce`] and the picker renders inline above the
    /// composer; `None` in the common case so input handling is byte-identical.
    pub active_picker: Option<PickerSession>,
    /// Scrollback row of the tool-activity line currently showing a `▸`
    /// "running" marker, so [`finish_tool_line`](Self::finish_tool_line) can
    /// flip it to `✔`/`✘` when the tool completes. `None` when no tool is in
    /// flight.
    running_tool_row: Option<usize>,
    /// Whether terminal mouse capture is on. Default `true`: the wheel scrolls
    /// and left-drag selects text in-app (auto-copied on release). `/mouse off`
    /// releases capture for terminal-native selection instead; scrollback stays
    /// reachable via PageUp/Shift+arrows either way.
    pub mouse_capture: bool,
    /// The most recent finalized assistant reply, for `/copy` (OSC 52). `None`
    /// until the first reply completes.
    pub last_assistant: Option<String>,
    /// Total input tokens (uncached + cache-read + cache-write) of the most
    /// recent turn — a proxy for how full the context window is. `0` before any
    /// turn. Surfaced as `ctx N%` in the status line.
    pub last_ctx_tokens: u32,
    /// Cumulative input-token total snapshotted at turn start, so the turn's own
    /// context size can be isolated as a delta at turn end.
    ctx_at_start: u32,
    /// Customizable composer keybindings (claude-code L147). Seeded once at
    /// startup from [`KeyMap::load`] (builtin defaults overlaid with
    /// `~/.origin/keybindings.toml`). The default builtin map reports the same
    /// chords the legacy reducer already owns, so an absent override file leaves
    /// the key path byte-identical. The live key handler consults it via
    /// [`Self::keymap`] before the default reducer.
    keymap: KeyMap,
    /// In-progress / completed click-drag text selection (screen-cell coords),
    /// drawn as a reverse-video highlight and auto-copied on mouse release.
    /// `None` when nothing is selected. Only meaningful while mouse capture is
    /// on (the default); with `/mouse off` the terminal selects natively.
    pub selection: Option<Selection>,
    /// The most recently rendered main-pane text — one `String` per row, glyph
    /// per cell — captured each frame *while a selection is active* so
    /// [`Self::selection_text`] extracts exactly what is on screen. Empty
    /// otherwise (no per-frame cost when not selecting).
    pub screen_text: Vec<String>,
    /// Current working directory (lossy string), resolved once at startup for
    /// the persistent top chrome strip. Cached so the per-frame `draw` path
    /// never touches the filesystem.
    cwd: String,
    /// Short git branch name for the top chrome strip, read cheaply from
    /// `.git/HEAD` once at startup (no subprocess). `None` outside a repo or on
    /// a detached HEAD. Cached so `draw` does no I/O.
    branch: Option<String>,
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

/// Best-effort short git branch name for the top chrome strip, read once at
/// startup (never on the per-frame draw path).
///
/// Walks up from the current directory to find a `.git/HEAD` and parses the
/// `ref: refs/heads/<branch>` line, returning the short branch name. Returns
/// `None` outside a repo, on a detached HEAD (HEAD holds a raw SHA), or on any
/// I/O error — the strip simply omits the `⎇ branch` segment in that case. No
/// subprocess is spawned, so this is cheap enough even though it only runs once.
fn git_branch_short() -> Option<String> {
    let start = std::env::current_dir().ok()?;
    let mut dir = start.as_path();
    loop {
        let head = dir.join(".git").join("HEAD");
        if let Ok(contents) = std::fs::read_to_string(&head) {
            let line = contents.trim();
            // `ref: refs/heads/<branch>` ⇒ on a branch; a bare SHA ⇒ detached.
            let branch = line.strip_prefix("ref:")?.trim();
            let short = branch.rsplit('/').next().unwrap_or(branch);
            return (!short.is_empty()).then(|| short.to_string());
        }
        dir = dir.parent()?;
    }
}

/// The compact first-run wordmark line. Seeded verbatim as `◆ origin` so
/// `render_scroll_line`'s `is_origin_header` path paints it in copper + bold —
/// byte-identical to the live turn's `◆ origin` role header, so identity reads
/// consistently. The old multi-line block-art banner is retired (spec §"Motion"
/// / §"Identity / wordmark"): the persistent top chrome strip now carries
/// identity (◆ origin · model · cwd · branch · clock · ctx%), so a giant
/// scroll-away banner is redundant.
const WORDMARK: &str = "\u{25C6} origin";

/// The one-line first-run tip seeded under the wordmark. Tells a new user how to
/// reach the two most useful affordances (`/` skills, `@` file mentions) without
/// the old banner's bulk.
const FIRST_RUN_TIP: &str =
    "Ask anything — type your task and press \u{23CE}.  / browses skills · @ mentions files";

impl App {
    #[must_use]
    pub fn new(provider: &'static str, model: impl Into<String>, sources: CompletionSources) -> Self {
        Self {
            scrollback: Vec::new(),
            input: Editor::new(),
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
            active_picker: None,
            running_tool_row: None,
            mouse_capture: true,
            last_assistant: None,
            last_ctx_tokens: 0,
            ctx_at_start: 0,
            keymap: KeyMap::default(),
            selection: None,
            screen_text: Vec::new(),
            cwd: std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            branch: git_branch_short(),
        }
    }

    /// Begin a click-drag selection anchored at screen cell `(row, col)`. A new
    /// anchor also clears any prior (already-copied) highlight.
    pub const fn begin_selection(&mut self, row: u16, col: u16) {
        self.selection = Some(Selection {
            anchor: (row, col),
            head: (row, col),
        });
    }

    /// Extend the in-progress selection to screen cell `(row, col)`. No-op when
    /// no selection is active.
    pub const fn update_selection(&mut self, row: u16, col: u16) {
        if let Some(sel) = &mut self.selection {
            sel.head = (row, col);
        }
    }

    /// Drop the current selection/highlight. Returns whether one was cleared, so
    /// the caller can decide whether a repaint is needed.
    pub const fn clear_selection(&mut self) -> bool {
        self.selection.take().is_some()
    }

    /// Extract the selected text from the last captured screen snapshot, exactly
    /// as it appears on screen (trailing blanks trimmed per line, empty trailing
    /// lines dropped). `None` when the selection is empty or off the snapshot.
    #[must_use]
    pub fn selection_text(&self) -> Option<String> {
        let sel = self.selection?;
        if sel.is_empty() || self.screen_text.is_empty() {
            return None;
        }
        let ((r1, c1), (r2, c2)) = sel.normalized();
        let last_row = self.screen_text.len().saturating_sub(1);
        let (r1, r2) = (r1 as usize, (r2 as usize).min(last_row));
        let mut lines: Vec<String> = Vec::new();
        for (r, row) in self.screen_text.iter().enumerate().take(r2 + 1).skip(r1) {
            let chars: Vec<char> = row.chars().collect();
            let width = chars.len();
            let start = if r == r1 { c1 as usize } else { 0 };
            // Inclusive of the cell under the release point on the final row.
            let end = if r == r2 {
                (c2 as usize + 1).min(width)
            } else {
                width
            };
            let slice: String = if start < end {
                chars[start..end].iter().collect()
            } else {
                String::new()
            };
            lines.push(slice.trim_end().to_string());
        }
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        if lines.is_empty() {
            return None;
        }
        Some(lines.join("\n"))
    }

    /// The session's composer [`KeyMap`]. Defaults to the builtin map (==
    /// current behaviour); replaced once at startup via [`Self::set_keymap`]
    /// with the user's `~/.origin/keybindings.toml` overlay.
    #[must_use]
    pub const fn keymap(&self) -> &KeyMap {
        &self.keymap
    }

    /// Install the session keymap (called once at startup with [`KeyMap::load`]).
    pub fn set_keymap(&mut self, keymap: KeyMap) {
        self.keymap = keymap;
    }

    /// Seed the opt-in vim layer's active state at startup.
    ///
    /// Passed [`crate::input::vim_active_default`] so `ORIGIN_VIM=1` starts the
    /// session in vim Normal mode; `false` (the default) leaves the composer in
    /// direct-insert mode so the key path is byte-identical. Enabling starts in
    /// [`VimMode::Normal`] (vim convention), matching [`Self::toggle_vim`].
    pub const fn set_vim_active(&mut self, active: bool) {
        self.vim_active = active;
        self.vim_mode = if active { VimMode::Normal } else { VimMode::Insert };
    }

    /// Whether the opt-in vim layer is active this session. `false` ⇒ the vim
    /// reducer is never consulted (byte-identical input handling).
    #[must_use]
    pub const fn vim_active(&self) -> bool {
        self.vim_active
    }

    /// The current vim input mode (only meaningful when [`Self::vim_active`]).
    #[must_use]
    pub const fn vim_mode(&self) -> VimMode {
        self.vim_mode
    }

    /// Total input tokens currently counted (uncached + cache-read + cache-write).
    const fn ctx_total(&self) -> u32 {
        self.usage
            .input_tokens
            .saturating_add(self.usage.cache_read_input_tokens)
            .saturating_add(self.usage.cache_creation_input_tokens)
    }

    /// The context-window fill of the most recent turn as a percentage (0–100),
    /// or `None` before any turn ran. Uses a per-model window estimate.
    #[must_use]
    pub fn ctx_pct(&self) -> Option<u8> {
        if self.last_ctx_tokens == 0 {
            return None;
        }
        let window = origin_daemon::model_window::model_context_window(&self.usage.model);
        let pct = u64::from(self.last_ctx_tokens) * 100 / u64::from(window.max(1));
        Some(u8::try_from(pct.min(100)).unwrap_or(100))
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
        // Snapshot the cumulative input total so this turn's context size (its
        // delta) can be isolated for the `ctx N%` meter.
        self.ctx_at_start = self.ctx_total();
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
        // This turn's context size = the input-token delta since turn start.
        self.last_ctx_tokens = self.ctx_total().saturating_sub(self.ctx_at_start);
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
    /// [`STALL_SOFT_AFTER`] while a turn is active, the daemon is quiet and we
    /// show a "still working…" reassurance. The animating spinner frame is
    /// intentionally excluded so a silent-but-spinning UI still registers as
    /// "no activity".
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
    pub const fn record_usage_tokens(
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
        self.suggestions = crate::suggestions::suggest(self.input.buffer(), &self.sources);
    }

    /// Seed the first-run greeting: a single compact `◆ origin` wordmark line
    /// plus one short tip. Replaces the retired multi-line ASCII block-art banner
    /// (spec §"Identity / wordmark") — the persistent top chrome strip now owns
    /// identity, so the greeting stays minimal and tasteful.
    ///
    /// `cols`/`rows` are kept in the signature (call sites + the `reset_to_login`
    /// parity test pass them) but the greeting no longer centers itself in the
    /// viewport: it's a two-row header pinned to the top of the transcript, the
    /// way a real prompt-first terminal reads.
    pub fn push_banner(&mut self, _cols: u16, _rows: u16) {
        let tok = crate::tui::tokens::Tokens::from_palette(self.palette());
        // One leading blank for breathing room above the wordmark.
        self.scrollback
            .push(ScrollLine::styled(String::new(), 0, 0, false));
        // The wordmark: seeded verbatim as `◆ origin` (indent 0) so the
        // `is_origin_header` render path paints `◆` in `tok.accent` + bold and
        // "origin" alongside it — consistent with the live turn header.
        // (The flat single-fg scrollline can't split the glyph/word into two
        // tones, so both ride the one copper header style the renderer already
        // applies; this is the intended identity color.)
        self.scrollback
            .push(ScrollLine::styled(WORDMARK.to_string(), tok.origin, 0, true));
        // The tip, hang-indented under the wordmark in muted text.
        self.scrollback.push(ScrollLine::styled(
            format!("  {FIRST_RUN_TIP}"),
            tok.muted,
            0,
            false,
        ));
        self.scrollback
            .push(ScrollLine::styled(String::new(), 0, 0, false));
    }

    /// The last `n` finalized scrollback line texts, most-recent-first. Used by
    /// [`crate::resume::augment_for_resume`] to spot a recent error when the user
    /// types a bare "continue" / "try again", so the agent resumes from the error
    /// instead of restarting.
    #[must_use]
    pub fn recent_output_lines(&self, n: usize) -> Vec<String> {
        self.scrollback
            .iter()
            .rev()
            .take(n)
            .map(|l| l.text.clone())
            .collect()
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

    pub const fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(n);
    }

    pub const fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    pub const fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn add_line(&mut self, prefix: &str, body: &str) {
        match prefix {
            "you> " => {
                // Right-aligned warm band — the placement + tone + right rule are
                // the "you" affordance, so no inline ❯ label is needed.
                self.scrollback
                    .push(ScrollLine::styled(String::new(), 0, 0, false));
                self.scrollback.push(ScrollLine::styled_right(
                    body.to_string(),
                    self.palette().user,
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
                self.scrollback.push(ScrollLine::styled(
                    format!("  {body}"),
                    self.palette().muted,
                    0,
                    false,
                ));
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
                self.scrollback.push(ScrollLine::styled(
                    format!("    {body}"),
                    self.palette().muted,
                    0,
                    false,
                ));
            }
            _ => {
                self.scrollback.push(ScrollLine::styled(
                    format!("  {body}"),
                    self.palette().body,
                    0,
                    false,
                ));
            }
        }
        self.scroll_offset = 0;
    }

    /// Open the upgraded permission picker for a daemon `PermissionAsk`. Builds a
    /// single-select picker with the fixed option order Allow once / Deny /
    /// Always allow `<tool>` (no custom row) and stores it as the
    /// [`active_picker`](App::active_picker) with a [`PickerSource::Permission`].
    /// The option indices map back to `(allow, always)` via
    /// [`picker_outcome_to_permission`].
    pub fn open_permission_picker(&mut self, id: u64, tool: &str, args: &str) {
        let question = crate::locale::linef("permission.ask", &[("tool", tool), ("args", args)]);
        let options = vec![
            picker::PickerOption {
                label: "Allow once".to_string(),
                description: None,
            },
            picker::PickerOption {
                label: "Deny".to_string(),
                description: None,
            },
            picker::PickerOption {
                label: format!("Always allow {tool}"),
                description: None,
            },
        ];
        let state = picker::PickerState {
            question,
            options,
            multi: false,
            allow_custom: false,
            cursor: 0,
            checked: Vec::new(),
            custom: None,
            typing_custom: false,
        };
        self.active_picker = Some(PickerSession {
            state,
            source: PickerSource::Permission {
                id,
                tool: tool.to_string(),
            },
        });
        self.scroll_offset = 0;
    }

    /// Open the structured-choice picker for an `ask_user` `ChoiceAsk`.
    ///
    /// Maps each `ChoiceOption` to a [`picker::PickerOption`], sets `multi` /
    /// `allow_custom`, and sizes `checked` to all-`false`. Stored as the
    /// [`active_picker`](App::active_picker) with a [`PickerSource::Choice`].
    pub fn open_choice_picker(
        &mut self,
        id: String,
        question: String,
        options: Vec<(String, Option<String>)>,
        multi: bool,
        allow_custom: bool,
    ) {
        let opts: Vec<picker::PickerOption> = options
            .into_iter()
            .map(|(label, description)| picker::PickerOption { label, description })
            .collect();
        let checked = vec![false; opts.len()];
        let state = picker::PickerState {
            question,
            options: opts,
            multi,
            allow_custom,
            cursor: 0,
            checked,
            custom: None,
            typing_custom: false,
        };
        self.active_picker = Some(PickerSession {
            state,
            source: PickerSource::Choice { id },
        });
        self.scroll_offset = 0;
    }

    /// Take the active picker, leaving `None`. Returns `Some` only while a picker
    /// is open.
    pub const fn take_picker(&mut self) -> Option<PickerSession> {
        self.active_picker.take()
    }

    /// Whether an interactive picker is currently open.
    #[must_use]
    pub const fn has_picker(&self) -> bool {
        self.active_picker.is_some()
    }

    pub fn add_colored_line(&mut self, text: String, fg: u32, bg: u32) {
        // Pre-formatted (tool output, diff rows, streamed command lines): drawn
        // verbatim so embedded `**`/backticks aren't reinterpreted as markdown.
        self.scrollback.push(ScrollLine::verbatim(text, fg, bg));
        // This is the high-volume append path (streamed Bash, diffs); cap here.
        self.trim_scrollback();
    }

    /// Append a single blank separator row so a finished tool block (its output
    /// and any "+N bytes omitted" footer) doesn't sit flush against the next
    /// tool block or the assistant's reply. No-op when the last row is already
    /// blank (so stacked producers never accumulate double gaps) and on empty
    /// scrollback (nothing to separate — no leading blank at the top).
    pub fn add_blank_line(&mut self) {
        let already_blank = self.scrollback.last().is_none_or(|l| l.text.trim().is_empty());
        if !already_blank {
            self.scrollback.push(ScrollLine::verbatim(String::new(), 0, 0));
            self.trim_scrollback();
        }
    }

    /// Drop the oldest rows when scrollback exceeds [`MAX_SCROLLBACK`] (plus
    /// slack), keeping the newest. Front-drains in a batch so the O(n) shift is
    /// rare. Indices into scrollback are fixed up: the running-tool marker row
    /// shifts down (or is forgotten if its line was trimmed). `scroll_offset` is
    /// measured from the bottom, so trimming the front never invalidates it.
    fn trim_scrollback(&mut self) {
        if self.scrollback.len() <= MAX_SCROLLBACK + SCROLLBACK_SLACK {
            return;
        }
        let drop_n = self.scrollback.len() - MAX_SCROLLBACK;
        self.scrollback.drain(0..drop_n);
        // `checked_sub` ⇒ `None` when the tracked tool line was itself trimmed.
        self.running_tool_row = self.running_tool_row.and_then(|r| r.checked_sub(drop_n));
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
    pub const fn toggle_vim(&mut self) -> bool {
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
        // Seed the working cursor (char-indexed) from the editor's live caret so
        // motions chain across consecutive vim keys and the visible caret moves.
        // `self.cursor` is the char-indexed scratch the motion arithmetic uses;
        // it is mirrored back onto the byte-indexed editor cursor below.
        self.cursor = self.input.cursor_chars();
        let len = self.input.buffer().chars().count();
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
            // Mirror the char-indexed motion onto the byte-indexed editor caret
            // so the rendered caret (which reads `input.cursor()`) tracks it.
            self.input.set_cursor_chars(c);
        }
        if let Some(m) = mode {
            self.vim_mode = m;
        }
        true
    }

    pub fn add_tool_line(&mut self, text: String) {
        self.scrollback
            .push(ScrollLine::verbatim(text, self.palette().tool, 0));
    }

    /// Push a tool-activity line with a leading `▸` "running" marker, recording
    /// its row so [`finish_tool_line`](Self::finish_tool_line) can flip it to
    /// `✔`/`✘`. If a previous tool is still marked running (e.g. a streaming
    /// Bash whose completion isn't signalled explicitly), assume it finished OK
    /// before starting the new one — so at most one `▸` is ever visible.
    pub fn start_tool_line(&mut self, text: &str) {
        self.finish_tool_line(true);
        // A streaming tool (e.g. Bash) may end without an explicit result
        // event, leaving its output flush against this header. Guarantee
        // exactly one separator row between blocks — no-op when the
        // on-result path already appended it.
        self.add_blank_line();
        self.running_tool_row = Some(self.scrollback.len());
        self.scrollback.push(ScrollLine::verbatim(
            format!("  \u{25B8} {text}"),
            self.palette().tool,
            0,
        ));
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
            let Some(text) = origin_outputstyle::resolve_display(&raw, self.output_style, hook_action) else {
                return;
            };
            if !text.is_empty() {
                // Same separator invariant as `start_tool_line`: a preceding
                // block whose trailing blank never arrived (streamed Bash
                // without a result event) must not end flush against the reply.
                self.add_blank_line();
                let tok = crate::tui::tokens::Tokens::from_palette(self.palette());
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
                        self.scrollback.push(ScrollLine::styled(
                            format!("  {task}"),
                            self.palette().body,
                            0,
                            false,
                        ));
                    } else {
                        // Classify the line via the shared markdown block model
                        // (the single source of heading/quote/rule styling). It
                        // resolves the fg + weight; ATX heading markers are then
                        // stripped (the color/weight conveys the hierarchy, so the
                        // literal hashes are clutter). Non-headings render verbatim
                        // and pick up inline markdown at render time.
                        let bs = markdown::block_style(line, &tok);
                        let bold = bs.attr.bits() & Attr::BOLD.bits() != 0;
                        let fg = if bs.fg == 0 { self.palette().body } else { bs.fg };
                        let rendered = if matches!(bs.kind, markdown::BlockKind::H(_)) {
                            strip_heading_markers(line).unwrap_or_else(|| line.to_string())
                        } else {
                            line.to_string()
                        };
                        self.scrollback
                            .push(ScrollLine::styled(format!("  {rendered}"), fg, 0, bold));
                    }
                }
                // Trailing blank line so the next user turn (or the input
                // card) has visible separation from this response.
                self.scrollback
                    .push(ScrollLine::styled(String::new(), 0, 0, false));
            }
            // Remember the rendered reply for `/copy` (OSC 52).
            self.last_assistant = Some(text);
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

    // The frame orchestration (clear → chrome → transcript → status → composer →
    // popups → selection); linear by design. Over the line cap only after rustfmt.
    #[allow(clippy::too_many_lines)]
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

            // Snapshot the active palette + the derived Tokens once per frame.
            // Painters read `tok`; the few legacy card helpers still take a
            // `theme::Palette`, so keep `pal` too. A `/theme` switch re-themes
            // everything because both derive from `self.palette()`.
            let pal = self.palette();
            let tok = crate::tui::tokens::Tokens::from_palette(pal);

            // ── Vertical regions (top → bottom) ──────────────────────────────
            //   row 0           : top chrome strip (model · cwd · ⎇ branch …)
            //   row 1           : full-width rule (+ "↑ N more" at the right)
            //   rows CHROME_H.. : transcript (copper spine in col 0)
            //   status zone     : spinner/phase/tokens/cost readout + rule
            //   composer frame  : rounded ╭──╮ … ╰──╯ field
            //   hint line       : ⏎ send · / skills · @ files …
            //
            // The composer spans the FULL width (left=0,width=cols); its text
            // column is `left+3` and clips at `right-1`, giving a text width of
            // `cols-4` — exactly what `main::input_text_width` computes, so the
            // painted caret stays in lock-step with the editor reducer across
            // wrapped lines. Do not narrow the composer without updating that fn.
            let hint_h: u16 = 1;

            // Composer geometry: text width drives the editor wrap, which must
            // match `main::input_text_width` (== cols - 4).
            let text_w = cols.saturating_sub(4) as usize;
            let buf = self.input.buffer();
            let input_layout = crate::editor::wrap_with_cursor(buf, text_w, self.input.cursor());
            let wrapped: Vec<&str> = input_layout
                .lines
                .iter()
                .map(|vl| &buf[vl.byte_start..vl.byte_end])
                .collect();
            let line_count = clamp_u16(wrapped.len()).clamp(MIN_INPUT_ROWS, MAX_INPUT_ROWS);
            // Composer frame = top border + `line_count` text rows + bottom border.
            let composer_h = line_count + 2;
            let composer_top = rows.saturating_sub(hint_h).saturating_sub(composer_h);

            // The status zone (readout + rule) and the top chrome strip are
            // *optional* furniture: on a short grid the transcript wins the space
            // so content never vanishes. Reserve each only while the rows above
            // the composer can spare them and still leave the transcript room.
            // `status_h` is 2 (readout + rule) when there's room, else 0.
            let status_h: u16 = if composer_top >= 4 { 2 } else { 0 };
            // `status_top`/`at_bottom`: the first row the transcript may NOT use
            // (kept name for the notices zone). The transcript fills
            // [chrome_h, status_top).
            let status_top = composer_top.saturating_sub(status_h);
            let at_bottom = status_top;
            let chrome_h: u16 = if rows >= 8 && status_top >= 4 { 2 } else { 0 };
            let transcript_top = chrome_h;
            // Reserve a breathing row above the status zone so the last transcript
            // line never sits flush against it — but drop it on a short grid where
            // it would steal the only transcript row, so content always renders.
            let avail = at_bottom.saturating_sub(transcript_top);
            let gap = if avail > INPUT_GAP_ROWS { INPUT_GAP_ROWS } else { 0 };

            // ── Interactive picker reservation ───────────────────────────────
            // When a picker (permission upgrade or `ask_user` choice) is open it
            // renders in the rows just above the status zone, tied to the copper
            // spine. Lay it out first so we can shrink the transcript by exactly
            // its height — the composer geometry / cursor sync are untouched
            // because the picker only consumes transcript rows.
            let picker_rows = self.layout_active_picker(cols, &tok);
            // Cap the picker to the available transcript height (minus the gap)
            // so a long question never crowds the transcript out entirely; the
            // picker reducer keeps the cursor row reachable regardless.
            let transcript_budget = avail.saturating_sub(gap) as usize;
            let picker_h = picker_rows.len().min(transcript_budget);
            // Leave a one-row breather above the picker when there's room.
            let picker_gap = usize::from(picker_h > 0 && transcript_budget > picker_h);
            let scrollback_limit = transcript_budget.saturating_sub(picker_h + picker_gap);

            let cols_usize = cols as usize;
            let mut visual_lines: Vec<VisualLine<'_>> = Vec::new();

            for entry in &self.scrollback {
                // User prompts wrap into the right band; everything else full-width.
                let right = entry.align == LineAlign::Right;
                let wrap_cols = if right {
                    right_band(cols) as usize
                } else {
                    cols_usize
                };
                wrap_into(
                    &entry.text,
                    entry.fg,
                    entry.bg,
                    entry.bold,
                    entry.literal,
                    right,
                    wrap_cols,
                    &mut visual_lines,
                );
            }
            // Live assistant turn gets an explicit `◆ origin` role header (the one
            // turn boundary the flat model still knows), then the streamed prose —
            // markdown-parsed live (literal=false) so headings/code style as they
            // stream rather than snapping at finalize. Owned outside the wrap so
            // the `&str` slices in `visual_lines` outlive it.
            let live_buf = self
                .current_assistant
                .as_ref()
                .map(|buf| format!("\u{25C6} origin\n  {buf}"));
            if let Some(text) = live_buf.as_deref() {
                wrap_into(
                    text,
                    tok.origin,
                    0,
                    false,
                    false,
                    false,
                    cols_usize,
                    &mut visual_lines,
                );
            }
            // Index of the live turn's *last* visual row, used to ride a streaming
            // caret on it. The live buffer is appended last, so when one is in
            // flight its final wrapped piece is the last element. `None` when no
            // turn is in flight (finalized scrollback therefore never carries a
            // caret — the buffer is `take`n on finalize).
            let live_last_idx: Option<usize> =
                live_buf.as_ref().and_then(|_| visual_lines.len().checked_sub(1));

            let total = visual_lines.len();
            let visible = scrollback_limit;
            let max_offset = total.saturating_sub(visible);
            let offset = self.scroll_offset.min(max_offset);
            let skip = total.saturating_sub(visible).saturating_sub(offset);

            // The transcript may not paint into the picker's reserved rows at the
            // bottom of the transcript area (`[picker_top, at_bottom)`).
            let picker_h_u16 = clamp_u16(picker_h);
            let transcript_bottom = at_bottom.saturating_sub(picker_h_u16);
            let mut row: u16 = transcript_top;
            let last_painted_row = paint_transcript_rows(
                main,
                &visual_lines,
                skip,
                visible,
                &mut row,
                transcript_bottom,
                cols,
                &tok,
            );

            // ── Streaming caret (Stage 9, Motion) ────────────────────────────
            // While a turn is in flight, ride a `▌` (in `accent`) after the last
            // glyph of the live assistant text on its last visual row. Drawn as a
            // single cell so the dirty-only diff repaints just that cell, and
            // absent once the turn finalizes (the live buffer is `take`n, so
            // `live_last_idx` is `None`).
            // deferred: the per-running-tool micro-spinner + ▸→✔ completion tick
            // (spec §Motion) need structured tool-block data the flat scrollback
            // model doesn't carry yet — out of scope for this pass.
            if let Some(idx) = live_last_idx {
                // The live turn's last visual line lands at `transcript_top +
                // (idx - skip)` iff it's within the painted window.
                if let Some(crow) = caret_row_for(idx, skip, transcript_top, last_painted_row) {
                    paint_streaming_caret(main, crow, cols, &tok);
                }
            }

            // ── Interactive picker (Stage 5b) ────────────────────────────────
            // Render the laid-out picker rows in their reserved region, tied to
            // the copper spine in col 0 so it reads as part of the transcript.
            Self::draw_picker_zone(main, transcript_bottom, at_bottom, cols, &picker_rows, &tok);

            // ── Top chrome strip + rule (Stage 1) ────────────────────────────
            self.draw_chrome_top(main, cols, chrome_h, &tok, offset);

            // ── Bottom status zone (Stage 5): readout + seating rule ─────────
            self.draw_status_zone(main, status_top, status_h, cols, &tok);
            // Goal / stall / permission notices overpaint the readout row — they
            // are the more urgent signal when present.
            self.draw_notices_zone(main, status_top, cols, &tok);

            // ── Composer frame + caret + ghost completion (Stage 4) ──────────
            let field_region = crate::tui::tokens::Region::new(composer_top, 0, cols, composer_h);
            self.draw_composer(main, field_region, &wrapped, &input_layout, &tok);

            // ── Hint line (Stage 4) ──────────────────────────────────────────
            let hint_region = crate::tui::tokens::Region::new(rows.saturating_sub(hint_h), 0, cols, hint_h);
            crate::tui::composer::draw_hint(
                main,
                hint_region,
                self.spinner.active || self.goal_status.is_some(),
                &tok,
            );

            // ── Slash / mention popup above the composer (Stage 5) ───────────
            self.draw_suggestions_zone(main, composer_top, cols, &tok);

            // Reverse-video overlay for an active click-drag selection, drawn last
            // so it sits on top of all content.
            if let Some(sel) = self.selection {
                apply_selection_highlight(main, sel);
            }
        }
        clear_prompt_grid(composer.prompt_grid(), self.palette());
    }

    /// Lay out the active picker into [`RenderRow`](crate::tui::tokens::RenderRow)s,
    /// or an empty `Vec` when none is open.
    ///
    /// The spine gutter occupies col 0, so the picker is laid out into the
    /// remaining width (`cols - 2`) so its descriptions clip correctly against
    /// the right edge.
    fn layout_active_picker(
        &self,
        cols: u16,
        tok: &crate::tui::tokens::Tokens,
    ) -> Vec<crate::tui::tokens::RenderRow> {
        self.active_picker
            .as_ref()
            .map(|sess| crate::tui::picker::layout_picker(&sess.state, cols.saturating_sub(2), tok))
            .unwrap_or_default()
    }

    /// Stage 5b — paint the interactive picker into its reserved region.
    ///
    /// The region is `[top, bottom)` at the foot of the transcript, with the
    /// copper `┃` spine in col 0 so it reads as part of the live turn. No-op when
    /// no picker is open or the region is empty. The pre-laid-out `rows` are
    /// blitted starting at col 2 (after the spine gutter `┃ `); rows beyond the
    /// region are clipped (the reducer keeps the cursor reachable regardless).
    fn draw_picker_zone(
        main: &mut Grid,
        top: u16,
        bottom: u16,
        cols: u16,
        rows: &[crate::tui::tokens::RenderRow],
        tok: &crate::tui::tokens::Tokens,
    ) {
        if rows.is_empty() || top >= bottom {
            return;
        }
        let mut r = top;
        for rr in rows {
            if r >= bottom {
                break;
            }
            // Copper spine gutter in col 0 (matching the transcript spine).
            main.put(
                r,
                0,
                Cell::new(crate::tui::tokens::glyph::SPINE, tok.spine, 0, Attr::PLAIN),
            );
            // Blit the picker row after the spine gutter (col 2).
            crate::tui::tokens::blit_row(main, r, 2, cols, rr);
            r = r.saturating_add(1);
        }
    }

    /// Stage 1 — paint the persistent top chrome strip (row 0) + a full-width
    /// rule (row 1), with the "↑ N more" scroll indicator riding the rule's
    /// right edge so it never collides with the strip. No-op when the terminal
    /// is too short to spare the two rows (`chrome_h == 0`).
    fn draw_chrome_top(
        &self,
        main: &mut Grid,
        cols: u16,
        chrome_h: u16,
        tok: &crate::tui::tokens::Tokens,
        offset: usize,
    ) {
        if chrome_h == 0 {
            return;
        }
        let live_elapsed = self
            .turn_started
            .map_or(self.usage.elapsed, |t| self.usage.elapsed + t.elapsed());
        let ctx = crate::tui::chrome::ChromeCtx {
            model: self.usage.model.clone(),
            cwd: self.cwd.clone(),
            branch: self.branch.clone(),
            elapsed: format_elapsed_clock(live_elapsed),
            ctx_pct: self.ctx_pct().unwrap_or(0),
        };
        crate::tui::chrome::draw_top(main, crate::tui::tokens::Region::new(0, 0, cols, 1), &ctx, tok);
        // Full-width rule on row 1, in accent_dim, seating the transcript.
        for c in 0..cols {
            main.put(1, c, Cell::new('\u{2500}', tok.accent_dim, 0, Attr::PLAIN));
        }
        // "↑ N more" indicator overpaints the right edge of the rule.
        if offset > 0 {
            let indicator = format!(" \u{2191} {offset} more ");
            let w = char_display_width(&indicator);
            let start = cols.saturating_sub(w.saturating_add(1));
            write_str_styled(
                main,
                1,
                start,
                &indicator,
                cols,
                Style {
                    fg: tok.accent,
                    bg: 0,
                    bold: false,
                },
            );
        }
    }

    /// Stage 5 — paint the bottom status zone (a spinner/phase/tokens/cost
    /// readout plus a seating rule) via [`chrome::draw_status`], in its own quiet
    /// zone above the composer (moved out of the input card).
    fn draw_status_zone(
        &self,
        main: &mut Grid,
        status_top: u16,
        status_h: u16,
        cols: u16,
        tok: &crate::tui::tokens::Tokens,
    ) {
        let live_elapsed = self
            .turn_started
            .map_or(self.usage.elapsed, |t| self.usage.elapsed + t.elapsed());
        let phase = turn_phase(self.spinner.active, self.current_assistant.as_deref());
        // Phase prefers the live goal status when one is set (so the zone shows
        // the active operation), else the localized Thinking/Responding label.
        let phase_owned = self
            .goal_status
            .clone()
            .or_else(|| localize_phase(phase).map(|p| format!("{}s {p}", live_elapsed.as_secs())));
        let st = crate::tui::chrome::StatusCtx {
            spinner: self.spinner.active.then(|| self.spinner.frame_char().to_string()),
            phase: phase_owned,
            tokens: self.usage.input_tokens.saturating_add(self.usage.output_tokens),
            cost: Some(crate::status::cost_usd(&self.usage)),
            in_flight: self.spinner.active || self.goal_status.is_some(),
        };
        crate::tui::chrome::draw_status(
            main,
            crate::tui::tokens::Region::new(status_top, 0, cols, status_h),
            &st,
            tok,
        );
    }

    /// Render the goal / stall / permission notices on the status readout row.
    /// They overpaint the quiet readout because each is the more urgent signal
    /// when present; priority: permission > stall > goal (goal already shows as
    /// the readout phase, so only stall/permission need to overpaint).
    fn draw_notices_zone(
        &self,
        main: &mut Grid,
        status_top: u16,
        cols: u16,
        tok: &crate::tui::tokens::Tokens,
    ) {
        let rows = main.rows();
        if status_top >= rows {
            return;
        }
        // INT-3: permission asks now render through the interactive picker (see
        // `draw_picker_zone`) instead of the cramped y/n readout line, so the
        // legacy `pending_permission` branch is retired here. The picker is the
        // higher-priority signal and draws in the transcript foot.
        // Stall watchdog: a gentle "still working…" reassurance overpaints the
        // readout (muted, never an alarm).
        if let Some(StallTier::Soft(secs)) = self.stall {
            for c in 0..cols {
                main.put(status_top, c, Cell::new(' ', 0, 0, Attr::PLAIN));
            }
            write_str_styled(
                main,
                status_top,
                1,
                &format!("\u{2026} still working\u{2026} {secs}s"),
                cols,
                Style {
                    fg: tok.muted,
                    bg: 0,
                    bold: false,
                },
            );
        }
    }

    /// Stage 4 — paint the framed composer field via [`composer::draw_field`]
    /// (rounded frame + `›` prompt + placeholder + soft-wrap cues + "▴ more
    /// above"), then overlay the reverse-video caret and the ghost-suggestion
    /// completion. The text geometry is `region.left+3 .. region.right()-1`, a
    /// width of `cols-4`, matching `main::input_text_width` so the caret tracks
    /// typing exactly across wrapped lines.
    fn draw_composer(
        &self,
        main: &mut Grid,
        region: crate::tui::tokens::Region,
        wrapped: &[&str],
        layout: &crate::editor::Layout,
        tok: &crate::tui::tokens::Tokens,
    ) {
        // Internal scroll: show only the last MAX_INPUT_ROWS wrapped rows.
        let max_rows = MAX_INPUT_ROWS as usize;
        let scroll_top = wrapped.len().saturating_sub(max_rows);
        let lines: Vec<String> = wrapped.iter().map(|s| (*s).to_string()).collect();
        let ed = crate::tui::composer::EditorView {
            lines,
            cursor_row: layout.cursor_row,
            cursor_col: layout.cursor_col,
            placeholder: "Ask anything\u{2026}".to_string(),
            scroll_top,
            max_rows: MAX_INPUT_ROWS,
        };
        // The painter draws the frame, text, soft-wrap cues, and "▴ more above".
        crate::tui::composer::draw_field(main, region, &ed, tok);

        // Text geometry inside the frame (mirrors composer::draw_field).
        let text_col = region.left + 3;
        let last_col = region.right().saturating_sub(1); // right border column
        let first_content = region.top + 1;
        let frame_bottom = region.bottom().saturating_sub(1); // bottom border row

        // Ghost-suggestion completion: trailing dim text after the last input
        // row's content, when a single unique candidate is being offered.
        if !self.suggestions.ghost.is_empty() && !wrapped.is_empty() {
            let last_idx = wrapped.len() - 1;
            if last_idx >= scroll_top {
                let vis_row = clamp_u16(last_idx - scroll_top);
                let r = first_content + vis_row;
                if r < frame_bottom {
                    let gc = text_col + char_display_width(wrapped[last_idx]);
                    write_str_styled(
                        main,
                        r,
                        gc,
                        &self.suggestions.ghost,
                        last_col,
                        Style {
                            fg: tok.muted,
                            bg: tok.raised,
                            bold: false,
                        },
                    );
                }
            }
        }

        // Reverse-video caret at the insertion point — drawn LAST (over the text,
        // placeholder, and ghost) so the user always sees the cursor even when it
        // sits at end-of-input where the ghost begins. Preserves the underlying
        // glyph so the character stays readable under the reverse cell.
        if layout.cursor_row >= scroll_top {
            let vis_row = clamp_u16(layout.cursor_row - scroll_top);
            let r = first_content + vis_row;
            let c = text_col + clamp_u16(layout.cursor_col);
            if r < frame_bottom && c < last_col {
                let glyph = char::from_u32(main.get(r, c).glyph)
                    .filter(|&g| g != ' ' && g != '\0')
                    .unwrap_or(' ');
                main.put(r, c, Cell::new(glyph, tok.bright, tok.raised, Attr::REVERSE));
            }
        }
    }

    /// Stage 5 — render the autocomplete popup in the rows directly above the
    /// composer. Slash/skill candidates route through [`palette::draw_slash`] so
    /// each shows its DESCRIPTION (threaded from `SuggestionState::descriptions`,
    /// previously computed then discarded). Non-slash candidates (e.g.
    /// `{workflow:…}`) have no descriptions and render with an empty desc column.
    fn draw_suggestions_zone(
        &self,
        main: &mut Grid,
        composer_top: u16,
        cols: u16,
        tok: &crate::tui::tokens::Tokens,
    ) {
        let total = self.suggestions.candidates.len();
        if total == 0 || composer_top == 0 {
            return;
        }
        let win = crate::suggestions::MAX_VISIBLE;
        let offset = crate::suggestions::scroll_offset(total, self.suggestions.selected);
        let visible = total.saturating_sub(offset).min(win);
        let count = clamp_u16(visible);
        // The popup sits in the `count` rows immediately above the composer.
        let popup_top = composer_top.saturating_sub(count);
        let items: Vec<crate::tui::palette::SlashItem> = self
            .suggestions
            .candidates
            .iter()
            .enumerate()
            .map(|(i, name)| crate::tui::palette::SlashItem {
                name: name.clone(),
                desc: self.suggestions.descriptions.get(i).cloned().unwrap_or_default(),
            })
            .collect();
        crate::tui::palette::draw_slash(
            main,
            crate::tui::tokens::Region::new(popup_top, 0, cols, count),
            &items,
            self.suggestions.selected,
            tok,
        );
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

/// Overlay a click-drag selection as reverse-video on the main grid. For each
/// cell within the (normalized) selection range, OR in the `REVERSE` attribute,
/// preserving glyph/fg/bg so the highlighted text stays readable. Screen-cell
/// coordinates map 1:1 to the main pane (offset `0,0`); out-of-range rows/cols
/// (e.g. into the side panel) are clamped away.
fn apply_selection_highlight(main: &mut Grid, sel: Selection) {
    let rows = main.rows();
    let cols = main.cols();
    let ((r1, c1), (r2, c2)) = sel.normalized();
    let mut r = r1;
    while r <= r2 && r < rows {
        let start = if r == r1 { c1 } else { 0 };
        let end = if r == r2 { c2 } else { cols.saturating_sub(1) };
        let mut c = start;
        while c <= end && c < cols {
            let mut cell = main.get(r, c);
            cell.attr |= Attr::REVERSE.bits();
            main.put(r, c, cell);
            c = c.saturating_add(1);
        }
        r = r.saturating_add(1);
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
        self.trim_scrollback();
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
        side.put(r, 0, Cell::new('\u{2502}', pal.border, pal.panel_bg, Attr::PLAIN));
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
        side.put(1, c, Cell::new('\u{2500}', pal.border, pal.panel_bg, Attr::PLAIN));
    }
    side.put(1, 0, Cell::new('\u{251C}', pal.border, pal.panel_bg, Attr::PLAIN));

    // cline L171: render the plan as a live focus-chain checkbox todo list.
    // The checkbox marker (`[ ]`/`[~]`/`[x]`) carries the GFM-style state; the
    // existing status glyph stays as a colored leading dot so progress reads at
    // a glance.
    let checklist = render_focus_chain(plan_lines);
    let mut row: u16 = 2;
    // `row` is a screen-row render cursor starting at 2 (not a 0-based index),
    // and the loop early-breaks at `rows`, so enumerate doesn't fit cleanly.
    #[allow(clippy::explicit_counter_loop)]
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
    // Control characters (`\r`, `\t`, ESC, `\b`, …) have NO display width and must
    // never be rendered into a cell: emitting one raw moves the terminal cursor
    // (e.g. `\r` returns it to column 0), corrupting the row and permanently
    // desyncing the damage-diff shadow grid from the screen. Treat them as
    // zero-width so the render skips them entirely. This matters on Windows,
    // where tool/file output carries `\r\n` line endings — the wrapper splits on
    // `\n` but leaves the `\r`.
    if c.is_control() {
        return 0;
    }
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

/// Word-boundary-wrap one logical line (no embedded `\n`) to `width` columns,
/// returning the wrapped pieces as slices into `s`.
///
/// Breaks at the last space that fits rather than mid-word; a single word longer
/// than `width` is hard-broken (unavoidable). Leading spaces on a continuation
/// line are dropped. `width == 0` or an empty `s` yields `s` unwrapped.
/// Word-boundary wrap with a distinct width for continuation lines: the
/// first piece wraps at `first_width`, every later piece at `rest_width`.
/// The caller renders continuations shifted right by the difference (hanging
/// indent), so each rendered row still spans the full terminal width.
fn wrap_segment_hanging(s: &str, first_width: usize, rest_width: usize) -> Vec<&str> {
    if first_width == 0 || s.is_empty() {
        return vec![s];
    }
    // (byte offset, char, display width) per char.
    let chars: Vec<(usize, char, usize)> = s
        .char_indices()
        .map(|(byte, ch)| (byte, ch, UnicodeWidthChar::width(ch).unwrap_or(1)))
        .collect();
    let len = chars.len();
    let mut width = first_width;
    let mut lines: Vec<&str> = Vec::new();
    let mut start = 0usize; // char index of the current line's first char
    let mut col = 0usize; // accumulated display width of the current line
    let mut last_space: Option<usize> = None; // char index of the last space seen
    let mut i = 0usize;
    while i < len {
        let (byte, ch, cw) = chars[i];
        // Record the FIRST space of a run as the break point, so breaking there
        // drops the whole run and the wrapped line has no trailing space.
        if ch.is_whitespace() && (i == 0 || !chars[i - 1].1.is_whitespace()) {
            last_space = Some(i);
        }
        if col + cw > width && i > start {
            width = rest_width;
            if let Some(sp) = last_space.filter(|&sp| sp > start) {
                // Break at the space: drop it; continuation skips further spaces.
                lines.push(&s[chars[start].0..chars[sp].0]);
                let mut ns = sp + 1;
                while ns < len && chars[ns].1.is_whitespace() {
                    ns += 1;
                }
                start = ns;
                i = ns;
            } else {
                // No usable space (a word longer than the column): hard-break.
                lines.push(&s[chars[start].0..byte]);
                start = i;
            }
            col = 0;
            last_space = None;
            continue;
        }
        col += cw;
        i += 1;
    }
    if start < len {
        lines.push(&s[chars[start].0..]);
    } else if lines.is_empty() {
        lines.push("");
    }
    lines
}

/// If `line` is an ATX markdown heading (`# `/`## `/`### ` after optional
/// leading whitespace), return the text with the `#` markers and the one
/// following space removed. Hierarchy is conveyed by the heading color/weight
/// (from [`markdown::block_style`]), so the literal hashes are visual clutter.
/// Non-headings (and `#hashtag` with no space, or 4+ hashes) return `None` and
/// render verbatim.
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

/// Localize a [`turn_phase`] label for display.
///
/// The pre-token `"Thinking"` state routes through the `thinking` catalog key,
/// whose English literal is exactly `"Thinking"` — so the default-locale output
/// is byte-identical, while `--lang`/`$LANG` renders e.g. "Pensando". The
/// `"Responding"` state has no catalog key and passes through unchanged. `None`
/// (idle) yields `None`.
fn localize_phase(phase: Option<&str>) -> Option<String> {
    match phase {
        Some("Thinking") => Some(crate::locale::line("thinking").to_string()),
        Some(other) => Some(other.to_string()),
        None => None,
    }
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
        if w == 2 {
            grid.put(row, c + 1, Cell::continuation(style.bg));
        }
        c += w;
    }
    c
}

/// Paint the visible transcript rows (`visual_lines[skip..]`, capped at
/// `visible` and at `transcript_bottom`) starting at `*row`, advancing `*row`
/// per painted line. Returns the first row past the last painted line (the
/// exclusive bottom of the painted window), so the caller can tell which visual
/// lines actually made it on screen. Extracted from `draw` to keep it under the
/// line cap.
#[allow(clippy::too_many_arguments)]
fn paint_transcript_rows(
    grid: &mut Grid,
    visual_lines: &[VisualLine<'_>],
    skip: usize,
    visible: usize,
    row: &mut u16,
    transcript_bottom: u16,
    cols: u16,
    tok: &crate::tui::tokens::Tokens,
) -> u16 {
    for vl in visual_lines.iter().skip(skip).take(visible) {
        if *row >= transcript_bottom {
            break;
        }
        render_scroll_line(grid, *row, vl, cols, tok);
        *row = row.saturating_add(1);
    }
    *row
}

/// The grid row the live turn's last visual line (`idx`) landed on, or `None`
/// when it scrolled out of the painted window. The painted window covers visual
/// indices `[skip, ..)` mapped to rows `[transcript_top, last_painted_row)`, so
/// `idx` is on screen iff `idx >= skip` and its row is below `last_painted_row`.
fn caret_row_for(idx: usize, skip: usize, transcript_top: u16, last_painted_row: u16) -> Option<u16> {
    let offset_in_window = idx.checked_sub(skip)?;
    let crow = transcript_top.saturating_add(clamp_u16(offset_in_window));
    (crow < last_painted_row).then_some(crow)
}

/// Paint the streaming caret (`▌`, in `accent`) on `row`, immediately after the
/// last rendered glyph of the live assistant text.
///
/// Scans the already-painted row from the right for the last non-blank cell.
/// Reading the *rendered* grid keeps this accurate even after inline-markdown
/// markers are stripped, then places the caret in the first column past it.
/// Respects width: the caret is one cell wide and is placed only when a free
/// column remains before `cols`; on a full row it is skipped (never wrapped onto
/// a new row), so the cell accounting below is never disturbed. Only this one
/// cell changes frame-to-frame, keeping the dirty-only/damage-diff repaint minimal.
fn paint_streaming_caret(grid: &mut Grid, row: u16, cols: u16, tok: &crate::tui::tokens::Tokens) {
    if cols == 0 {
        return;
    }
    // Find the rightmost occupied cell. A wide-glyph continuation counts as
    // occupied so the caret lands past the full wide glyph, not on its tail.
    let mut last_occupied: Option<u16> = None;
    for col in 0..cols {
        let cell = grid.get(row, col);
        let blank = cell.glyph == 0 || cell.glyph == u32::from(' ');
        if !blank || cell.is_continuation() {
            last_occupied = Some(col);
        }
    }
    // The first free column after the text; an empty row (no content yet) sits
    // the caret at col 0 so an empty live turn still shows it.
    let caret_col = last_occupied.map_or(0, |c| c.saturating_add(1));
    if caret_col >= cols {
        // Row is full — skip rather than overflow or wrap onto the next row.
        return;
    }
    grid.put(
        row,
        caret_col,
        Cell::new(crate::tui::tokens::glyph::CARET, tok.accent, 0, Attr::PLAIN),
    );
}

/// Render one wrapped visual line into the grid.
///
/// Literal lines (pre-formatted tool/diff/command output) are written verbatim
/// so `**`/backticks survive; prose lines go through the inline-markdown
/// renderer so `**bold**` and `` `code` `` style correctly.
///
/// Stage 2/3 of the TUI rework layers three things over the legacy line render:
///   * a copper `┃` **spine** in the gutter (col 0) for every non-blank row, so
///     the transcript reads as one threaded column;
///   * **role placement** — a user prompt (`vl.right`) renders right-aligned in
///     the warm `you` tone with a mirrored right rule, while the `◆ origin`
///     marker (prepended to the live assistant turn) renders in copper on the
///     left — giving each role a clear, asymmetric affordance;
///   * **deeper markdown** via [`markdown::render_inline`] (italic/strike/links
///     on top of bold/code) for prose, and a **syntax tint** (via
///     [`syntax::tint`]) for code-block rows (those carrying the `code_bg`).
fn render_scroll_line(
    grid: &mut Grid,
    row: u16,
    vl: &VisualLine<'_>,
    cols: u16,
    tok: &crate::tui::tokens::Tokens,
) {
    let style = Style {
        fg: vl.fg,
        bg: vl.bg,
        bold: vl.bold,
    };
    // Pre-fill the hang gap with the line's background so wrapped code-block
    // rows stay a solid band instead of showing a default-bg notch.
    if vl.indent > 0 && vl.bg != 0 {
        for c in 0..vl.indent.min(cols) {
            grid.put(row, c, Cell::new(' ', vl.fg, vl.bg, Attr::PLAIN));
        }
    }

    // Right-aligned user prompt: a warm band hugging the right edge. Rendered
    // verbatim (not markdown) so the user's literal bytes show as typed, with a
    // mirrored copper rule at the far-right column. Placed BEFORE the left-spine
    // block so right rows never draw a left gutter spine.
    if vl.right {
        let w = char_display_width(vl.text);
        // Exclusive right edge for content; leaves a 1-col gap before the rule at
        // `cols - 1`.
        let content_right = cols.saturating_sub(2);
        let start = content_right.saturating_sub(w).max(1);
        write_str_styled(
            grid,
            row,
            start,
            vl.text,
            content_right,
            Style {
                fg: tok.you,
                bg: 0,
                bold: vl.bold,
            },
        );
        // Mirrored right accent rule for non-blank rows, in the user tone.
        if !vl.text.trim_start().is_empty() && cols > 0 {
            grid.put(
                row,
                cols - 1,
                Cell::new(crate::tui::tokens::glyph::SPINE, tok.you, 0, Attr::PLAIN),
            );
        }
        return;
    }

    let trimmed = vl.text.trim_start();
    // Detect role markers (only on the first wrapped piece, indent == 0).
    let is_origin_header = vl.indent == 0 && trimmed == "\u{25C6} origin"; // ◆ origin

    if is_origin_header {
        // Assistant turn affordance: ◆ origin in copper, bold, at col 2.
        write_str_styled(
            grid,
            row,
            2,
            "\u{25C6} origin",
            cols,
            Style {
                fg: tok.origin,
                bg: 0,
                bold: true,
            },
        );
    } else if vl.literal {
        write_str_styled(grid, row, vl.indent, vl.text, cols, style);
    } else if vl.bg == tok.code_bg && vl.bg != 0 {
        // Code-block row: lexically tint it via the dep-free syntax lexer (the
        // visual essence of `codeblock::layout_code`; the framed label row can't
        // be reconstructed from the flat post-wrap model — see report).
        render_code_tint(grid, row, vl.text, cols, vl.indent, tok);
    } else {
        // Prose: deeper inline markdown (italic/strike/links + bold/code).
        markdown::render_inline(grid, row, vl.text, cols, style, tok, vl.indent);
    }

    // Spine in the gutter (col 0) for non-blank transcript rows. Drawn last and
    // ONLY when col 0 is currently blank/space, so it sits over the content's
    // leading indent and never clobbers a glyph from content that starts flush
    // left (raw diffs / command output via `add_colored_line`). This reconciles
    // the spine with the existing `  ` content indent without double-indenting.
    if !trimmed.is_empty() && cols > 0 {
        let g0 = grid.get(row, 0).glyph;
        let blank = g0 == 0 || g0 == u32::from(' ');
        if blank {
            grid.put(
                row,
                0,
                Cell::new(crate::tui::tokens::glyph::SPINE, tok.spine, 0, Attr::PLAIN),
            );
        }
    }
}

/// Lexically tint one code-block row using [`syntax::tint`], mapping token
/// classes to theme colors. Falls back to `code_fg` for untinted gaps; the whole
/// row stays on the `code_bg` band. Untintable languages (or plain rows) render
/// in `code_fg` verbatim.
fn render_code_tint(
    grid: &mut Grid,
    row: u16,
    text: &str,
    max_cols: u16,
    start_col: u16,
    tok: &crate::tui::tokens::Tokens,
) {
    use crate::tui::syntax::{self, Tok};
    // The flat model doesn't carry the fence language to each wrapped row, so we
    // best-effort tint as Rust (the dominant code in this tool's transcripts);
    // unknown tokens simply fall back to code_fg, so a mis-guess only means a few
    // un-tinted identifiers, never corruption.
    let spans = syntax::tint(syntax::Lang::Rust, text);
    let map = |k: Tok| match k {
        Tok::Keyword => tok.accent,
        Tok::Str => tok.ok,
        Tok::Comment => tok.muted,
        Tok::Num => tok.warn,
        Tok::Ident => tok.code_fg,
        Tok::Punct => tok.body,
    };
    let mut col = start_col;
    let bytes = text.len();
    let mut cursor = 0usize;
    let emit = |grid: &mut Grid, from: usize, to: usize, fg: u32, col: &mut u16| {
        if let Some(slice) = text.get(from..to) {
            *col = write_span(
                grid,
                row,
                *col,
                slice,
                max_cols,
                Style {
                    fg,
                    bg: tok.code_bg,
                    bold: false,
                },
            );
        }
    };
    for sp in &spans {
        if sp.start >= bytes {
            break;
        }
        if sp.start > cursor {
            emit(grid, cursor, sp.start, tok.code_fg, &mut col);
        }
        let end = (sp.start + sp.len).min(bytes);
        emit(grid, sp.start, end, map(sp.kind), &mut col);
        cursor = end;
    }
    if cursor < bytes {
        emit(grid, cursor, bytes, tok.code_fg, &mut col);
    }
    // Fill the rest of the band to max_cols so the code block reads as a block.
    while col < max_cols {
        grid.put(row, col, Cell::new(' ', 0, tok.code_bg, Attr::PLAIN));
        col += 1;
    }
}

/// Format a session elapsed duration as a compact clock for the top chrome
/// strip: `12s`, `1m 04s`, `2h 03m`. Mirrors the spirit of the status line's
/// `{secs:.1}s` but reads as a wall clock at the larger scales the persistent
/// strip lives at.
fn format_elapsed_clock(d: Duration) -> String {
    let total = d.as_secs();
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
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
    /// Columns to shift this piece right when drawn. Non-zero only on wrap
    /// continuations, which hang under their source line's leading indent
    /// instead of snapping back to column 0.
    indent: u16,
    /// When `true`, this row is rendered right-aligned (a user prompt). Each row
    /// is positioned independently against the right edge — `indent` is forced to
    /// 0 (no hanging indent) since alignment, not a fixed start column, places it.
    right: bool,
}

/// Width to wrap a right-aligned user message at: ~3/5 of the terminal, clamped
/// to leave a left gutter and a right margin + rule. Degrades safely on tiny
/// grids: on a narrow terminal the upper bound (`cols-4`) drops below the
/// preferred floor (`min(cols,16)`), so the bounds are ordered as `lo =
/// min(floor, hi)` BEFORE clamping — `u16::clamp` panics when `min > max`, so
/// the order must be guaranteed (regression: a raw `clamp(16, cols-4)` crashed
/// the render on any resize to 1..=19 cols once a user prompt was in scrollback).
/// Returns 0 only when `cols` is 0. The `u32` intermediate avoids overflow on a
/// very wide terminal; the result is always `<= cols`, so the `try_from` never
/// actually fails (the `unwrap_or(cols)` is just a lint-clean fallback).
fn right_band(cols: u16) -> u16 {
    let b = u16::try_from(u32::from(cols) * 3 / 5).unwrap_or(cols);
    let hi = cols.saturating_sub(4);
    let lo = cols.min(16).min(hi);
    b.clamp(lo, hi)
}

#[allow(clippy::too_many_arguments)]
fn wrap_into<'a>(
    text: &'a str,
    fg: u32,
    bg: u32,
    bold: bool,
    literal: bool,
    right: bool,
    cols: usize,
    out: &mut Vec<VisualLine<'a>>,
) {
    for sub in text.split('\n') {
        if cols == 0 {
            continue;
        }
        if sub.is_empty() {
            out.push(VisualLine {
                text: "",
                fg,
                bg,
                bold,
                literal,
                indent: 0,
                right,
            });
            continue;
        }
        if right {
            // Right-aligned: no hanging indent — every piece is positioned
            // independently against the right edge by the renderer. Wrap at the
            // full band width with a flat start column for each row.
            for piece in wrap_segment_hanging(sub, cols, cols) {
                out.push(VisualLine {
                    text: piece,
                    fg,
                    bg,
                    bold,
                    literal,
                    indent: 0,
                    right,
                });
            }
            continue;
        }
        // Hanging indent: continuation pieces align under the source line's
        // leading spaces instead of snapping back to column 0. Disabled when
        // the indent would eat half the width (pathologically narrow grids).
        let lead = sub.len() - sub.trim_start_matches(' ').len();
        let indent = if lead * 2 >= cols { 0 } else { lead };
        // Word-boundary wrap so prose breaks between words, not mid-word.
        let mut first = true;
        for piece in wrap_segment_hanging(sub, cols, cols - indent) {
            out.push(VisualLine {
                text: piece,
                fg,
                bg,
                bold,
                literal,
                indent: if first { 0 } else { clamp_u16(indent) },
                right,
            });
            first = false;
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
        if w == 2 {
            grid.put(row, c + 1, Cell::continuation(style.bg));
        }
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
#[allow(clippy::panic, clippy::unwrap_used)] // panic!/unwrap are idiomatic test assertions
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
        wrap_into("ab\u{276F}cd", 0, 0, false, false, false, 4, &mut lines);
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
            app.scrollback
                .last()
                .is_some_and(|l| l.text.contains('\u{2718}') && l.fg == theme::RED),
            "failure shows ✘ in red"
        );
    }

    #[test]
    fn starting_next_tool_resolves_previous_running_marker() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.start_tool_line("[Bash] first");
        app.start_tool_line("[Bash] second");
        let ticks = app
            .scrollback
            .iter()
            .filter(|l| l.text.contains('\u{2714}'))
            .count();
        let arrows = app
            .scrollback
            .iter()
            .filter(|l| l.text.contains('\u{25B8}'))
            .count();
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
    fn scrollback_is_bounded_and_keeps_newest() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        let total = MAX_SCROLLBACK + SCROLLBACK_SLACK + 200;
        for i in 0..total {
            app.add_colored_line(format!("line {i}"), 0, 0);
        }
        assert!(
            app.scrollback.len() <= MAX_SCROLLBACK + SCROLLBACK_SLACK,
            "scrollback must be capped, got {}",
            app.scrollback.len()
        );
        assert!(
            app.scrollback
                .last()
                .is_some_and(|l| l.text == format!("line {}", total - 1)),
            "the newest line must be retained"
        );
    }

    #[test]
    fn trim_forgets_running_tool_marker_when_its_line_is_dropped() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.start_tool_line("[Bash] big"); // row 0
        for i in 0..(MAX_SCROLLBACK + SCROLLBACK_SLACK + 200) {
            app.add_colored_line(format!("out {i}"), 0, 0);
        }
        // Row 0 was trimmed; finishing must not panic or mis-index a stale row.
        app.finish_tool_line(true);
    }

    #[test]
    fn finish_tool_line_without_running_is_noop() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.finish_tool_line(true);
        assert!(app.scrollback.is_empty(), "no tool line, nothing happens");
    }

    #[test]
    fn add_blank_line_suppresses_consecutive_blanks() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.add_colored_line("    line one".to_string(), 0, 0);
        app.add_blank_line();
        app.add_blank_line(); // second one is a no-op — no double gap
        assert_eq!(app.scrollback.len(), 2, "one content row + one blank only");
        // A whitespace-only tool-output row also counts as blank for dedup.
        app.add_colored_line("    ".to_string(), 0, 0);
        let before = app.scrollback.len();
        app.add_blank_line();
        assert_eq!(
            app.scrollback.len(),
            before,
            "blank after a whitespace-only row is suppressed"
        );
    }

    #[test]
    fn start_tool_line_separates_from_unterminated_stream_output() {
        // A streaming tool (e.g. Bash) may end without an explicit result
        // event, so the on-result separator never runs; the next header must
        // not sit flush against the previous block's last output row.
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.start_tool_line("[Bash] first");
        app.add_colored_line("    streamed output".to_string(), 0, 0);
        app.start_tool_line("[Read] second");
        let n = app.scrollback.len();
        assert!(app.scrollback[n - 1].text.contains("[Read] second"));
        assert!(
            app.scrollback[n - 2].text.trim().is_empty(),
            "exactly one separator row before the new header"
        );
        assert!(app.scrollback[n - 3].text.contains("streamed output"));
    }

    #[test]
    fn start_tool_line_does_not_double_separator() {
        // When the result path already appended the trailing separator, the
        // next header must not add a second blank on top of it.
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.start_tool_line("[Bash] first");
        app.add_colored_line("    out".to_string(), 0, 0);
        app.add_blank_line(); // on-result separator
        app.start_tool_line("[Read] second");
        let n = app.scrollback.len();
        assert!(app.scrollback[n - 1].text.contains("[Read] second"));
        assert!(app.scrollback[n - 2].text.trim().is_empty());
        assert!(
            app.scrollback[n - 3].text.contains("out"),
            "no double blank between blocks"
        );
    }

    #[test]
    fn first_tool_line_has_no_leading_blank() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.start_tool_line("[Bash] first");
        assert_eq!(app.scrollback.len(), 1, "no separator at the top of scrollback");
        assert!(app.scrollback[0].text.contains("[Bash] first"));
    }

    #[test]
    fn assistant_reply_separates_from_unterminated_stream_output() {
        // Same invariant for the assistant's reply: streamed tool output with
        // no result event must not end flush against the reply text.
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.start_tool_line("[Bash] first");
        app.add_colored_line("    streamed output".to_string(), 0, 0);
        app.start_assistant_turn();
        app.append_to_current_assistant("done");
        app.finalize_assistant_turn(0);
        let reply_row = app
            .scrollback
            .iter()
            .position(|l| l.text.contains("done"))
            .expect("reply rendered");
        assert!(
            app.scrollback[reply_row - 1].text.trim().is_empty(),
            "separator row between tool output and the reply"
        );
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
            indent: 0,
            right: false,
        };
        let tok = crate::tui::tokens::Tokens::default_tokens();
        render_scroll_line(&mut g, 0, &lit, 12, &tok);
        assert_eq!(g.get(0, 0).glyph, u32::from('*'), "literal line keeps leading *");

        let mut g2 = Grid::new(12, 1);
        let prose = VisualLine {
            text: "**x**",
            fg: theme::BODY,
            bg: 0,
            bold: false,
            literal: false,
            indent: 0,
            right: false,
        };
        render_scroll_line(&mut g2, 0, &prose, 12, &tok);
        assert_eq!(
            g2.get(0, 0).glyph,
            u32::from('x'),
            "prose line parses **bold**, dropping the markers"
        );
    }

    #[test]
    fn wrapped_continuation_lines_keep_leading_indent() {
        let mut lines = Vec::new();
        // 4-space indented tool output, width 12: continuation pieces must
        // hang under the content instead of snapping back to column 0.
        wrap_into("    abcdef ghij klmn", 0, 0, false, true, false, 12, &mut lines);
        assert!(
            lines.len() >= 2,
            "must wrap, got {:?}",
            lines.iter().map(|l| l.text).collect::<Vec<_>>()
        );
        assert_eq!(lines[0].text, "    abcdef");
        assert_eq!(lines[0].indent, 0, "first piece carries its own literal indent");
        assert!(
            lines[1..].iter().all(|l| l.indent == 4),
            "continuations hang at the content's indent, got {:?}",
            lines.iter().map(|l| l.indent).collect::<Vec<_>>()
        );
    }

    #[test]
    fn wrap_indent_disabled_on_very_narrow_terminal() {
        let mut lines = Vec::new();
        // Indent (4) would eat half of the 6-col width — fall back to col 0
        // so pathological narrow terminals never render slivers of text.
        wrap_into("    abcdefghij", 0, 0, false, true, false, 6, &mut lines);
        assert!(lines.len() >= 2);
        assert!(lines.iter().all(|l| l.indent == 0));
    }

    #[test]
    fn render_scroll_line_draws_at_indent() {
        let mut g = Grid::new(12, 1);
        let vl = VisualLine {
            text: "abc",
            fg: theme::BODY,
            bg: 0,
            bold: false,
            literal: true,
            indent: 4,
            right: false,
        };
        let tok = crate::tui::tokens::Tokens::default_tokens();
        render_scroll_line(&mut g, 0, &vl, 12, &tok);
        assert_eq!(
            g.get(0, 4).glyph,
            u32::from('a'),
            "content starts at the hang column"
        );
    }

    #[test]
    fn user_line_is_right_aligned_no_marker() {
        // The "you> " arm now pushes a right-aligned warm line carrying just the
        // body — no ❯ marker, no "you  " label.
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.add_line("you> ", "hello");
        let line = app
            .scrollback
            .iter()
            .find(|l| l.text == "hello")
            .expect("user body present verbatim");
        assert_eq!(line.align, LineAlign::Right, "user prompt aligns right");
        assert!(
            !line.text.contains('\u{276F}'),
            "the ❯ marker is dropped from the stored text"
        );
        assert!(
            !app.scrollback.iter().any(|l| l.text.starts_with("you  ")),
            "no inline `you  ` label is rendered"
        );
    }

    #[test]
    fn right_band_never_panics_on_any_width() {
        // Regression: `right_band` clamped with `min = cols.min(16)`, `max =
        // cols-4`, whose min > max for cols in 1..=19 — `u16::clamp` panics on
        // min > max, crashing the live render on a narrow resize once a user
        // prompt was in scrollback. Sweep every plausible width and assert it
        // returns a sane band (and, implicitly, never panics).
        for cols in 0u16..=200 {
            let band = right_band(cols);
            assert!(band <= cols, "band {band} must not exceed cols {cols}");
        }
    }

    // Index of the rightmost non-blank cell on `row`, or None if the row is blank.
    fn rightmost_glyph(g: &Grid, row: u16, cols: u16) -> Option<u16> {
        let mut last = None;
        for c in 0..cols {
            let gl = g.get(row, c).glyph;
            if gl != 0 && gl != u32::from(' ') {
                last = Some(c);
            }
        }
        last
    }

    #[test]
    fn right_aligned_user_line_hugs_right_edge_with_rule() {
        let cols: u16 = 40;
        let mut g = Grid::new(cols, 1);
        let tok = crate::tui::tokens::Tokens::default_tokens();
        let vl = VisualLine {
            text: "hi there",
            fg: tok.you,
            bg: 0,
            bold: true,
            literal: false,
            indent: 0,
            right: true,
        };
        render_scroll_line(&mut g, 0, &vl, cols, &tok);

        // The right rule sits at the far-right column, in the user tone.
        let rule = g.get(0, cols - 1);
        assert_eq!(
            rule.glyph,
            u32::from(crate::tui::tokens::glyph::SPINE),
            "right rule at col cols-1"
        );
        assert_eq!(rule.fg, tok.you, "right rule painted in the user tone");

        // Col 0 must NOT be a left spine on a right row.
        let g0 = g.get(0, 0).glyph;
        assert!(
            g0 == 0 || g0 == u32::from(' '),
            "no left spine on a right-aligned row, got {g0}"
        );

        // The text glyphs sit in the right portion of the grid (past the midpoint),
        // ending just before the 1-col gap and the rule.
        let w = char_display_width("hi there");
        let content_right = cols - 2; // exclusive content edge
        let start = content_right - w; // first glyph column
        assert!(start > cols / 2, "text starts in the right half (start={start})");
        assert_eq!(
            g.get(0, start).glyph,
            u32::from('h'),
            "first glyph at computed start"
        );
        assert_eq!(
            rightmost_glyph(&g, 0, cols - 1),
            Some(content_right - 1),
            "last text glyph hugs the content-right edge (before the gap+rule)"
        );
    }

    #[test]
    fn wrapped_user_message_right_aligns_on_every_row() {
        // A long body, narrow band: each produced row must independently hug the
        // right edge (not just the first), with a right rule on each non-blank row.
        let cols: u16 = 24;
        let band = right_band(cols);
        let body = "alpha beta gamma delta epsilon zeta";
        let mut lines = Vec::new();
        wrap_into(body, 0, 0, true, false, true, band as usize, &mut lines);
        assert!(
            lines.len() >= 2,
            "must wrap into multiple rows, got {}",
            lines.len()
        );
        assert!(
            lines.iter().all(|l| l.right && l.indent == 0),
            "every wrapped piece is right-aligned with indent 0"
        );

        let tok = crate::tui::tokens::Tokens::default_tokens();
        for (i, vl) in lines.iter().enumerate() {
            let mut g = Grid::new(cols, 1);
            render_scroll_line(&mut g, 0, vl, cols, &tok);
            let content_right = cols - 2;
            let w = char_display_width(vl.text);
            let start = (content_right.saturating_sub(w)).max(1);
            // The last glyph of this row hugs the content-right edge.
            assert_eq!(
                rightmost_glyph(&g, 0, cols - 1),
                Some(content_right - 1),
                "row {i} ({:?}) does not hug the right edge",
                vl.text
            );
            // First glyph lands at the computed right-aligned start.
            let first = vl.text.chars().next().unwrap();
            assert_eq!(
                g.get(0, start).glyph,
                u32::from(first),
                "row {i} first glyph at the right-aligned start"
            );
            // Right rule present, no left spine.
            assert_eq!(
                g.get(0, cols - 1).glyph,
                u32::from(crate::tui::tokens::glyph::SPINE),
                "row {i} has the right rule"
            );
            let g0 = g.get(0, 0).glyph;
            assert!(g0 == 0 || g0 == u32::from(' '), "row {i} has no left spine");
        }
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
    fn localize_phase_routes_thinking_through_catalog_byte_identical() {
        // The "Thinking" label routes through the `thinking` catalog key; in
        // English it is byte-identical ("Thinking", no ellipsis), and it must
        // equal what the catalog resolves so a `--lang` override localizes it.
        let thinking = localize_phase(Some("Thinking")).expect("thinking yields a label");
        assert_eq!(thinking, crate::locale::line("thinking"));
        assert_eq!(origin_i18n::t(origin_i18n::Lang::En, "thinking"), "Thinking");
        // "Responding" has no catalog key and passes through unchanged.
        assert_eq!(localize_phase(Some("Responding")).as_deref(), Some("Responding"));
        // Idle yields nothing.
        assert_eq!(localize_phase(None), None);
    }

    // The legibility hierarchy of the status readout (workflow/phase/model/
    // tokens/cost/ctx coloring) now lives in `chrome::draw_status` and is tested
    // there; the status-line span builder was retired with the input-card status
    // line in the TUI rework (INT-2). The ctx-meter percentage is still computed
    // by `App::ctx_pct` (consumed by the chrome strip), so it keeps its test.
    #[test]
    fn ctx_meter_tracks_turn_input() {
        let mut app = App::new("anthropic", "claude-sonnet-4-6", CompletionSources::default());
        assert_eq!(app.ctx_pct(), None, "no turn yet");
        app.start_turn_timer();
        app.record_usage_tokens(150_000, 10, 0, 0); // ~150k of a ~200k window
        app.stop_turn_timer();
        let pct = app.ctx_pct().expect("a turn ran");
        assert!((70..=80).contains(&pct), "≈75% of 200k, got {pct}");
    }

    #[test]
    fn ctx_meter_uses_real_one_million_window_for_opus_4_8() {
        // Opus 4.8 has a 1M window: 150k tokens must read as ~15%, NOT the ~75%
        // the old crude 200K heuristic produced. This exercises the shared
        // `origin_daemon::model_window::model_context_window` resolver via
        // `App::ctx_pct`.
        let mut app = App::new("anthropic", "claude-opus-4-8", CompletionSources::default());
        app.start_turn_timer();
        app.record_usage_tokens(150_000, 10, 0, 0); // ~150k of a 1M window
        app.stop_turn_timer();
        let pct = app.ctx_pct().expect("a turn ran");
        assert!((13..=17).contains(&pct), "≈15% of 1M, got {pct}");
    }

    #[test]
    fn wide_glyph_writes_continuation_cell_without_drift() {
        // '世' (U+4E16) is double-width: the cell after it must be a continuation
        // marker, and the following 'x' must land at column +2 (no drift).
        let mut g = Grid::new(8, 1);
        write_str_styled(
            &mut g,
            0,
            0,
            "\u{4e16}x",
            8,
            Style {
                fg: theme::BODY,
                bg: 0,
                bold: false,
            },
        );
        assert_eq!(g.get(0, 0).glyph, u32::from('\u{4e16}'), "wide glyph at col 0");
        assert!(g.get(0, 1).is_continuation(), "col 1 is a continuation cell");
        assert_eq!(
            g.get(0, 2).glyph,
            u32::from('x'),
            "next char at col 2, not drifted"
        );
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
    fn wrap_segment_breaks_at_word_boundary() {
        assert_eq!(
            wrap_segment_hanging("hello world", 20, 20),
            vec!["hello world"],
            "fits on one line"
        );
        assert_eq!(
            wrap_segment_hanging("hello world", 7, 7),
            vec!["hello", "world"],
            "break between words"
        );
        assert_eq!(
            wrap_segment_hanging("a b c d e", 5, 5),
            vec!["a b c", "d e"],
            "pack greedily"
        );
    }

    #[test]
    fn wrap_segment_hard_breaks_overlong_word() {
        assert_eq!(
            wrap_segment_hanging("abcdefghij", 4, 4),
            vec!["abcd", "efgh", "ij"],
            "no spaces → hard break"
        );
    }

    #[test]
    fn wrap_segment_drops_space_run_at_break() {
        assert_eq!(
            wrap_segment_hanging("foo    bar", 4, 4),
            vec!["foo", "bar"],
            "the whole space run is dropped, no trailing space"
        );
    }

    #[test]
    fn stall_tier_shows_soft_reassurance_only() {
        let soft = Duration::from_secs(11);
        assert_eq!(
            stall_tier(Duration::from_secs(5), soft),
            None,
            "below soft: nothing"
        );
        assert_eq!(
            stall_tier(Duration::from_secs(11), soft),
            Some(StallTier::Soft(11)),
            "at soft threshold"
        );
        // No hard/alarm tier: even sustained silence stays a soft reassurance.
        assert_eq!(
            stall_tier(Duration::from_secs(90), soft),
            Some(StallTier::Soft(90)),
            "long quiet still reads as 'still working', never an alarm"
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
        app.stall = Some(StallTier::Soft(90));
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
        app.stall = Some(StallTier::Soft(42));
        app.scroll_offset = 7;

        app.reset_to_login(80, 24);

        // The banner is re-pushed, so scrollback is non-empty but contains
        // only freshly-painted launch rows — none of the conversation lines.
        assert!(
            !app.scrollback.is_empty(),
            "reset must re-paint the startup banner"
        );
        assert!(
            !app.scrollback
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
        assert!(app.active_picker.is_none(), "no picker open by default");
        assert_eq!(app.palette(), theme::palette(Theme::Default));
    }

    // ── INT-3: picker wiring (permission + choice) ──────────────────────────

    #[test]
    fn picker_outcome_to_permission_maps_each_option() {
        // 0 = Allow once, 1 = Deny, 2 = Always allow.
        assert_eq!(picker_outcome_to_permission(0), (true, false), "Allow once");
        assert_eq!(picker_outcome_to_permission(1), (false, false), "Deny");
        assert_eq!(picker_outcome_to_permission(2), (true, true), "Always allow");
        // Any unexpected index is a safe deny.
        assert_eq!(
            picker_outcome_to_permission(7),
            (false, false),
            "out-of-range denies"
        );
        // Esc-as-deny.
        assert_eq!(permission_cancel(), (false, false), "cancel is a plain deny");
    }

    #[test]
    fn open_permission_picker_builds_three_single_select_options() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.open_permission_picker(42, "Bash", "rm -rf build/");
        let sess = app.active_picker.as_ref().expect("picker opened");
        assert!(matches!(sess.source, PickerSource::Permission { id: 42, .. }));
        assert!(!sess.state.multi, "permission picker is single-select");
        assert!(!sess.state.allow_custom, "permission picker has no custom row");
        assert_eq!(sess.state.options.len(), 3);
        assert_eq!(sess.state.options[0].label, "Allow once");
        assert_eq!(sess.state.options[1].label, "Deny");
        assert_eq!(sess.state.options[2].label, "Always allow Bash");
    }

    #[test]
    fn picker_outcome_to_choice_maps_selected_and_cancel() {
        // Multi-select with custom text.
        let out = picker::PickerOutcome::Selected {
            indices: vec![0, 2],
            custom: Some("other".to_string()),
        };
        let (sel, custom) = picker_outcome_to_choice(&out);
        assert_eq!(sel, vec![0, 2]);
        assert_eq!(custom.as_deref(), Some("other"));
        // Cancel ⇒ the daemon's "user skipped" signal (empty + no custom).
        let (sel, custom) = picker_outcome_to_choice(&picker::PickerOutcome::Cancelled);
        assert!(sel.is_empty());
        assert!(custom.is_none());
    }

    #[test]
    fn open_choice_picker_maps_options_and_sizes_checked() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.open_choice_picker(
            "tool-7".to_string(),
            "Which target?".to_string(),
            vec![
                ("staging".to_string(), Some("safe".to_string())),
                ("prod".to_string(), None),
            ],
            true,
            true,
        );
        let sess = app.active_picker.as_ref().expect("picker opened");
        assert!(matches!(&sess.source, PickerSource::Choice { id } if id == "tool-7"));
        assert!(sess.state.multi);
        assert!(sess.state.allow_custom);
        assert_eq!(sess.state.options.len(), 2);
        assert_eq!(sess.state.options[0].description.as_deref(), Some("safe"));
        assert_eq!(sess.state.checked, vec![false, false], "checked sized all-false");
    }

    #[test]
    fn take_picker_clears_active() {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.open_permission_picker(1, "Write", "a.txt");
        assert!(app.has_picker());
        let taken = app.take_picker();
        assert!(taken.is_some());
        assert!(!app.has_picker(), "take_picker leaves None");
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
            let mut widget = StreamWidget::new(Rect {
                row: 0,
                col: 0,
                cols: 60,
                rows: 6,
            });
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
            let mut widget = StreamWidget::new(Rect {
                row: 0,
                col: 0,
                cols: 60,
                rows: 6,
            });
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
        let mut widget = StreamWidget::new(Rect {
            row: 0,
            col: 0,
            cols: 60,
            rows: 6,
        });
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
        app.input.set_buffer("hello".to_string()); // editor caret at end (char 5)
                                                   // Drive the editor caret to char 2 so the vim layer (which seeds from the
                                                   // editor caret) starts there.
        app.input.move_left();
        app.input.move_left();
        app.input.move_left(); // 5 -> 2
        app.vim_mode = VimMode::Normal;
        // h moves left: both the App scratch cursor and the editor caret track it.
        assert!(app.apply_vim_action(crate::input::VimAction::MoveLeft));
        assert_eq!(app.cursor, 1);
        assert_eq!(app.input.cursor_chars(), 1, "editor caret follows the motion");
        // $ jumps to end (char count).
        assert!(app.apply_vim_action(crate::input::VimAction::LineEnd));
        assert_eq!(app.cursor, 5);
        assert_eq!(app.input.cursor_chars(), 5);
        // i switches to Insert and is consumed.
        assert!(app.apply_vim_action(crate::input::VimAction::SwitchMode(VimMode::Insert)));
        assert_eq!(app.vim_mode, VimMode::Insert);
        // Pass is not consumed.
        assert!(!app.apply_vim_action(crate::input::VimAction::Pass));
    }

    #[test]
    fn set_vim_active_seeds_mode() {
        // The startup seed point: active ⇒ Normal mode (vim convention),
        // inactive ⇒ Insert (byte-identical direct insert).
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.set_vim_active(true);
        assert!(app.vim_active());
        assert_eq!(app.vim_mode(), VimMode::Normal);
        app.set_vim_active(false);
        assert!(!app.vim_active());
        assert_eq!(app.vim_mode(), VimMode::Insert);
    }

    #[test]
    fn set_keymap_installs_override() {
        // The session keymap defaults to builtin and can be replaced once at
        // startup with a user override.
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        assert!(!app.keymap().is_overridden(), "defaults to builtin");
        let km = crate::keybindings::KeyMap::from_toml_str("history-prev = \"ctrl+p\"").expect("parse");
        app.set_keymap(km);
        assert!(app.keymap().is_overridden());
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

    // ── Fix #2: unclosed markdown markers ───────────────────────────────────

    /// Read a grid row's glyphs into a `String` (trailing blanks trimmed).
    fn row_text(grid: &Grid, row: u16) -> String {
        (0..grid.cols())
            .map(|c| char::from_u32(grid.get(row, c).glyph).unwrap_or(' '))
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    // Inline-markdown rendering now lives in `markdown::render_inline` (the
    // legacy `render_md_line` was retired in the TUI rework, INT-2). These
    // regression guards keep the streaming-friendly unclosed-marker behavior
    // pinned from the mod.rs side; deeper coverage lives in markdown.rs's tests.
    #[test]
    fn unclosed_bold_marker_is_hidden_and_styles_to_end_of_line() {
        // A `**bold` that never closes on this visual line (it wrapped / is still
        // streaming) must NOT leak literal `*` glyphs; the remainder is bolded.
        let tok = crate::tui::tokens::Tokens::default_tokens();
        let mut grid = Grid::new(20, 1);
        let style = Style {
            fg: tok.body,
            bg: 0,
            bold: false,
        };
        markdown::render_inline(&mut grid, 0, "**bold", 20, style, &tok, 0);
        assert_eq!(row_text(&grid, 0), "bold", "marker hidden, text kept");
        assert!(
            (0..grid.cols()).all(|c| grid.get(0, c).glyph != u32::from(b'*')),
            "no literal '*' should be rendered",
        );
        // The visible glyphs carry the BOLD attribute.
        assert_eq!(grid.get(0, 0).attr & Attr::BOLD.bits(), Attr::BOLD.bits());
    }

    #[test]
    fn unclosed_code_marker_is_hidden() {
        let tok = crate::tui::tokens::Tokens::default_tokens();
        let mut grid = Grid::new(20, 1);
        let style = Style {
            fg: tok.body,
            bg: 0,
            bold: false,
        };
        markdown::render_inline(&mut grid, 0, "`code", 20, style, &tok, 0);
        assert_eq!(row_text(&grid, 0), "code");
        assert!(
            (0..grid.cols()).all(|c| grid.get(0, c).glyph != u32::from(b'`')),
            "no literal backtick should be rendered",
        );
    }

    #[test]
    fn closed_bold_still_renders_without_markers() {
        // Regression guard: the normal closed case is unchanged.
        let tok = crate::tui::tokens::Tokens::default_tokens();
        let mut grid = Grid::new(20, 1);
        let style = Style {
            fg: tok.body,
            bg: 0,
            bold: false,
        };
        markdown::render_inline(&mut grid, 0, "a **b** c", 20, style, &tok, 0);
        assert_eq!(row_text(&grid, 0), "a b c");
    }

    // ── Feature: click-drag selection extraction ────────────────────────────

    fn app_with_screen(rows: &[&str]) -> App {
        let mut app = App::new("anthropic", "m", CompletionSources::default());
        app.screen_text = rows.iter().map(|s| (*s).to_string()).collect();
        app
    }

    #[test]
    fn selection_text_single_line_is_inclusive_of_both_ends() {
        let mut app = app_with_screen(&["hello world"]);
        app.begin_selection(0, 0);
        app.update_selection(0, 4); // covers cols 0..=4 → "hello"
        assert_eq!(app.selection_text().as_deref(), Some("hello"));
    }

    #[test]
    fn selection_text_spans_multiple_rows() {
        let mut app = app_with_screen(&["first line", "second line", "third line"]);
        app.begin_selection(0, 6); // "line" on row 0
        app.update_selection(2, 4); // "third" on row 2 (cols 0..=4)
        assert_eq!(app.selection_text().as_deref(), Some("line\nsecond line\nthird"),);
    }

    #[test]
    fn selection_text_normalizes_a_reversed_drag() {
        // Dragging up-and-left must yield the same text as down-and-right.
        let mut app = app_with_screen(&["abcdef"]);
        app.begin_selection(0, 5);
        app.update_selection(0, 2); // reversed: head before anchor
        assert_eq!(app.selection_text().as_deref(), Some("cdef"));
    }

    #[test]
    fn selection_text_trims_trailing_blanks_and_empty_lines() {
        let mut app = app_with_screen(&["text      ", "          "]);
        app.begin_selection(0, 0);
        app.update_selection(1, 9); // whole 2x10 block
        assert_eq!(
            app.selection_text().as_deref(),
            Some("text"),
            "trailing spaces and the all-blank second line are dropped",
        );
    }

    #[test]
    fn empty_or_unset_selection_yields_nothing() {
        let mut app = app_with_screen(&["abc"]);
        assert_eq!(app.selection_text(), None, "no selection → None");
        app.begin_selection(0, 1); // zero-width (no drag)
        assert_eq!(app.selection_text(), None, "zero-width selection → None");
        assert!(app.clear_selection());
        assert!(!app.clear_selection(), "second clear is a no-op");
    }

    #[test]
    fn selection_highlight_sets_reverse_on_covered_cells() {
        let mut grid = Grid::new(10, 2);
        for c in 0..10 {
            grid.put(0, c, Cell::glyph('x'));
            grid.put(1, c, Cell::glyph('y'));
        }
        let sel = Selection {
            anchor: (0, 2),
            head: (0, 5),
        };
        apply_selection_highlight(&mut grid, sel);
        for c in 0..10 {
            let reversed = grid.get(0, c).attr & Attr::REVERSE.bits() != 0;
            assert_eq!(
                reversed,
                (2..=5).contains(&c),
                "only cols 2..=5 on row 0 are reversed (col {c})",
            );
        }
        assert!(
            (0..10).all(|c| grid.get(1, c).attr & Attr::REVERSE.bits() == 0),
            "row 1 is untouched",
        );
    }

    // Regression: Windows `\r\n` line endings (the wrapper splits on `\n` but
    // leaves the `\r`) must never reach a cell. A `\r` rendered into a cell and
    // emitted raw returns the terminal cursor to column 0 mid-row, corrupting the
    // line and permanently desyncing the damage-diff shadow grid — the real cause
    // of the stale-fragment corruption.
    #[test]
    fn control_chars_are_zero_width_and_never_rendered() {
        assert_eq!(char_cell_width('\r'), 0, "carriage return must be zero-width");
        assert_eq!(char_cell_width('\t'), 0);
        assert_eq!(char_cell_width('\u{1b}'), 0, "ESC must be zero-width");
        assert_eq!(char_cell_width('a'), 1);

        // A line with an embedded `\r` renders as if it were absent — no control
        // glyph reaches the grid, and following text is not pushed off.
        let mut grid = Grid::new(10, 1);
        write_str_styled(
            &mut grid,
            0,
            0,
            "ab\rcd",
            10,
            Style {
                fg: 0,
                bg: 0,
                bold: false,
            },
        );
        let row: String = (0..4)
            .map(|c| char::from_u32(grid.get(0, c).glyph).unwrap_or('?'))
            .collect();
        assert_eq!(row, "abcd", "the `\\r` must be dropped, not rendered into a cell");
        assert!(
            (0..10).all(|c| grid.get(0, c).glyph != u32::from(b'\r')),
            "no cell may hold a carriage-return glyph",
        );
    }
}
