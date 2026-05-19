//! Provider trait + canonical request/response/usage/error types.
//!
//! Each `Provider` implementation translates the canonical types to its wire
//! format. Multi-provider support is achieved by per-provider crates that
//! `impl Provider for …`.

use origin_core::types::Message;
use thiserror::Error;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_input_tokens: u32,
    pub cache_creation_input_tokens: u32,
}

#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON Schema describing the tool input shape.
    pub input_schema_json: String,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<Message>,
    pub model: String,
    pub tools: Vec<ToolSchema>,
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub assistant: Message,
    pub usage: Usage,
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("api: {0}")]
    Api(String),
    #[error("auth")]
    Auth,
    #[error("rate limit; retry after {retry_after_secs}s")]
    RateLimit { retry_after_secs: u32 },
}

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;

    /// Send a single non-streaming chat request.
    ///
    /// # Errors
    /// Returns `ProviderError` for transport, API, auth, or rate-limit failures.
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError>;

    /// Stream tokens into `ring`. Default impl falls back to `chat` and emits
    /// one `TextDelta` + `TurnEnd` so providers without native streaming still
    /// work behind the ring API.
    ///
    /// # Errors
    /// Returns `ProviderError` for transport, API, auth, or rate-limit failures.
    async fn chat_stream(&self, req: ChatRequest, ring: &origin_stream::Ring) -> Result<(), ProviderError> {
        let resp = self.chat(req).await?;
        let text: String = resp
            .assistant
            .blocks
            .iter()
            .filter_map(|b| match b {
                origin_core::types::Block::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        ring.publish(&origin_stream::TokenEvent::new(
            origin_stream::TokenKind::TextDelta,
            text.into_bytes(),
        ))
        .map_err(|e| ProviderError::Api(e.to_string()))?;
        ring.publish(&origin_stream::TokenEvent::new(
            origin_stream::TokenKind::TurnEnd,
            Vec::new(),
        ))
        .map_err(|e| ProviderError::Api(e.to_string()))?;
        ring.close();
        Ok(())
    }
}
