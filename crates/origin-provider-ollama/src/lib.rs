// SPDX-License-Identifier: Apache-2.0
//! Ollama provider (non-streaming + NDJSON streaming).
//!
//! Implements `origin_provider::Provider` against a local Ollama daemon's
//! `POST {base}/api/chat` endpoint. Ollama is unauthenticated by default;
//! the struct holds only a base URL and an HTTP client. Streaming reuses the
//! shared `origin_provider::ndjson` line splitter.

pub mod streaming;
mod wire;

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use reqwest::StatusCode;

const DEFAULT_BASE: &str = "http://127.0.0.1:11434";

/// Ollama provider backed by `POST /api/chat`.
pub struct Ollama {
    base: String,
    client: reqwest::Client,
}

impl Ollama {
    /// Construct with the default base URL (`http://127.0.0.1:11434`).
    #[must_use]
    pub fn new() -> Self {
        Self::with_base_url(DEFAULT_BASE)
    }

    /// Construct against an arbitrary base URL (for testing and gateways).
    #[must_use]
    pub fn with_base_url(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }
}

impl Default for Ollama {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for Ollama {
    fn name(&self) -> &'static str {
        "ollama"
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let body = wire::encode_request(&req, false);
        let url = format!("{}/api/chat", self.base);
        let resp = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        if resp.status() == StatusCode::OK {
            let frame: wire::WireFrame = resp
                .json()
                .await
                .map_err(|e| ProviderError::Api(format!("decode: {e}")))?;
            return Ok(decode_frame(frame));
        }
        Err(status_error(resp).await)
    }

    async fn chat_stream(&self, req: ChatRequest, ring: &origin_stream::Ring) -> Result<(), ProviderError> {
        let body = wire::encode_request(&req, true);
        let url = format!("{}/api/chat", self.base);
        let resp = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            let err = status_error(resp).await;
            ring.close();
            return Err(err);
        }

        let result = streaming::parse_into_ring(resp, ring).await;
        ring.close();
        result
    }
}

/// Map a non-2xx `reqwest::Response` to the canonical `ProviderError` variant.
///
/// - 401 / 403 → `ProviderError::Auth`
/// - 429 → `ProviderError::RateLimit` (parses `retry-after` header, defaults to 1)
/// - other → `ProviderError::Api(format!("status {s}: {body}"))`
async fn status_error(resp: reqwest::Response) -> ProviderError {
    let status = resp.status();
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ProviderError::Auth,
        StatusCode::TOO_MANY_REQUESTS => {
            let retry = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(1);
            ProviderError::RateLimit {
                retry_after_secs: retry,
                message: String::new(),
            }
        }
        s => {
            let body = resp.text().await.unwrap_or_default();
            ProviderError::Api(format!("status {s}: {body}"))
        }
    }
}

fn decode_frame(frame: wire::WireFrame) -> ChatResponse {
    let mut blocks: Vec<Block> = Vec::new();
    if !frame.message.content.is_empty() {
        blocks.push(Block::Text {
            text: frame.message.content,
            cache_marker: None,
        });
    }
    let assistant = Message {
        role: Role::Assistant,
        blocks,
    };
    let usage = Usage {
        input_tokens: frame.prompt_eval_count,
        output_tokens: frame.eval_count,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
    };
    ChatResponse { assistant, usage }
}
