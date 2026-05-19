//! Input event handling (key reducer).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use origin_daemon::protocol::{ClientMessage, MemoryAction};

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, PartialEq, Eq)]
pub enum InputAction {
    Insert(char),
    Backspace,
    Submit(String),
    Quit,
    Noop,
}

#[must_use]
pub fn reduce(buffer: &mut String, ev: KeyEvent) -> InputAction {
    match (ev.code, ev.modifiers) {
        (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => InputAction::Quit,
        (KeyCode::Esc, _) => InputAction::Quit,
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
            reduce(&mut buf, k(KeyCode::Enter)),
            InputAction::Submit("hello".into())
        );
        assert!(buf.is_empty());
    }

    #[test]
    fn enter_on_empty_is_noop() {
        let mut buf = String::new();
        assert_eq!(reduce(&mut buf, k(KeyCode::Enter)), InputAction::Noop);
    }

    #[test]
    fn typing_appends_to_buffer() {
        let mut buf = "h".to_string();
        assert_eq!(reduce(&mut buf, k(KeyCode::Char('i'))), InputAction::Insert('i'));
        assert_eq!(buf, "hi");
    }

    #[test]
    fn backspace_pops() {
        let mut buf = "hi".to_string();
        assert_eq!(reduce(&mut buf, k(KeyCode::Backspace)), InputAction::Backspace);
        assert_eq!(buf, "h");
    }

    #[test]
    fn ctrl_c_quits() {
        let mut buf = String::new();
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(reduce(&mut buf, ev), InputAction::Quit);
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
}
