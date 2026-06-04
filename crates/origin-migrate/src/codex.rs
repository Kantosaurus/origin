// SPDX-License-Identifier: Apache-2.0
//! Codex CLI rollout-log adapter.
//!
//! The Codex CLI persists each session as a JSONL *rollout log* — one JSON
//! record per line — typically under `<root>/sessions/**/rollout-*.jsonl`. When
//! no `sessions/` subtree is present we fall back to walking `<root>` for any
//! `*.jsonl` file so a user can point directly at a transcript directory.
//!
//! Records are parsed permissively: a line that does not deserialize (or is not
//! a message) is logged and skipped rather than aborting the whole scan, so a
//! single malformed record can never lose an otherwise-recoverable transcript.

use crate::source::{ImportedMessage, ImportedSession, MigrateBundle, Source, SourceError};
use serde::Deserialize;
use std::path::Path;
use walkdir::WalkDir;

#[allow(clippy::module_name_repetitions)]
#[derive(Default)]
pub struct CodexSource;

/// One line of a Codex rollout log.
///
/// Deliberately permissive: every field is optional so header records,
/// message records, and tool-output records all deserialize through the same
/// shape. `content` is left as a raw [`serde_json::Value`] because Codex emits
/// it either as a string or as an array of `{type, text}` segments.
#[derive(Debug, Deserialize)]
struct CodexLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    role: Option<String>,
    content: Option<serde_json::Value>,
    text: Option<String>,
    #[serde(default)]
    ts: Option<u64>,
}

/// Flatten a Codex `content` value into a single body string.
///
/// Accepts a bare string, or an array of segments where each segment is either
/// a `{ "text": "…" }` object (any `type`) or itself a bare string. Anything
/// else contributes nothing. The optional top-level `text` field is used as a
/// fallback when `content` is absent.
fn flatten_content(content: Option<&serde_json::Value>, text: Option<&str>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => {
            let mut body = String::new();
            for part in parts {
                match part {
                    serde_json::Value::String(s) => body.push_str(s),
                    serde_json::Value::Object(map) => {
                        if let Some(serde_json::Value::String(s)) = map.get("text") {
                            body.push_str(s);
                        }
                    }
                    _ => {}
                }
            }
            body
        }
        _ => text.unwrap_or_default().to_string(),
    }
}

/// Map a Codex record onto an origin import role string.
///
/// `function_call_output` / `tool` records become `"tool"`; an explicit
/// `assistant` role is preserved; everything else defaults to `"user"` — the
/// safe default that keeps an unknown speaker in the transcript rather than
/// dropping it.
fn map_codex_role(kind: Option<&str>, role: Option<&str>) -> String {
    if matches!(kind, Some("function_call_output")) {
        return "tool".to_string();
    }
    match role.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("assistant") => "assistant".to_string(),
        Some("tool" | "function" | "function_call_output") => "tool".to_string(),
        Some("system" | "developer") => "system".to_string(),
        Some(other) if !other.is_empty() => other.to_string(),
        _ => "user".to_string(),
    }
}

/// Does this record represent a conversational message (vs. a header/meta line)?
///
/// A record is a message when it is explicitly `type: "message"`, is a
/// `function_call_output`, or simply carries a `role` (Codex header/meta lines
/// such as `session_meta` carry neither).
fn is_message(line: &CodexLine) -> bool {
    matches!(line.kind.as_deref(), Some("message" | "function_call_output")) || line.role.is_some()
}

impl Source for CodexSource {
    fn name(&self) -> &str {
        "codex"
    }

    fn scan(&self, root: &Path) -> Result<MigrateBundle, SourceError> {
        let sessions_root = root.join("sessions");
        // Codex normally nests under `sessions/`; fall back to the root itself
        // so a user can point straight at a directory of `*.jsonl` transcripts.
        let walk_root = if sessions_root.exists() { sessions_root } else { root.to_path_buf() };

        let mut bundle = MigrateBundle::default();

        for entry in WalkDir::new(&walk_root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "jsonl"))
        {
            let body = std::fs::read_to_string(entry.path())?;
            let source_id = entry
                .path()
                .strip_prefix(root)
                .unwrap_or_else(|_| entry.path())
                .to_string_lossy()
                .into_owned();

            let mut created_at_unix_ms = 0u64;
            let mut messages: Vec<ImportedMessage> = Vec::new();

            for (i, raw) in body.lines().enumerate() {
                if raw.trim().is_empty() {
                    continue;
                }
                let line: CodexLine = match serde_json::from_str(raw) {
                    Ok(l) => l,
                    Err(err) => {
                        // Skip and continue: a single unparseable record must not
                        // abort recovery of the rest of the transcript.
                        tracing::warn!(
                            source = "codex",
                            path = %entry.path().display(),
                            line = i + 1,
                            error = %err,
                            "skipping unparseable codex rollout record"
                        );
                        continue;
                    }
                };

                // Capture the first timestamp we see (header lines carry it).
                if created_at_unix_ms == 0 {
                    if let Some(ts) = line.ts {
                        created_at_unix_ms = ts;
                    }
                }

                if !is_message(&line) {
                    continue;
                }

                let role = map_codex_role(line.kind.as_deref(), line.role.as_deref());
                let body = flatten_content(line.content.as_ref(), line.text.as_deref());
                messages.push(ImportedMessage { role, body });
            }

            bundle.sessions.push(ImportedSession {
                source_id,
                title: None,
                created_at_unix_ms,
                messages,
            });
        }

        Ok(bundle)
    }
}
