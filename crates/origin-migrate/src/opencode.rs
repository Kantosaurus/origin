use crate::source::{ImportedMessage, ImportedSession, MigrateBundle, Source, SourceError};
use serde::Deserialize;
use std::path::Path;
use walkdir::WalkDir;

#[allow(clippy::module_name_repetitions)]
#[derive(Default)]
pub struct OpencodeSource;

#[derive(Debug, Deserialize)]
struct OcPart {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
struct OcMessage {
    role: String,
    parts: Vec<OcPart>,
}

#[derive(Debug, Deserialize)]
struct OcSession {
    id: String,
    title: Option<String>,
    #[serde(rename = "createdAt", default)]
    created_at: u64,
    messages: Vec<OcMessage>,
}

impl Source for OpencodeSource {
    fn name(&self) -> &str {
        "opencode"
    }

    fn scan(&self, root: &Path) -> Result<MigrateBundle, SourceError> {
        let storage = root.join("storage");
        if !storage.exists() {
            return Ok(MigrateBundle::default());
        }
        let mut bundle = MigrateBundle::default();
        for e in WalkDir::new(&storage)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "json"))
        {
            let body = std::fs::read(e.path())?;
            let s: OcSession = serde_json::from_slice(&body).map_err(|err| SourceError::Parse {
                path: e.path().display().to_string(),
                reason: err.to_string(),
            })?;
            let messages = s
                .messages
                .into_iter()
                .map(|m| ImportedMessage {
                    role: m.role,
                    body: m
                        .parts
                        .into_iter()
                        .filter(|p| p.kind == "text")
                        .map(|p| p.text)
                        .collect::<String>(),
                })
                .collect();
            bundle.sessions.push(ImportedSession {
                source_id: s.id,
                title: s.title,
                created_at_unix_ms: s.created_at,
                messages,
            });
        }
        Ok(bundle)
    }
}
