//! Loads user-defined provider entries from a TOML file.

use std::borrow::Cow;
use std::path::Path;

use serde::Deserialize;

use crate::catalog::{AuthScheme, Capabilities, ProviderEntry, WireFormat};

#[derive(Debug, Deserialize)]
struct TomlRoot {
    #[serde(default)]
    providers: std::collections::BTreeMap<String, TomlEntry>,
}

#[derive(Debug, Deserialize)]
struct TomlEntry {
    display_name: String,
    wire: String,
    base_url: String,
    #[serde(default = "default_chat_path")]
    chat_path: String,
    default_model: String,
    #[serde(default)]
    auth: Option<TomlAuth>,
}

fn default_chat_path() -> String {
    "/v1/chat/completions".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum TomlAuth {
    None,
    ApiKey {
        header: String,
        #[serde(default)]
        prefix: String,
    },
}

/// Errors that can occur when loading a custom providers TOML file.
#[derive(Debug, thiserror::Error)]
#[allow(clippy::module_name_repetitions)]
pub enum CustomLoadError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("unknown wire format: {0}")]
    UnknownWire(String),
}

/// Load user-defined provider entries from `path`.
///
/// Returns `Vec::new()` if the file does not exist.
///
/// # Errors
/// Returns [`CustomLoadError`] for IO failures, TOML parse errors, or unknown
/// wire-format strings.
pub fn load(path: &Path) -> Result<Vec<ProviderEntry>, CustomLoadError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path)?;
    let root: TomlRoot = toml::from_str(&text)?;
    let mut out = Vec::with_capacity(root.providers.len());
    for (id, entry) in root.providers {
        let wire = match entry.wire.as_str() {
            "openai-chat" => WireFormat::OpenAIChat,
            "anthropic" => WireFormat::Anthropic,
            "gemini" => WireFormat::Gemini,
            "ollama" => WireFormat::Ollama,
            other => return Err(CustomLoadError::UnknownWire(other.to_string())),
        };
        let auth = match entry.auth.unwrap_or(TomlAuth::None) {
            TomlAuth::None => AuthScheme::None,
            TomlAuth::ApiKey { header, prefix } => AuthScheme::ApiKey {
                header: Cow::Owned(header),
                prefix: Cow::Owned(prefix),
            },
        };
        out.push(ProviderEntry {
            id: Cow::Owned(id),
            display_name: Cow::Owned(entry.display_name),
            wire,
            auth,
            base_url: Cow::Owned(entry.base_url),
            chat_path: Cow::Owned(entry.chat_path),
            default_model: Cow::Owned(entry.default_model),
            capabilities: Capabilities {
                streaming: true,
                tools: true,
                ..Default::default()
            },
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    #[allow(clippy::unwrap_used)]
    fn loads_minimal_toml() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            f,
            r#"
[providers.my-gateway]
display_name = "My Gateway"
wire = "openai-chat"
base_url = "https://gw.example.com"
default_model = "llama-3.3-70b"
auth = {{ kind = "api-key", header = "Authorization", prefix = "Bearer " }}
"#
        )
        .unwrap();
        let entries = load(f.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "my-gateway");
        assert_eq!(entries[0].base_url, "https://gw.example.com");
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn missing_file_yields_empty() {
        let entries = load(std::path::Path::new("/nonexistent/providers.toml")).unwrap();
        assert!(entries.is_empty());
    }
}
