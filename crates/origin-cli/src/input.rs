// SPDX-License-Identifier: Apache-2.0
//! Input event handling (key reducer).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use origin_daemon::protocol::{ClientMessage, MemoryAction};

use crate::editor::Editor;

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, PartialEq, Eq)]
pub enum InputAction {
    Insert(char),
    Newline,
    Backspace,
    Submit(String),
    Quit,
    /// User cancelled an in-flight operation (Ctrl+C while a goal is
    /// active or a prompt is mid-stream). The CLI sends
    /// [`origin_daemon::protocol::ClientMessage::Interrupt`] to the
    /// daemon and stays running — distinct from `Quit` which exits the
    /// process. See bug #5.
    Interrupt,
    Noop,
}

/// Reduce a key event against the input buffer.
///
/// `op_in_flight` is the CLI's view of "is there something to interrupt":
/// `true` while a goal is active or a prompt is mid-stream. Ctrl+C is
/// remapped to [`InputAction::Interrupt`] when `op_in_flight` is `true`
/// and falls back to [`InputAction::Quit`] otherwise. Ctrl+D and Esc
/// remain quit-only — that gives the user an unambiguous exit even
/// during a goal (bug #5).
#[must_use]
pub fn reduce(buffer: &mut String, ev: KeyEvent, op_in_flight: bool) -> InputAction {
    match (ev.code, ev.modifiers) {
        (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
            if op_in_flight {
                InputAction::Interrupt
            } else {
                InputAction::Quit
            }
        }
        // Ctrl+D always quits, regardless of operation state — gives the
        // user a deterministic exit affordance even when Ctrl+C is rebound
        // to Interrupt.
        (KeyCode::Char('d'), m) if m.contains(KeyModifiers::CONTROL) => InputAction::Quit,
        (KeyCode::Esc, _) => InputAction::Quit,
        (KeyCode::Enter, m) if m.contains(KeyModifiers::SHIFT) => {
            buffer.push('\n');
            InputAction::Newline
        }
        (KeyCode::Enter, _) => {
            if buffer.is_empty() {
                InputAction::Noop
            } else {
                let out = std::mem::take(buffer);
                InputAction::Submit(out)
            }
        }
        (KeyCode::Backspace, _) => {
            let popped = buffer.pop();
            if popped.is_some() {
                InputAction::Backspace
            } else {
                InputAction::Noop
            }
        }
        (KeyCode::Char(c), _) => {
            buffer.push(c);
            InputAction::Insert(c)
        }
        _ => InputAction::Noop,
    }
}

/// Reduce a key event against an [`Editor`] — cursor-aware editing.
///
/// Adds Home/End/Delete, arrow navigation, and prompt-history recall (Up/Down
/// past the buffer edges). `op_in_flight` gates Ctrl+C (Interrupt vs Quit).
/// `width` is the input card's text width, for visual Home/End and Up/Down
/// across wrapped lines. Returns the same [`InputAction`]s as [`reduce`];
/// cursor-only moves return `Noop` (the caller still redraws to move the caret).
pub fn reduce_editor(editor: &mut Editor, ev: KeyEvent, op_in_flight: bool, width: usize) -> InputAction {
    match (ev.code, ev.modifiers) {
        (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
            if op_in_flight {
                InputAction::Interrupt
            } else {
                InputAction::Quit
            }
        }
        (KeyCode::Char('d'), m) if m.contains(KeyModifiers::CONTROL) => InputAction::Quit,
        (KeyCode::Esc, _) => InputAction::Quit,
        (KeyCode::Enter, m) if m.contains(KeyModifiers::SHIFT) => {
            editor.insert_newline();
            InputAction::Newline
        }
        (KeyCode::Enter, _) => {
            if editor.is_empty() {
                InputAction::Noop
            } else {
                let text = editor.buffer().to_string();
                editor.push_history(&text);
                editor.set_buffer(String::new());
                InputAction::Submit(text)
            }
        }
        (KeyCode::Backspace, _) => {
            editor.backspace();
            InputAction::Backspace
        }
        (KeyCode::Delete, _) => {
            editor.delete();
            InputAction::Backspace
        }
        (KeyCode::Left, _) => {
            editor.move_left();
            InputAction::Noop
        }
        (KeyCode::Right, _) => {
            editor.move_right();
            InputAction::Noop
        }
        (KeyCode::Home, _) => {
            editor.move_home(width);
            InputAction::Noop
        }
        (KeyCode::End, _) => {
            editor.move_end(width);
            InputAction::Noop
        }
        (KeyCode::Up, _) => {
            // Move up a visual line; at the top, recall older history.
            if !editor.move_up_visual(width) {
                editor.history_up();
            }
            InputAction::Noop
        }
        (KeyCode::Down, _) => {
            if !editor.move_down_visual(width) {
                editor.history_down();
            }
            InputAction::Noop
        }
        // Plain typed character (no Ctrl/Alt) inserts at the cursor.
        (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
            editor.insert_char(c);
            InputAction::Insert(c)
        }
        _ => InputAction::Noop,
    }
}

