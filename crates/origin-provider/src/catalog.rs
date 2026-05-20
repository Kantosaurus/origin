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
    ApiKey { header: Cow<'static, str>, prefix: Cow<'static, str> },
    OAuth(OAuthSpec),
    SigV4 { service: Cow<'static, str> },
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
