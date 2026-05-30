// SPDX-License-Identifier: Apache-2.0
//! Output styles plus a transform-or-hide `MessageDisplay` hook for `origin`.
//!
//! Two orthogonal text concerns live here. A [`Style`] picks a claude-code-style
//! output persona (Explanatory, Learning, Concise) and contributes a
//! [`Style::system_suffix`] appended to the system prompt. Separately, a
//! `MessageDisplay` hook can rewrite or suppress a rendered message via a
//! [`DisplayAction`]; [`parse_display_hook`] decodes a hook's JSON verdict and
//! [`apply_display`] applies it. The crate is a pure text transform — no I/O, no
//! async — so every path is offline and trivially testable.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// A claude-code-style output persona that shapes the assistant's prose.
///
/// The selected style contributes a [`Style::system_suffix`] appended to the
/// system prompt; it does not alter tool behaviour, only the narration around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Style {
    /// Neutral default — no extra guidance.
    #[default]
    Default,
    /// Explain the reasoning behind each step as work proceeds.
    Explanatory,
    /// Teach the underlying concepts as if mentoring the reader.
    Learning,
    /// Be terse: minimal prose, answer-first.
    Concise,
}

impl Style {
    /// Parse a case-insensitive style label, returning `None` if unrecognised.
    ///
    /// Accepts the canonical labels (`default`, `explanatory`, `learning`,
    /// `concise`) regardless of surrounding whitespace or letter case.
    #[must_use]
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "default" => Some(Self::Default),
            "explanatory" => Some(Self::Explanatory),
            "learning" => Some(Self::Learning),
            "concise" => Some(Self::Concise),
            _ => None,
        }
    }

    /// The canonical lowercase label for this style.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Explanatory => "explanatory",
            Self::Learning => "learning",
            Self::Concise => "concise",
        }
    }

    /// Extra system-prompt guidance contributed by this style.
    ///
    /// The daemon appends this to the system prompt. [`Style::Default`] returns
    /// an empty string (it contributes nothing); every other style returns a
    /// non-empty instruction.
    #[must_use]
    pub const fn system_suffix(self) -> &'static str {
        match self {
            Self::Default => "",
            Self::Explanatory => {
                "Output style: Explanatory. As you work, explain the reasoning \
                 behind your choices and the trade-offs you weigh, so the reader \
                 understands not just what you did but why."
            }
            Self::Learning => {
                "Output style: Learning. Teach the underlying concepts as you go, \
                 as if mentoring the reader; surface the principles at play and \
                 invite them to learn alongside the solution."
            }
            Self::Concise => {
                "Output style: Concise. Be terse and answer-first; minimise prose, \
                 omit pleasantries, and prefer the shortest correct response."
            }
        }
    }
}

/// A `MessageDisplay` hook's decision about a single rendered message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "action", content = "text")]
pub enum DisplayAction {
    /// Show the original text unchanged.
    Show,
    /// Suppress the message entirely (render nothing).
    Hide,
    /// Replace the message with the wrapped text.
    Replace(String),
}

/// The structured result returned by a `MessageDisplay` hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayHookResult {
    /// The decided action for the inspected message.
    pub action: DisplayAction,
}

impl DisplayHookResult {
    /// Construct a result wrapping `action`.
    #[must_use]
    pub const fn new(action: DisplayAction) -> Self {
        Self { action }
    }
}

/// Apply a [`DisplayAction`] to `text`.
///
/// Returns `Some(text)` for [`DisplayAction::Show`], `None` for
/// [`DisplayAction::Hide`], and `Some(replacement)` for
/// [`DisplayAction::Replace`]. A `None` result means the caller should render
/// nothing for this message.
#[must_use]
pub fn apply_display(text: &str, action: &DisplayAction) -> Option<String> {
    match action {
        DisplayAction::Show => Some(text.to_string()),
        DisplayAction::Hide => None,
        DisplayAction::Replace(s) => Some(s.clone()),
    }
}

