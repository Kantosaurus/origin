//! Generic `OpenAI` Chat-Completions wire client.
//!
//! Parameterised over base URL, chat path, auth header, and extra headers so
//! it can serve every OpenAI-shape provider in the catalog from a single
//! implementation.

pub mod streaming;
mod token_source;
pub mod wire;

pub use token_source::{NoAuth, StaticBearer, StaticHeader, TokenSource};

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_provider::openai_tools::tool_call_to_block;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use reqwest::StatusCode;
use std::sync::Arc;

#[derive(Clone)]
pub struct OpenAiCompatConfig {
    pub name: &'static str,
    pub base_url: String,
    pub chat_path: String,
    pub auth: Arc<dyn TokenSource>,
    pub extra_headers: Vec<(String, String)>,
}

pub struct OpenAiCompat {
    cfg: OpenAiCompatConfig,
    client: reqwest::Client,
    cas: Option<Arc<origin_cas::Store>>,
}

impl OpenAiCompat {
    #[must_use]
    pub fn new(cfg: OpenAiCompatConfig) -> Self {
        Self {
            cfg,
            client: reqwest::Client::new(),
            cas: None,
        }
    }

    /// Attach the content-addressed store so handle-backed `ToolResult` blocks
    /// (produced by the daemon whenever a CAS is configured) are inflated to
    /// inline bytes before wire encoding. Without this, follow-up turns send
    /// empty tool results and the agentic loop silently breaks.
    #[must_use]
    pub fn with_cas(mut self, cas: Arc<origin_cas::Store>) -> Self {
        self.cas = Some(cas);
        self
    }
}

#[async_trait]
impl Provider for OpenAiCompat {
    fn name(&self) -> &'static str {
        self.cfg.name
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let messages = origin_provider::inflate_tool_result_handles(&req.messages, self.cas.as_ref())?;
        let req = ChatRequest { messages, ..req };
        let body = wire::encode_request(&req, false);
        let url = format!(
            "{}{}",
            self.cfg.base_url.trim_end_matches('/'),
            self.cfg.chat_path
        );
        let mut builder = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body);
        let (hdr, val) = self.cfg.auth.header().await.map_err(|_| ProviderError::Auth)?;
        if !hdr.is_empty() {
            builder = builder.header(hdr, val);
        }
        for (h, v) in &self.cfg.extra_headers {
            builder = builder.header(h, v);
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        if resp.status() == StatusCode::OK {
            let wire: wire::WireResponse = resp
                .json()
                .await
                .map_err(|e| ProviderError::Api(format!("decode: {e}")))?;
            return Ok(decode_response(wire));
        }
        Err(status_error(resp).await)
    }

    async fn chat_stream(&self, req: ChatRequest, ring: &origin_stream::Ring) -> Result<(), ProviderError> {
        let messages = origin_provider::inflate_tool_result_handles(&req.messages, self.cas.as_ref())?;
        let req = ChatRequest { messages, ..req };
        let body = wire::encode_request(&req, true);
        let url = format!(
            "{}{}",
            self.cfg.base_url.trim_end_matches('/'),
            self.cfg.chat_path
        );
        let mut builder = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(&body);
        let (hdr, val) = self.cfg.auth.header().await.map_err(|_| ProviderError::Auth)?;
        if !hdr.is_empty() {
            builder = builder.header(hdr, val);
        }
        for (h, v) in &self.cfg.extra_headers {
            builder = builder.header(h, v);
        }
        let resp = builder
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

fn decode_response(wire: wire::WireResponse) -> ChatResponse {
    let mut blocks: Vec<Block> = Vec::new();
    if let Some(choice) = wire.choices.into_iter().next() {
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
    ChatResponse {
        assistant: Message {
            role: Role::Assistant,
            blocks,
        },
        usage: Usage {
            input_tokens: wire.usage.prompt_tokens,
            output_tokens: wire.usage.completion_tokens,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        },
    }
}