/// Parse a `/mem ...` slash command into a [`ClientMessage::MemoryDecision`].
///
/// Recognized forms (case-insensitive on the verb):
/// - `/mem accept <N>`
/// - `/mem reject <N>`
/// - `/mem edit <N> <body...>`
///
/// Returns `None` for any other input so the caller can fall back to sending
/// the raw text as a [`origin_daemon::protocol::PromptRequest`].
#[must_use]
pub fn parse_mem_command(line: &str) -> Option<ClientMessage> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix("/mem")?.trim_start();
    // Tokenize by whitespace runs (not a single whitespace char): `splitn` on
    // `char::is_whitespace` yields empty tokens when fields are separated by
    // more than one space, so e.g. `/mem accept  1` would parse an empty id and
    // silently fail. Split verb and id off the front, keeping the remainder
    // (with its internal spacing) as the edit body.
    let verb_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let verb = &rest[..verb_end];
    let after_verb = rest[verb_end..].trim_start();
    let id_end = after_verb.find(char::is_whitespace).unwrap_or(after_verb.len());
    let id_tok = &after_verb[..id_end];
    let body_rest = after_verb[id_end..].trim();
    let proposal_id: u32 = id_tok.parse().ok()?;
    match verb.to_ascii_lowercase().as_str() {
        "accept" => Some(ClientMessage::MemoryDecision {
            proposal_id,
            action: MemoryAction::Accept,
        }),
        "reject" => Some(ClientMessage::MemoryDecision {
            proposal_id,
            action: MemoryAction::Reject,
        }),
        "edit" => {
            if body_rest.is_empty() {
                return None;
            }
            Some(ClientMessage::MemoryDecision {
                proposal_id,
                action: MemoryAction::Edit {
                    body: body_rest.to_string(),
                    tags: Vec::new(),
                },
            })
        }
        _ => None,
    }
}

/// Slash verbs that already have dedicated handlers — they must not be
/// re-routed through the skill parser even though they start with `/`.
/// Update this list when a new slash verb is added.
const RESERVED_SLASH_VERBS: &[&str] = &["mem", "account", "help", "model", "clear"];

/// Parse `/<name>` (activate) and `/-<name>` (deactivate) into a
/// [`ClientMessage::ActivateSkill`] or [`ClientMessage::DeactivateSkill`].
///
/// Rules:
/// - Leading `/` required; the rest is the skill name (or `-<name>` for
///   deactivate). Names may contain `:` (namespaced skills like
///   `frontend-design:frontend-design`).
/// - Names must not contain whitespace — a slash with embedded spaces is
///   prompt text mentioning a path, not a skill invocation.
/// - Reserved slash verbs (`/mem`, `/account`, `/help`) and the workflow
///   shape (`{workflow:...}`) are rejected so callers can fall through to
///   their own handlers.
///
/// Returns `None` for any non-matching input.
#[must_use]
pub fn parse_skill_command(line: &str) -> Option<ClientMessage> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix('/')?;
    if rest.is_empty() {
        return None;
    }

    // Split into `name_token` and `args` on the first whitespace.
    let (name_token, args_str) = rest.find(char::is_whitespace).map_or((rest, ""), |idx| {
        let (n, a) = rest.split_at(idx);
        (n, a.trim_start())
    });
    if name_token.is_empty() {
        return None;
    }

    // Deactivate sigil: `-<name>`, no args allowed (would be ambiguous).
    if let Some(name) = name_token.strip_prefix('-') {
        if name.is_empty() || !args_str.is_empty() {
            return None;
        }
        if RESERVED_SLASH_VERBS.iter().any(|v| name == *v) {
            return None;
        }
        return Some(ClientMessage::DeactivateSkill {
            name: name.to_string(),
        });
    }

    // Activate form. Reserved-verb guard applies to the first `:`-segment.
    let first_segment = name_token.split(':').next().unwrap_or(name_token);
    if RESERVED_SLASH_VERBS.iter().any(|v| first_segment == *v) {
        return None;
    }
    let args = if args_str.is_empty() {
        None
    } else {
        Some(args_str.to_string())
    };
    Some(ClientMessage::ActivateSkill {
        name: name_token.to_string(),
        args,
    })
}

