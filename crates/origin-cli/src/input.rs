//! Input event handling (key reducer).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use origin_daemon::protocol::{ClientMessage, MemoryAction};

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
    let mut it = rest.splitn(3, char::is_whitespace);
    let verb = it.next()?;
    let id_tok = it.next()?;
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
            let body = it.next()?.trim();
            if body.is_empty() {
                return None;
            }
            Some(ClientMessage::MemoryDecision {
                proposal_id,
                action: MemoryAction::Edit {
                    body: body.to_string(),
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
const RESERVED_SLASH_VERBS: &[&str] = &["mem", "account", "help", "model"];

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
    let (name_token, args_str) = match rest.find(char::is_whitespace) {
        Some(idx) => {
            let (n, a) = rest.split_at(idx);
            (n, a.trim_start())
        }
        None => (rest, ""),
    };
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
        let got = parse_skill_command("/clear");
        assert!(matches!(
            got,
            Some(ClientMessage::ActivateSkill { ref name, args: None })
                if name == "clear"
        ));
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
