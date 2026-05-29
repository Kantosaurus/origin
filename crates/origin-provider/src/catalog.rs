// SPDX-License-Identifier: Apache-2.0
//! Static catalog of supported LLM providers.

use std::borrow::Cow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireFormat {
    OpenAIChat,
    Anthropic,
    Gemini,
    Bedrock,
    Ollama,
    GitHubCopilot,
}

#[derive(Debug, Clone)]
pub enum AuthScheme {
    None,
    ApiKey {
        header: Cow<'static, str>,
        prefix: Cow<'static, str>,
    },
    OAuth(OAuthSpec),
    SigV4 {
        service: Cow<'static, str>,
    },
    Custom,
}

#[derive(Debug, Clone)]
pub struct OAuthSpec {
    pub authorize_url: Cow<'static, str>,
    pub token_url: Cow<'static, str>,
    pub client_id: Cow<'static, str>,
    pub scopes: Cow<'static, [Cow<'static, str>]>,
    pub redirect_uri: Cow<'static, str>,
    pub pkce: bool,
    pub device_flow: bool,
}

#[derive(Debug, Clone, Copy, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Capabilities {
    pub streaming: bool,
    pub tools: bool,
    pub prompt_cache: bool,
    pub thinking: bool,
}

#[derive(Debug, Clone)]
pub struct ProviderEntry {
    pub id: Cow<'static, str>,
    pub display_name: Cow<'static, str>,
    pub wire: WireFormat,
    pub auth: AuthScheme,
    pub base_url: Cow<'static, str>,
    pub chat_path: Cow<'static, str>,
    pub default_model: Cow<'static, str>,
    pub capabilities: Capabilities,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_entry_constructs() {
        let e = ProviderEntry {
            id: "test".into(),
            display_name: "Test".into(),
            wire: WireFormat::OpenAIChat,
            auth: AuthScheme::None,
            base_url: "https://example.com".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "test-model".into(),
            capabilities: Capabilities::default(),
        };
        assert_eq!(e.id, "test");
        assert_eq!(e.wire, WireFormat::OpenAIChat);
    }
}

use crate::builtin_catalog;

/// Merged (builtin + custom) catalog handle.
#[derive(Debug, Clone)]
pub struct Catalog {
    entries: Vec<ProviderEntry>,
}

impl Catalog {
    #[must_use]
    pub fn builtin() -> Self {
        Self {
            entries: builtin_catalog(),
        }
    }

    /// Merges user-defined entries into the catalog.
    ///
    /// # Errors
    /// Returns [`CatalogError::IdCollision`] if any custom entry id matches an
    /// existing builtin entry id.
    pub fn merge_custom(&mut self, custom: Vec<ProviderEntry>) -> Result<(), CatalogError> {
        // Validate every entry BEFORE mutating, against both the existing
        // catalog and earlier custom entries, so a collision anywhere leaves the
        // catalog completely unchanged (atomic all-or-nothing merge).
        for (i, entry) in custom.iter().enumerate() {
            let collides =
                self.entries.iter().any(|e| e.id == entry.id) || custom[..i].iter().any(|e| e.id == entry.id);
            if collides {
                return Err(CatalogError::IdCollision(entry.id.to_string()));
            }
        }
        self.entries.extend(custom);
        Ok(())
    }

    #[must_use]
    pub fn lookup(&self, id: &str) -> Option<&ProviderEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    #[must_use]
    pub fn entries(&self) -> &[ProviderEntry] {
        &self.entries
    }
}

#[derive(Debug, thiserror::Error)]
#[allow(clippy::module_name_repetitions)]
pub enum CatalogError {
    #[error("user-defined provider id collides with builtin: {0}")]
    IdCollision(String),
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    #[test]
    #[allow(clippy::unwrap_used)]
    fn lookup_finds_builtin() {
        let cat = Catalog::builtin();
        assert!(cat.lookup("openai").is_some());
        assert!(cat.lookup("deepseek").is_some());
        assert!(cat.lookup("nonexistent").is_none());
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn collision_rejected() {
        let mut cat = Catalog::builtin();
        let dup = ProviderEntry {
            id: "openai".into(),
            display_name: "Fake".into(),
            wire: WireFormat::OpenAIChat,
            auth: AuthScheme::None,
            base_url: "https://x".into(),
            chat_path: "/x".into(),
            default_model: "x".into(),
            capabilities: Capabilities::default(),
        };
        assert!(cat.merge_custom(vec![dup]).is_err());
    }
}