/// Parse a `MessageDisplay` hook's JSON verdict.
///
/// The expected shape is an object with an `action` field that is one of
/// `"show"`, `"hide"`, or `"replace"`, plus an optional `"text"` field. For
/// `"replace"` the `"text"` field supplies the replacement (defaulting to the
/// empty string when absent); for `"show"`/`"hide"` any `"text"` is ignored.
///
/// # Errors
///
/// Returns [`OutputStyleError::Parse`] when the input is not valid JSON, is not
/// an object, omits the `action` field, or carries an unrecognised action.
pub fn parse_display_hook(json: &str) -> Result<DisplayHookResult, OutputStyleError> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| OutputStyleError::Parse(e.to_string()))?;
    let obj = value
        .as_object()
        .ok_or_else(|| OutputStyleError::Parse("expected a JSON object".to_string()))?;
    let action = obj
        .get("action")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| OutputStyleError::Parse("missing string field `action`".to_string()))?;
    let text = obj.get("text").and_then(serde_json::Value::as_str);
    let action = match action.trim().to_ascii_lowercase().as_str() {
        "show" => DisplayAction::Show,
        "hide" => DisplayAction::Hide,
        "replace" => DisplayAction::Replace(text.unwrap_or_default().to_string()),
        other => {
            return Err(OutputStyleError::Parse(format!(
                "unknown action `{other}` (expected show|hide|replace)"
            )));
        }
    };
    Ok(DisplayHookResult::new(action))
}

/// Errors produced while decoding output-style hook input.
#[derive(Debug, thiserror::Error)]
pub enum OutputStyleError {
    /// The hook JSON could not be parsed or was structurally invalid.
    #[error("failed to parse display hook: {0}")]
    Parse(String),
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn from_str_opt_round_trips_labels() {
        for style in [
            Style::Default,
            Style::Explanatory,
            Style::Learning,
            Style::Concise,
        ] {
            assert_eq!(Style::from_str_opt(style.label()), Some(style));
        }
        // Case- and whitespace-insensitive.
        assert_eq!(Style::from_str_opt("  Learning "), Some(Style::Learning));
        assert_eq!(Style::from_str_opt("nonsense"), None);
    }

    #[test]
    fn system_suffix_non_empty_for_non_default() {
        for style in [Style::Explanatory, Style::Learning, Style::Concise] {
            assert!(
                !style.system_suffix().is_empty(),
                "{} suffix should be non-empty",
                style.label()
            );
        }
    }

    #[test]
    fn default_system_suffix_is_empty() {
        assert_eq!(Style::Default.system_suffix(), "");
        assert_eq!(Style::default(), Style::Default);
    }

    #[test]
    fn apply_display_show_returns_original() {
        assert_eq!(
            apply_display("hello", &DisplayAction::Show),
            Some("hello".to_string())
        );
    }

    #[test]
    fn apply_display_hide_returns_none() {
        assert_eq!(apply_display("hello", &DisplayAction::Hide), None);
    }

    #[test]
    fn apply_display_replace_returns_substitute() {
        let action = DisplayAction::Replace("[redacted]".to_string());
        assert_eq!(
            apply_display("secret", &action),
            Some("[redacted]".to_string())
        );
    }

    #[test]
    fn parse_display_hook_all_three_actions() {
        let show = parse_display_hook(r#"{"action":"show"}"#).unwrap();
        assert_eq!(show.action, DisplayAction::Show);

        let hide = parse_display_hook(r#"{"action":"HIDE"}"#).unwrap();
        assert_eq!(hide.action, DisplayAction::Hide);

        let replace = parse_display_hook(r#"{"action":"replace","text":"hi"}"#).unwrap();
        assert_eq!(replace.action, DisplayAction::Replace("hi".to_string()));

        // Replace without text defaults to empty.
        let replace_empty = parse_display_hook(r#"{"action":"replace"}"#).unwrap();
        assert_eq!(
            replace_empty.action,
            DisplayAction::Replace(String::new())
        );
    }

    #[test]
    fn parse_display_hook_bad_json_errs() {
        assert!(parse_display_hook("not json").is_err());
        assert!(parse_display_hook("[1,2,3]").is_err());
        assert!(parse_display_hook(r#"{"action":"explode"}"#).is_err());
        assert!(parse_display_hook(r#"{"nope":true}"#).is_err());
    }

    #[test]
    fn parse_then_apply_round_trip() {
        let result = parse_display_hook(r#"{"action":"replace","text":"X"}"#).unwrap();
        assert_eq!(
            apply_display("original", &result.action),
            Some("X".to_string())
        );
    }
}
