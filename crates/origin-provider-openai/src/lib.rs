//! `OpenAI` Chat Completions provider (non-streaming + SSE streaming).
//!
//! Implements the canonical `origin_provider::Provider` trait against
//! `POST /v1/chat/completions` with bearer-token auth. Streaming reuses the
//! shared `origin_provider::sse` pump; non-streaming tool calls are mapped
//! through the shared `origin_provider::openai_tools` module.

pub mod streaming;
mod wire;

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_provider::openai_tools::tool_call_to_block;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use reqwest::StatusCode;

const DEFAULT_BASE: &str = "https://api.openai.com";

/// `OpenAI` provider backed by Chat Completions with bearer-token auth.
pub struct OpenAi {
    api_key: String,
    base: String,
    client: reqwest::Client,
}

impl OpenAi {
    /// Construct with the default base URL (`https://api.openai.com`).
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE)
    }

    /// Construct against an arbitrary base URL (for testing and gateways).
    #[must_use]
    pub fn with_base_url(api_key: impl Into<String>, base: &str) -> Self {
        Self {
            api_key: api_key.into(),
            base: base.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Provider for OpenAi {
    fn name(&self) -> &'static str {
        "openai"
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let body = wire::encode_request(&req, false);
        let url = format!("{}/v1/chat/completions", self.base);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        match resp.status() {
            StatusCode::OK => {
                let wire: wire::WireResponse = resp
                    .json()
                    .await
                    .map_err(|e| ProviderError::Api(format!("decode: {e}")))?;
                Ok(decode_response(wire))
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(ProviderError::Auth),
            StatusCode::TOO_MANY_REQUESTS => {
                let retry = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(1);
                Err(ProviderError::RateLimit {
                    retry_after_secs: retry,
                })
            }
            s => {
                let body = resp.text().await.unwrap_or_default();
                Err(ProviderError::Api(format!("status {s}: {body}")))
            }
        }
    }

    async fn chat_stream(&self, req: ChatRequest, ring: &origin_stream::Ring) -> Result<(), ProviderError> {
        let body = wire::encode_request(&req, true);
        let url = format!("{}/v1/chat/completions", self.base);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api(format!("status {status}: {text}")));
        }

        streaming::parse_into_ring(resp, ring).await?;
        ring.close();
        Ok(())
    }
}

fn decode_response(wire: wire::WireResponse) -> ChatResponse {
    let mut blocks: Vec<Block> = Vec::new();
    let first = wire.choices.into_iter().next();
    if let Some(choice) = first {
        if let Some(text) = choice.message.content {
            if !text.is_empty() {
                blocks.push(Block::Text {
                    text,
                    cache_marker: None,
                });
            }
        }
        if let Some(tool_calls) = choice.message.tool_calls {
            for tc in &tool_calls {
                blocks.push(tool_call_to_block(tc));
            }
        }
    }

    let assistant = Message {
        role: Role::Assistant,
        blocks,
    };
    let usage = Usage {
        input_tokens: wire.usage.prompt_tokens,
        output_tokens: wire.usage.completion_tokens,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
    };
    ChatResponse { assistant, usage }
}