/// Parse the mechanical `/clear` command into [`ClientMessage::ClearAll`].
///
/// `/clear` is NOT a skill — it resets the in-session context directly. The
/// whole trimmed line must be exactly `/clear` (no args), so a prompt that
/// merely mentions `/clear` mid-sentence is left as chat text. Returns `None`
/// for anything else.
#[must_use]
pub fn parse_clear_command(line: &str) -> Option<ClientMessage> {
    if line.trim() == "/clear" {
        Some(ClientMessage::ClearAll)
    } else {
        None
    }
}

/// Parse `{workflow:<name>}` (the whole trimmed line) into a
/// [`ClientMessage::ActivateWorkflow`].
///
/// Surrounding whitespace is allowed; inline references mid-prompt are NOT
/// — the entire trimmed line must be the brace token, to keep the form
/// unambiguous with chat content that happens to mention braces.
///
/// Returns `None` for unrecognized input.
#[must_use]
pub fn parse_workflow_command(line: &str) -> Option<ClientMessage> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix('{')?.strip_suffix('}')?;
    let name = inner.strip_prefix("workflow:")?.trim();
    if name.is_empty() {
        return None;
    }
    if name.chars().any(char::is_whitespace) {
        return None;
    }
    Some(ClientMessage::ActivateWorkflow {
        name: name.to_string(),
    })
}

/// Parse a `/model <name>` slash command into the requested model name.
///
/// Recognized form:
/// - `/model <name>` — switch the TUI's active model to `<name>` for
///   subsequent prompts. Surrounding whitespace is tolerated; the name
///   itself must be a single token.
///
/// Returns `None` for any non-matching input (including `/model` with no
/// argument and `/model foo bar` with extra tokens) so the caller can
/// surface a usage hint.
#[must_use]
pub fn parse_model_command(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix("/model")?;
    // Require a word boundary so `/modelfoo` is not matched.
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let mut parts = rest.split_whitespace();
    let name = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    Some(name.to_string())
}

/// Modal input mode for the opt-in vim layer (aider L107 parity).
///
/// Default sessions never construct anything but [`VimMode::Insert`] and the
/// vim reducer is never consulted, so the composer's direct-insert behaviour is
/// byte-identical unless the user opts in (`/vim`, `ORIGIN_VIM=1`, or a config
/// flag).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VimMode {
    /// Direct text entry — every printable key inserts (today's behaviour).
    #[default]
    Insert,
    /// Command mode — `hjkl`/`0`/`$`/`w`/`b` move; `i`/`a`/`A`/`I` enter insert.
    Normal,
}

/// The effect a key has in vim mode, returned by the pure [`vim_key`] reducer.
///
/// The caller (`tui.rs`) owns the actual cursor/buffer mutation; this enum is
/// the deterministically-testable decision. [`VimAction::Pass`] means "not a
/// vim key — handle it the normal way" so unmapped keys fall through unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimAction {
    /// Switch to the given mode (no cursor change of its own).
    SwitchMode(VimMode),
    /// Move the cursor one cell left (`h`).
    MoveLeft,
    /// Move the cursor one cell right (`l`).
    MoveRight,
    /// Move to the previous line (`k`).
    MoveUp,
    /// Move to the next line (`j`).
    MoveDown,
    /// Jump to the start of the line (`0`).
    LineStart,
    /// Jump to the end of the line (`$`).
    LineEnd,
    /// Jump forward one word (`w`).
    WordForward,
    /// Jump back one word (`b`).
    WordBack,
    /// Enter insert mode at the cursor (`i`).
    InsertHere,
    /// Enter insert mode after the cursor (`a`).
    AppendAfter,
    /// Enter insert mode at the line start (`I`).
    InsertLineStart,
    /// Enter insert mode at the line end (`A`).
    AppendLineEnd,
    /// Begin a `:`-command (Normal mode only).
    BeginCommand,
    /// Not a vim binding in this mode — caller uses its normal handling.
    Pass,
}

