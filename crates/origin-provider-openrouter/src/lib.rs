//! `OpenRouter` provider — thin wrapper around `origin-provider-openai-compat`.

use origin_provider_openai_compat::{OpenAiCompat, OpenAiCompatConfig, StaticBearer};

const DEFAULT_BASE: &str = "https://openrouter.ai";
const REFERER: &str = "https://origin.local";
const TITLE: &str = "origin";

pub struct OpenRouter(OpenAiCompat);

impl OpenRouter {
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE)
    }

    #[must_use]
    pub fn with_base_url(api_key: impl Into<String>, base: &str) -> Self {
        let cfg = OpenAiCompatConfig {
            name: "openrouter",
            base_url: base.trim_end_matches('/').to_string(),
            chat_path: "/api/v1/chat/completions".to_string(),
            auth: StaticBearer::new(api_key.into()),
            extra_headers: vec![
                ("HTTP-Referer".to_string(), REFERER.to_string()),
                ("X-Title".to_string(), TITLE.to_string()),
            ],
        };
        Self(OpenAiCompat::new(cfg))
    }
}

#[async_trait::async_trait]
impl origin_provider::Provider for OpenRouter {
    fn name(&self) -> &'static str { self.0.name() }

    async fn chat(&self, req: origin_provider::ChatRequest) -> Result<origin_provider::ChatResponse, origin_provider::ProviderError> {
        self.0.chat(req).await
    }

    async fn chat_stream(&self, req: origin_provider::ChatRequest, ring: &origin_stream::Ring) -> Result<(), origin_provider::ProviderError> {
        self.0.chat_stream(req, ring).await
    }
}
