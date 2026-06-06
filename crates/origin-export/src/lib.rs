// SPDX-License-Identifier: Apache-2.0
//! Conversation transcript export for `origin` sessions.
//!
//! `origin` keeps a session's turns in memory but offers no portable artifact a
//! user can read, diff, or hand to a teammate. This crate closes that gap
//! (openclaude's `/export`, opencode's local share) by rendering an
//! [`ExportSession`] into either a clean Markdown transcript with a YAML-ish
//! header and role headings, or a pretty-printed JSON document.
//!
//! The crate is pure logic plus serde — no I/O, no async — so it is trivially
//! testable and free of platform concerns.
//!
//! ```
//! use origin_export::{ExportSession, ExportTurn, to_markdown, to_json};
//!
//! let session = ExportSession {
//!     id: "s-1".to_string(),
//!     title: Some("Fix the build".to_string()),
//!     provider: "anthropic".to_string(),
//!     model: "claude-sonnet-4-6".to_string(),
//!     created_at_unix_ms: 1_700_000_000_000,
//!     turns: vec![ExportTurn {
//!         role: "user".to_string(),
//!         text: "why is CI red?".to_string(),
//!         tools: vec![],
//!     }],
//! };
//!
//! let md = to_markdown(&session);
//! assert!(md.contains("Fix the build"));
//! let json = to_json(&session).expect("session serializes");
//! assert!(json.contains("\"model\""));
//! ```

#![forbid(unsafe_code)]

use core::fmt::Write as _;
use serde::{Deserialize, Serialize};

/// A single turn in an exported conversation.
///
/// Mirrors the daemon's per-turn shape closely enough that conversion is a
/// field-for-field copy with no lossy intermediate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportTurn {
    /// Speaker for this turn, e.g. `user`, `assistant`, or `system`.
    pub role: String,
    /// The turn's textual content (already rendered to plain text/Markdown).
    pub text: String,
    /// Names of tools the turn invoked, in call order. May be empty.
    pub tools: Vec<String>,
}

/// A complete conversation prepared for export.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportSession {
    /// Stable session identifier.
    pub id: String,
    /// Optional human-readable title; falls back to the id when absent.
    pub title: Option<String>,
    /// Provider that served the session, e.g. `anthropic`.
    pub provider: String,
    /// Model identifier, e.g. `claude-sonnet-4-6`.
    pub model: String,
    /// Session creation time as Unix milliseconds.
    pub created_at_unix_ms: u64,
    /// Turns in chronological order.
    pub turns: Vec<ExportTurn>,
}

/// Errors that can occur while exporting a session.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// Serialization to the target format failed.
    #[error("failed to serialize session: {0}")]
    Serialize(String),
}

/// Render `s` as a clean, readable Markdown transcript.
///
/// The output opens with a YAML-ish front-matter header (delimited by `---`)
/// carrying the session metadata, followed by one section per turn. Each
/// section has a role heading and, when the turn used tools, a fenced list of
/// the tool names.
///
/// This function is infallible: every field is a plain string or number and the
/// rendering never allocates fallibly in a way the caller can act on.
#[must_use]
pub fn to_markdown(s: &ExportSession) -> String {
    let mut out = String::new();
    let title = s.title.as_deref().unwrap_or(&s.id);

    // YAML-ish front matter. Writing into a `String` is infallible, so the
    // `write!` results are deliberately discarded (no fallible I/O here).
    out.push_str("---\n");
    let _ = writeln!(out, "title: {title}");
    let _ = writeln!(out, "id: {}", s.id);
    let _ = writeln!(out, "provider: {}", s.provider);
    let _ = writeln!(out, "model: {}", s.model);
    let _ = writeln!(out, "created_at_unix_ms: {}", s.created_at_unix_ms);
    let _ = writeln!(out, "turns: {}", s.turns.len());
    out.push_str("---\n\n");

    // Document title.
    let _ = writeln!(out, "# {title}");

    for turn in &s.turns {
        out.push('\n');
        let _ = writeln!(out, "## {}\n", heading_for(&turn.role));

        let body = turn.text.trim_end();
        if body.is_empty() {
            out.push_str("_(no content)_\n");
        } else {
            out.push_str(body);
            out.push('\n');
        }

        if !turn.tools.is_empty() {
            out.push_str("\n**Tools**\n\n");
            out.push_str("```\n");
            for tool in &turn.tools {
                out.push_str(tool);
                out.push('\n');
            }
            out.push_str("```\n");
        }
    }

    out
}