/// Pure modal-key reducer for the opt-in vim input layer.
///
/// Given the current [`VimMode`] and a key event, returns the [`VimAction`] to
/// apply. The cursor/buffer mutation lives in the caller; this map is the
/// unit-tested core.
///
/// Semantics:
/// - In [`VimMode::Insert`], only `Esc` is special (→ Normal); every other key
///   is [`VimAction::Pass`] so insertion stays byte-identical to the legacy
///   reducer.
/// - In [`VimMode::Normal`], `hjkl`/`0`/`$`/`w`/`b` move, `i`/`a`/`A`/`I` enter
///   insert, `:` begins a command, and `Esc` re-asserts Normal. Unmapped keys
///   are [`VimAction::Pass`] (they do not insert text in Normal mode — the
///   caller drops them).
///
/// Map a keypress to a permission decision while an interactive permission ask
/// is pending: `y`/`Y` allows, `n`/`N` or `Esc` denies, anything else is not an
/// answer (`None`) and falls through to normal input handling.
#[must_use]
pub const fn permission_answer(code: KeyCode) -> Option<bool> {
    match code {
        KeyCode::Char('y' | 'Y') => Some(true),
        KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(false),
        _ => None,
    }
}

/// Modifier chords (anything with `CONTROL`/`ALT`) always [`VimAction::Pass`]
/// so the global `Ctrl+C`/`Ctrl+D` exits keep working in either mode.
#[must_use]
pub fn vim_key(mode: VimMode, ev: KeyEvent) -> VimAction {
    if ev.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        return VimAction::Pass;
    }
    if ev.code == KeyCode::Esc {
        return VimAction::SwitchMode(VimMode::Normal);
    }
    match mode {
        VimMode::Insert => VimAction::Pass,
        VimMode::Normal => vim_normal_key(ev.code),
    }
}

/// Normal-mode key table, split out to keep [`vim_key`] flat (no nested match
/// on the mode *and* the code in one function body).
const fn vim_normal_key(code: KeyCode) -> VimAction {
    match code {
        KeyCode::Char('h') | KeyCode::Left => VimAction::MoveLeft,
        KeyCode::Char('l') | KeyCode::Right => VimAction::MoveRight,
        KeyCode::Char('k') | KeyCode::Up => VimAction::MoveUp,
        KeyCode::Char('j') | KeyCode::Down => VimAction::MoveDown,
        KeyCode::Char('0') => VimAction::LineStart,
        KeyCode::Char('$') => VimAction::LineEnd,
        KeyCode::Char('w') => VimAction::WordForward,
        KeyCode::Char('b') => VimAction::WordBack,
        KeyCode::Char('i') => VimAction::SwitchMode(VimMode::Insert),
        KeyCode::Char('a') => VimAction::AppendAfter,
        KeyCode::Char('A') => VimAction::AppendLineEnd,
        KeyCode::Char('I') => VimAction::InsertLineStart,
        KeyCode::Char(':') => VimAction::BeginCommand,
        _ => VimAction::Pass,
    }
}

/// Whether the opt-in vim input layer should be active for this session.
///
/// True when `ORIGIN_VIM=1` (env opt-in) or `config_flag` is set (e.g. a
/// `config.toml` field threaded in by the caller). Default-off ⇒ the composer's
/// direct-insert behaviour is byte-identical.
#[must_use]
pub fn vim_enabled(config_flag: bool) -> bool {
    config_flag || std::env::var("ORIGIN_VIM").as_deref() == Ok("1")
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unreachable)] // panic! is the idiomatic mismatched-variant assertion in test code
mod tests {
    use super::*;

