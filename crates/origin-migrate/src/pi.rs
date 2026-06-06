// SPDX-License-Identifier: Apache-2.0
//! `pi` CLI transcript adapter (cross-harness live-resume).
//!
//! `pi` is a terminal coding agent whose sessions, like most CLI agents
//! (Codex, Claude Code), are persisted as JSONL — one JSON record per line —
//! typically under `<root>/sessions/**/*.jsonl`. When no `sessions/` subtree is
//! present we fall back to walking `<root>` for any `*.jsonl` file so a user can
//! point straight at a transcript directory or file.
//!
//! The exact `pi` record schema is not pinned by an upstream spec available
//! here, so this parser is deliberately PERMISSIVE and shape-tolerant: it reads
//! `role`/`type` for the speaker and `content` (string OR `[{type,text}]` array)
//! or a top-level `text` for the body — the union of the shapes CLI agents use.
//! A line that does not deserialize, or is a header/meta line, is skipped (not
//! fatal), so a single odd record never loses the rest of the transcript. If
//! `pi`'s real format differs, extend [`PiLine`]/[`flatten_content`] here — the
//! dispatch + CLI wiring already routes `pi`/`π` to this adapter.

use crate::source::{ImportedMessage, ImportedSession, MigrateBundle, Source, SourceError};
use serde::Deserialize;
use std::path::Path;
use walkdir::WalkDir;

#[allow(clippy::module_name_repetitions)]
#[derive(Default)]
pub struct PiSource;

/// One line of a `pi` transcript. Every field is optional so header, message,
/// and tool records all deserialize through the same permissive shape.
#[derive(Debug, Deserialize)]
struct PiLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    role: Option<String>,
    content: Option<serde_json::Value>,
    text: Option<String>,
    #[serde(default)]
    ts: Option<u64>,
}

/// Flatten a `content` value into a single body string: a bare string, or an
/// array of `{ "text": "…" }` objects / bare strings. Falls back to the
/// top-level `text` field when `content` is absent.
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

/// Map a `pi` record onto an origin import role string. Tool/function records
/// become `"tool"`; an explicit `assistant`/`system` role is preserved;
/// everything else defaults to `"user"` (keeps an unknown speaker rather than
/// dropping it).
fn map_pi_role(kind: Option<&str>, role: Option<&str>) -> String {
    if matches!(kind, Some("function_call_output" | "tool_result")) {
        return "tool".to_string();
    }
    match role.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("assistant" | "model" | "ai") => "assistant".to_string(),
        Some("tool" | "function" | "function_call_output") => "tool".to_string(),
        Some("system" | "developer") => "system".to_string(),
        Some(other) if !other.is_empty() => other.to_string(),
        _ => "user".to_string(),
    }
}

/// A record is a conversational message when it is explicitly `type:"message"`,
/// a tool record, or simply carries a `role` (header/meta lines carry neither).
fn is_message(line: &PiLine) -> bool {
    matches!(
        line.kind.as_deref(),
        Some("message" | "function_call_output" | "tool_result")
    ) || line.role.is_some()
}

impl Source for PiSource {
    fn name(&self) -> &'static str {
        "pi"
    }

    fn scan(&self, root: &Path) -> Result<MigrateBundle, SourceError> {
        let sessions_root = root.join("sessions");
        let walk_root = if sessions_root.exists() {
            sessions_root
        } else {
            root.to_path_buf()
        };

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
                let line: PiLine = match serde_json::from_str(raw) {
                    Ok(l) => l,
                    Err(err) => {
                        tracing::warn!(
                            source = "pi",
                            path = %entry.path().display(),
                            line = i + 1,
                            error = %err,
                            "skipping unparseable pi transcript record"
                        );
                        continue;
                    }
                };

                if created_at_unix_ms == 0 {
                    if let Some(ts) = line.ts {
                        created_at_unix_ms = ts;
                    }
                }

                if !is_message(&line) {
                    continue;
                }

                let role = map_pi_role(line.kind.as_deref(), line.role.as_deref());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_jsonl_roles_and_content_shapes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("s.jsonl");
        std::fs::write(
            &file,
            concat!(
                "{\"type\":\"session_meta\",\"ts\":111}\n",
                "{\"role\":\"user\",\"content\":\"hello\"}\n",
                "{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"hi \"},{\"type\":\"text\",\"text\":\"there\"}]}\n",
                "not-json-skip-me\n",
                "{\"type\":\"function_call_output\",\"text\":\"tool out\"}\n",
            ),
        )
        .expect("write");

        let bundle = PiSource.scan(dir.path()).expect("scan");
        assert_eq!(bundle.sessions.len(), 1);
        let msgs = &bundle.sessions[0].messages;
        assert_eq!(msgs.len(), 3, "meta + unparseable lines are skipped");
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].body, "hello");
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].body, "hi there", "array content is concatenated");
        assert_eq!(msgs[2].role, "tool");
        assert_eq!(msgs[2].body, "tool out");
    }
}
