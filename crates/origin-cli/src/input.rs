//! Input event handling (key reducer).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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

#[cfg(test)]
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
}
