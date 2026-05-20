use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImportedMessage {
    pub role: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedSession {
    pub source_id: String,
    pub title: Option<String>,
    pub created_at_unix_ms: u64,
    pub messages: Vec<ImportedMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedSkill {
    pub name: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedMemory {
    pub kind: String,
    pub body: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MigrateBundle {
    pub sessions: Vec<ImportedSession>,
    pub skills: Vec<ImportedSkill>,
    pub memories: Vec<ImportedMemory>,
}

#[derive(Debug, Error)]
#[allow(clippy::module_name_repetitions)]
pub enum SourceError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse {path}: {reason}")]
    Parse { path: String, reason: String },
    #[error("not found: {0}")]
    NotFound(String),
}

pub trait Source {
    fn name(&self) -> &str;

    /// Scan `root` and return a bundle of importable artifacts.
    ///
    /// # Errors
    /// Returns a [`SourceError`] if the directory is unreadable or contents
    /// fail to parse.
    fn scan(&self, root: &Path) -> Result<MigrateBundle, SourceError>;
}