/// Capitalize a role for use as a section heading: `user` -> `User`.
fn heading_for(role: &str) -> String {
    let mut chars = role.chars();
    chars.next().map_or_else(
        || "Turn".to_string(),
        |first| first.to_uppercase().chain(chars).collect(),
    )
}

/// Serialize `s` to pretty-printed JSON.
///
/// # Errors
///
/// Returns [`ExportError::Serialize`] if serde fails to encode the session,
/// which in practice only happens on allocation failure since every field is a
/// plain owned value.
pub fn to_json(s: &ExportSession) -> Result<String, ExportError> {
    serde_json::to_string_pretty(s).map_err(|e| ExportError::Serialize(e.to_string()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    fn sample() -> ExportSession {
        ExportSession {
            id: "sess-abc".to_string(),
            title: Some("Refactor the parser".to_string()),
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            created_at_unix_ms: 1_700_000_000_000,
            turns: vec![
                ExportTurn {
                    role: "user".to_string(),
                    text: "Please refactor the tokenizer.".to_string(),
                    tools: vec![],
                },
                ExportTurn {
                    role: "assistant".to_string(),
                    text: "Done. I edited two files.".to_string(),
                    tools: vec!["read_file".to_string(), "edit_file".to_string()],
                },
            ],
        }
    }

    #[test]
    fn json_round_trips() {
        let s = sample();
        let json = to_json(&s).unwrap();
        let back: ExportSession = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn json_is_pretty() {
        let json = to_json(&sample()).unwrap();
        // Pretty output is multi-line and indented.
        assert!(json.contains('\n'));
        assert!(json.contains("  \"id\": \"sess-abc\""));
    }

    #[test]
    fn markdown_contains_title_model_and_roles() {
        let md = to_markdown(&sample());
        assert!(md.contains("title: Refactor the parser"));
        assert!(md.contains("# Refactor the parser"));
        assert!(md.contains("model: claude-sonnet-4-6"));
        assert!(md.contains("## User"));
        assert!(md.contains("## Assistant"));
        assert!(md.contains("Please refactor the tokenizer."));
    }

    #[test]
    fn markdown_renders_tool_list_in_fence() {
        let md = to_markdown(&sample());
        assert!(md.contains("**Tools**"));
        assert!(md.contains("```\nread_file\nedit_file\n```"));
    }

    #[test]
    fn markdown_omits_tools_when_none() {
        let s = ExportSession {
            turns: vec![ExportTurn {
                role: "user".to_string(),
                text: "hi".to_string(),
                tools: vec![],
            }],
            ..sample()
        };
        let md = to_markdown(&s);
        // The user turn has no tools, and the sample's only tool-bearing turn
        // was replaced, so no fence should appear.
        assert!(!md.contains("**Tools**"));
    }

    #[test]
    fn markdown_falls_back_to_id_without_title() {
        let s = ExportSession {
            title: None,
            ..sample()
        };
        let md = to_markdown(&s);
        assert!(md.contains("title: sess-abc"));
        assert!(md.contains("# sess-abc"));
    }

    #[test]
    fn empty_session_renders_and_serializes() {
        let s = ExportSession::default();
        let md = to_markdown(&s);
        // Header still present; zero turns reported; no panic.
        assert!(md.contains("turns: 0"));
        assert!(md.contains("---"));
        let json = to_json(&s).unwrap();
        let back: ExportSession = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn empty_turn_text_shows_placeholder() {
        let s = ExportSession {
            turns: vec![ExportTurn {
                role: "system".to_string(),
                text: "   ".to_string(),
                tools: vec![],
            }],
            ..sample()
        };
        let md = to_markdown(&s);
        assert!(md.contains("## System"));
        assert!(md.contains("_(no content)_"));
    }

    #[test]
    fn heading_handles_empty_role() {
        assert_eq!(heading_for(""), "Turn");
        assert_eq!(heading_for("user"), "User");
        assert_eq!(heading_for("assistant"), "Assistant");
    }
}