    const fn k(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn enter_submits_buffer() {
        let mut buf = "hello".to_string();
        assert_eq!(
            reduce(&mut buf, k(KeyCode::Enter), false),
            InputAction::Submit("hello".into())
        );
        assert!(buf.is_empty());
    }

    #[test]
    fn reduce_editor_inserts_and_submits() {
        let mut ed = Editor::new();
        assert_eq!(reduce_editor(&mut ed, k(KeyCode::Char('h')), false, 80), InputAction::Insert('h'));
        assert_eq!(reduce_editor(&mut ed, k(KeyCode::Char('i')), false, 80), InputAction::Insert('i'));
        assert_eq!(ed.buffer(), "hi");
        assert_eq!(
            reduce_editor(&mut ed, k(KeyCode::Enter), false, 80),
            InputAction::Submit("hi".to_string())
        );
        assert!(ed.is_empty(), "submit clears the buffer");
    }

    #[test]
    fn reduce_editor_inserts_at_the_cursor_not_the_end() {
        let mut ed = Editor::new();
        ed.set_buffer("ac".to_string()); // cursor at end
        reduce_editor(&mut ed, k(KeyCode::Left), false, 80); // now between a and c
        reduce_editor(&mut ed, k(KeyCode::Char('b')), false, 80);
        assert_eq!(ed.buffer(), "abc", "mid-buffer insert");
    }

    #[test]
    fn reduce_editor_up_recalls_history() {
        let mut ed = Editor::new();
        ed.set_buffer("first".to_string());
        reduce_editor(&mut ed, k(KeyCode::Enter), false, 80); // submit → pushed to history
        reduce_editor(&mut ed, k(KeyCode::Up), false, 80); // recall
        assert_eq!(ed.buffer(), "first", "Up past the top recalls the previous prompt");
    }

    #[test]
    fn permission_answer_maps_keys() {
        assert_eq!(permission_answer(KeyCode::Char('y')), Some(true));
        assert_eq!(permission_answer(KeyCode::Char('Y')), Some(true));
        assert_eq!(permission_answer(KeyCode::Char('n')), Some(false));
        assert_eq!(permission_answer(KeyCode::Char('N')), Some(false));
        assert_eq!(permission_answer(KeyCode::Esc), Some(false), "Esc denies");
        assert_eq!(permission_answer(KeyCode::Char('x')), None, "other keys not an answer");
        assert_eq!(permission_answer(KeyCode::Enter), None);
    }

    #[test]
    fn enter_on_empty_is_noop() {
        let mut buf = String::new();
        assert_eq!(reduce(&mut buf, k(KeyCode::Enter), false), InputAction::Noop);
    }

    #[test]
    fn typing_appends_to_buffer() {
        let mut buf = "h".to_string();
        assert_eq!(
            reduce(&mut buf, k(KeyCode::Char('i')), false),
            InputAction::Insert('i')
        );
        assert_eq!(buf, "hi");
    }

    #[test]
    fn backspace_pops() {
        let mut buf = "hi".to_string();
        assert_eq!(
            reduce(&mut buf, k(KeyCode::Backspace), false),
            InputAction::Backspace
        );
        assert_eq!(buf, "h");
    }

    #[test]
    fn ctrl_c_quits_when_no_operation_in_flight() {
        let mut buf = String::new();
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(reduce(&mut buf, ev, false), InputAction::Quit);
    }

    #[test]
    fn ctrl_c_interrupts_when_operation_in_flight() {
        // Bug #5: Ctrl+C must NOT quit the process when a goal is active or
        // a prompt is mid-stream. It must send Interrupt to the daemon so
        // the user can cancel the current operation without losing the
        // session.
        let mut buf = String::new();
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(reduce(&mut buf, ev, true), InputAction::Interrupt);
    }

    #[test]
    fn ctrl_d_always_quits_even_mid_operation() {
        // The exit affordance must remain reachable even when Ctrl+C is
        // rebound to Interrupt during an in-flight operation.
        let mut buf = String::new();
        let ev = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(reduce(&mut buf, ev, false), InputAction::Quit);
        assert_eq!(reduce(&mut buf, ev, true), InputAction::Quit);
    }

    #[test]
    fn esc_quits_even_mid_operation() {
        // Esc is the second deterministic exit path. We deliberately do not
        // remap it — Esc-to-cancel makes editor-history navigation
        // ambiguous; keep it as the quit hammer.
        let mut buf = String::new();
        assert_eq!(reduce(&mut buf, k(KeyCode::Esc), true), InputAction::Quit);
    }

    #[test]
    fn parse_mem_accept() {
        match parse_mem_command("/mem accept 3") {
            Some(ClientMessage::MemoryDecision { proposal_id, action }) => {
                assert_eq!(proposal_id, 3);
                assert!(matches!(action, MemoryAction::Accept));
            }
            other => panic!("expected accept, got {other:?}"),
        }
    }

    #[test]
    fn parse_mem_reject() {
        match parse_mem_command("/mem reject 7") {
            Some(ClientMessage::MemoryDecision { proposal_id, action }) => {
                assert_eq!(proposal_id, 7);
                assert!(matches!(action, MemoryAction::Reject));
            }
            other => panic!("expected reject, got {other:?}"),
        }
    }

    #[test]
    fn parse_mem_edit_with_body() {
        match parse_mem_command("/mem edit 12 user likes terse output") {
            Some(ClientMessage::MemoryDecision { proposal_id, action }) => {
                assert_eq!(proposal_id, 12);
                match action {
                    MemoryAction::Edit { body, tags } => {
                        assert_eq!(body, "user likes terse output");
                        assert!(tags.is_empty());
                    }
                    other => panic!("expected Edit, got {other:?}"),
                }
            }
            other => panic!("expected edit decision, got {other:?}"),
        }
    }

    #[test]
    fn parse_mem_unknown_returns_none() {
        assert!(parse_mem_command("hello world").is_none());
        assert!(parse_mem_command("/mem").is_none());
        assert!(parse_mem_command("/mem accept abc").is_none());
        assert!(parse_mem_command("/mem edit 1").is_none()); // missing body
    }

    #[test]
    fn parse_skill_bare_name() {
        let m = parse_skill_command("/frontend-design").expect("parse");
        match m {
            ClientMessage::ActivateSkill { name, args } => {
                assert_eq!(name, "frontend-design");
                assert!(args.is_none());
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn parse_skill_namespaced() {
        let m = parse_skill_command("/frontend-design:frontend-design").expect("parse");
        match m {
            ClientMessage::ActivateSkill { name, args } => {
                assert_eq!(name, "frontend-design:frontend-design");
                assert!(args.is_none());
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn parse_skill_deactivate_with_dash_prefix() {
        let m = parse_skill_command("/-frontend-design").expect("parse");
        match m {
            ClientMessage::DeactivateSkill { name } => assert_eq!(name, "frontend-design"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn parse_skill_deactivate_namespaced() {
        let m = parse_skill_command("/-frontend-design:frontend-design").expect("parse");
        match m {
            ClientMessage::DeactivateSkill { name } => {
                assert_eq!(name, "frontend-design:frontend-design");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn parse_skill_rejects_empty_and_whitespace() {
        // Bare slash with no name, dash with no name, slash with trailing text.
        assert!(parse_skill_command("/").is_none());
        assert!(parse_skill_command("/-").is_none());
        assert!(parse_skill_command("/ ").is_none());
        // Deactivate-with-args is ambiguous and must be rejected.
        assert!(parse_skill_command("/-foo bar").is_none());
    }

    #[test]
    fn parse_skill_does_not_shadow_known_verbs() {
        // `/mem accept 1`, `/account default`, and `/workflow X` are not skills.
        assert!(parse_skill_command("/mem").is_none());
        assert!(parse_skill_command("/account").is_none());
        // Free-form text never parses as a skill.
        assert!(parse_skill_command("hello").is_none());
        // `{workflow:foo}` is a workflow, not a skill.
        assert!(parse_skill_command("{workflow:foo}").is_none());
    }

    #[test]
    fn parse_workflow_command_basic() {
        let m = parse_workflow_command("{workflow:frontend-design}").expect("parse");
        match m {
            ClientMessage::ActivateWorkflow { name } => assert_eq!(name, "frontend-design"),
            other => panic!("wrong: {other:?}"),
        }
    }

    #[test]
    fn parse_workflow_command_tolerates_surrounding_whitespace() {
        let m = parse_workflow_command("  {workflow:frontend-design}  ").expect("parse");
        match m {
            ClientMessage::ActivateWorkflow { name } => assert_eq!(name, "frontend-design"),
            other => panic!("wrong: {other:?}"),
        }
    }

    #[test]
    fn parse_workflow_command_rejects_malformed() {
        assert!(parse_workflow_command("{workflow:}").is_none());
        assert!(parse_workflow_command("{workflow}").is_none());
        assert!(parse_workflow_command("{wf:foo}").is_none());
        // Inline references mid-prompt are explicitly out of scope.
        assert!(parse_workflow_command("please run {workflow:x}").is_none());
        assert!(parse_workflow_command("/foo").is_none());
    }

    #[test]
    fn parse_model_basic() {
        let name = parse_model_command("/model claude-opus-4-7").expect("parse");
        assert_eq!(name, "claude-opus-4-7");
    }

    #[test]
    fn parse_model_tolerates_surrounding_whitespace() {
        let name = parse_model_command("   /model   claude-sonnet-4-6   ").expect("parse");
        assert_eq!(name, "claude-sonnet-4-6");
    }

    #[test]
    fn parse_model_rejects_no_argument() {
        assert!(parse_model_command("/model").is_none());
        assert!(parse_model_command("/model    ").is_none());
    }

    #[test]
    fn parse_model_rejects_multiple_args() {
        // Model names are a single token; extra args is a usage error,
        // surfaced as None so the caller can show the usage hint.
        assert!(parse_model_command("/model foo bar").is_none());
    }

    #[test]
    fn parse_model_requires_word_boundary() {
        // `/modelfoo` is not `/model foo` — must not be treated as a model
        // command. (The skill parser will pick it up instead.)
        assert!(parse_model_command("/modelfoo").is_none());
    }

    #[test]
    fn parse_skill_does_not_shadow_model() {
        // After registering "model" as reserved, the skill parser must
        // refuse `/model` so /model handling owns the verb.
        assert!(parse_skill_command("/model").is_none());
        assert!(parse_skill_command("/model:foo").is_none());
    }

    // ---- vim input layer (aider L107) ----

    #[test]
    fn vim_default_mode_is_insert() {
        assert_eq!(VimMode::default(), VimMode::Insert);
    }

    #[test]
    fn vim_insert_passes_through_printable_keys() {
        // In Insert mode every printable key is Pass, so the caller's normal
        // direct-insert path runs unchanged (byte-identical default).
        assert_eq!(vim_key(VimMode::Insert, k(KeyCode::Char('x'))), VimAction::Pass);
        assert_eq!(vim_key(VimMode::Insert, k(KeyCode::Char('h'))), VimAction::Pass);
        assert_eq!(vim_key(VimMode::Insert, k(KeyCode::Enter)), VimAction::Pass);
    }

    #[test]
    fn vim_esc_enters_normal_from_insert() {
        assert_eq!(
            vim_key(VimMode::Insert, k(KeyCode::Esc)),
            VimAction::SwitchMode(VimMode::Normal)
        );
    }

    #[test]
    fn vim_normal_hjkl_moves() {
        assert_eq!(vim_key(VimMode::Normal, k(KeyCode::Char('h'))), VimAction::MoveLeft);
        assert_eq!(vim_key(VimMode::Normal, k(KeyCode::Char('j'))), VimAction::MoveDown);
        assert_eq!(vim_key(VimMode::Normal, k(KeyCode::Char('k'))), VimAction::MoveUp);
        assert_eq!(vim_key(VimMode::Normal, k(KeyCode::Char('l'))), VimAction::MoveRight);
    }

    #[test]
    fn vim_normal_line_and_word_motions() {
        assert_eq!(vim_key(VimMode::Normal, k(KeyCode::Char('0'))), VimAction::LineStart);
        assert_eq!(vim_key(VimMode::Normal, k(KeyCode::Char('$'))), VimAction::LineEnd);
        assert_eq!(
            vim_key(VimMode::Normal, k(KeyCode::Char('w'))),
            VimAction::WordForward
        );
        assert_eq!(vim_key(VimMode::Normal, k(KeyCode::Char('b'))), VimAction::WordBack);
    }

    #[test]
    fn vim_normal_insert_entries() {
        assert_eq!(
            vim_key(VimMode::Normal, k(KeyCode::Char('i'))),
            VimAction::SwitchMode(VimMode::Insert)
        );
        assert_eq!(
            vim_key(VimMode::Normal, k(KeyCode::Char('a'))),
            VimAction::AppendAfter
        );
        assert_eq!(
            vim_key(VimMode::Normal, k(KeyCode::Char('A'))),
            VimAction::AppendLineEnd
        );
        assert_eq!(
            vim_key(VimMode::Normal, k(KeyCode::Char('I'))),
            VimAction::InsertLineStart
        );
    }

    #[test]
    fn vim_normal_colon_begins_command() {
        assert_eq!(
            vim_key(VimMode::Normal, k(KeyCode::Char(':'))),
            VimAction::BeginCommand
        );
    }

    #[test]
    fn vim_normal_unmapped_key_passes() {
        // An unmapped printable key in Normal mode does NOT insert — it passes
        // so the caller drops it (vim Normal mode never types text).
        assert_eq!(vim_key(VimMode::Normal, k(KeyCode::Char('z'))), VimAction::Pass);
    }

    #[test]
    fn vim_ctrl_chords_always_pass() {
        // Ctrl+C / Ctrl+D must keep reaching the global exit reducer in either
        // mode, so modifier chords are never captured by the vim layer.
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(vim_key(VimMode::Normal, ctrl_c), VimAction::Pass);
        assert_eq!(vim_key(VimMode::Insert, ctrl_c), VimAction::Pass);
    }

    #[test]
    fn vim_enabled_off_by_default() {
        // With no config flag and ORIGIN_VIM unset/!="1", the layer is off.
        // (We only assert the config-flag arm here to avoid mutating process
        // env in a shared test binary; the env arm is exercised via the OR.)
        assert!(!vim_enabled(false) || std::env::var("ORIGIN_VIM").as_deref() == Ok("1"));
        assert!(vim_enabled(true));
    }
}

#[cfg(test)]
mod tests_args {
    use super::*;
    use origin_daemon::protocol::ClientMessage;

    #[test]
    fn slash_with_args_returns_args_field() {
        let got = parse_skill_command("/goal fix the failing tests");
        assert!(matches!(
            got,
            Some(ClientMessage::ActivateSkill { ref name, args: Some(ref a) })
                if name == "goal" && a == "fix the failing tests"
        ));
    }

    #[test]
    fn slash_without_args_returns_none_args() {
        let got = parse_skill_command("/brainstorming");
        assert!(matches!(
            got,
            Some(ClientMessage::ActivateSkill { ref name, args: None })
                if name == "brainstorming"
        ));
    }

    #[test]
    fn clear_is_not_a_skill() {
        // `/clear` is a reserved verb; it must NOT parse as a skill activation.
        assert!(
            parse_skill_command("/clear").is_none(),
            "/clear must route to the mechanical ClearAll path, not the skill parser"
        );
    }

    #[test]
    fn clear_parses_to_clear_all() {
        assert!(matches!(parse_clear_command("/clear"), Some(ClientMessage::ClearAll)));
        assert!(matches!(parse_clear_command("  /clear  "), Some(ClientMessage::ClearAll)));
    }

    #[test]
    fn clear_command_rejects_args_and_chat() {
        assert!(parse_clear_command("/clear now").is_none());
        assert!(parse_clear_command("please /clear the screen").is_none());
        assert!(parse_clear_command("/cleary").is_none());
        assert!(parse_clear_command("clear").is_none());
    }

    #[test]
    fn deactivate_form_unaffected() {
        let got = parse_skill_command("/-goal");
        assert!(matches!(
            got,
            Some(ClientMessage::DeactivateSkill { ref name }) if name == "goal"
        ));
    }

    #[test]
    fn reserved_verb_still_rejected_with_args() {
        assert!(parse_skill_command("/mem accept").is_none());
    }
}
