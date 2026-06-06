// SPDX-License-Identifier: Apache-2.0
//! `/style` slash-command parsing and message-display application.
//!
//! A leading `/style` token selects an [`origin_outputstyle::Style`] whose
//! [`system_suffix`](origin_outputstyle::Style::system_suffix) is appended to
//! the system prompt. [`Style::Default`] contributes an empty suffix, so the
//! default path is unchanged.
//!
//! This lands the parse plus a `MessageDisplay`-hook application point so the
//! mechanism is testable and ready to wire. Threading the chosen suffix into the
//! live daemon `ChatRequest` is deferred (the CLI has no interactive
//! prompt-rewrite loop yet).
// TODO(wire): thread `Style::system_suffix()` into the per-turn system prompt
// once the interactive composer routes typed `/`-commands through the daemon.
#![allow(clippy::module_name_repetitions)]

use origin_outputstyle::{apply_display, DisplayAction, Style};

/// Parse a `/style <name>` slash command.
///
/// Returns `Some(style)` when `line` is exactly `/style <name>` with a
/// recognised name (`default`, `explanatory`, `learning`, `concise`, in any
/// case), and `None` for any other input (so non-style lines are untouched).
#[must_use]
pub fn parse_style_command(line: &str) -> Option<Style> {
    let rest = line.trim().strip_prefix("/style")?;
    // Require whitespace (or end) after the command token so `/styleX` is not
    // mistaken for the command.
    let arg = rest.trim_start();
    if !rest.is_empty() && arg.len() == rest.len() {
        return None;
    }
    Style::from_str_opt(arg)
}

/// Apply a `MessageDisplay` decision to a rendered message under the active
/// style.
///
/// This is the hook application point for the `MessageDisplay` lifecycle event:
/// given the verdict `action`, it returns the text to render (or `None` to hide
/// it). The `style` is accepted so future styling can post-process the shown
/// text; today it forwards the decision unchanged.
#[must_use]
pub fn render_with_style(_style: Style, text: &str, action: &DisplayAction) -> Option<String> {
    apply_display(text, action)
}

#[cfg(test)]
mod tests {
    use super::{parse_style_command, render_with_style};
    use origin_outputstyle::{DisplayAction, Style};

    #[test]
    fn parses_each_named_style() {
        assert_eq!(parse_style_command("/style explanatory"), Some(Style::Explanatory));
        assert_eq!(parse_style_command("/style learning"), Some(Style::Learning));
        assert_eq!(parse_style_command("/style concise"), Some(Style::Concise));
        assert_eq!(parse_style_command("/style default"), Some(Style::Default));
        // Case- and whitespace-insensitive.
        assert_eq!(parse_style_command("  /style  Concise  "), Some(Style::Concise));
    }

    #[test]
    fn rejects_non_style_and_unknown_names() {
        assert_eq!(parse_style_command("hello world"), None);
        assert_eq!(parse_style_command("/model gpt-4o"), None);
        assert_eq!(parse_style_command("/style bogus"), None);
        // No argument, or a glued token, is not a valid command.
        assert_eq!(parse_style_command("/style"), None);
        assert_eq!(parse_style_command("/styleconcise"), None);
    }

    #[test]
    fn default_style_suffix_is_empty() {
        // The default style contributes nothing to the system prompt, so the
        // default path stays byte-identical.
        assert!(Style::Default.system_suffix().is_empty());
        assert!(!Style::Explanatory.system_suffix().is_empty());
    }

    #[test]
    fn render_with_style_applies_display_action() {
        assert_eq!(
            render_with_style(Style::Concise, "hi", &DisplayAction::Show),
            Some("hi".to_string())
        );
        assert_eq!(
            render_with_style(Style::Concise, "secret", &DisplayAction::Hide),
            None
        );
        assert_eq!(
            render_with_style(Style::Default, "x", &DisplayAction::Replace("y".to_string())),
            Some("y".to_string())
        );
    }
}
