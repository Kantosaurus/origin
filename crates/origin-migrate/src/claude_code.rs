// SPDX-License-Identifier: Apache-2.0
use crate::source::{ImportedMessage, ImportedSession, ImportedSkill, MigrateBundle, Source, SourceError};
use serde::Deserialize;
use std::path::Path;
use walkdir::WalkDir;

#[allow(clippy::module_name_repetitions)]
#[derive(Default)]
pub struct ClaudeCodeSource;

#[derive(Debug, Deserialize)]
struct CcLine {
    #[serde(rename = "type")]
    kind: String,
    content: String,
}

impl Source for ClaudeCodeSource {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn scan(&self, root: &Path) -> Result<MigrateBundle, SourceError> {
        let projects_root = root.join("projects");
        let skills_root = root.join("skills");

        let mut bundle = MigrateBundle::default();

        if projects_root.exists() {
            for e in WalkDir::new(&projects_root)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "jsonl"))
            {
                let body = std::fs::read_to_string(e.path())?;
                let mut session = ImportedSession {
                    source_id: e
                        .path()
                        .strip_prefix(root)
                        .unwrap_or_else(|_| e.path())
                        .to_string_lossy()
                        .into_owned(),
                    title: None,
                    created_at_unix_ms: 0,
                    messages: vec![],
                };
                for (i, line) in body.lines().enumerate() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let cc: CcLine = serde_json::from_str(line).map_err(|err| SourceError::Parse {
                        path: format!("{}:{}", e.path().display(), i + 1),
                        reason: err.to_string(),
                    })?;
                    session.messages.push(ImportedMessage {
                        role: cc.kind,
                        body: cc.content,
                    });
                }
                bundle.sessions.push(session);
            }
        }

        if skills_root.exists() {
            for e in WalkDir::new(&skills_root)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_file() && e.path().file_name().is_some_and(|n| n == "SKILL.md"))
            {
                let body = std::fs::read_to_string(e.path())?;
                let name = e
                    .path()
                    .parent()
                    .and_then(|p| p.file_name())
                    .map_or_else(|| "unknown".into(), |n| n.to_string_lossy().into_owned());
                bundle.skills.push(ImportedSkill { name, body });
            }
        }

        Ok(bundle)
    }
}
