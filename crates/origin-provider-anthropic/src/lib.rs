//! Anthropic Messages API provider (non-streaming, API key auth).
//!
//! Streaming support arrives in Phase 2 (origin spec N4.4).

pub mod streaming;
mod wire;

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use reqwest::StatusCode;
use serde_json::json;

const DEFAULT_BASE: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Anthropic provider backed by the Messages API with API key authentication.
pub struct Anthropic {
    api_key: String,
    base: String,
    client: reqwest::Client,
}

impl Anthropic {
    /// Construct with the default base URL.
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE)
    }

    /// Construct against an arbitrary base URL (for testing).
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
impl Provider for Anthropic {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let wire_messages = req.messages.iter().map(message_to_wire).collect::<Vec<_>>();
        let wire_tools = req
            .tools
            .iter()
            .map(|t| wire::WireTool {
                name: &t.name,
                description: &t.description,
                input_schema: serde_json::from_str(&t.input_schema_json).unwrap_or_else(|_| json!({})),
            })
            .collect::<Vec<_>>();

        let body = wire::WireRequest {
            model: &req.model,
            max_tokens: DEFAULT_MAX_TOKENS,
            system: if req.system.is_empty() {
                None
            } else {
                Some(req.system.as_str())
            },
            messages: wire_messages,
            tools: wire_tools,
        };

        let url = format!("{}/v1/messages", self.base);
        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
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
}

fn message_to_wire(m: &Message) -> wire::WireMessage<'_> {
    let role = match m.role {
        Role::User | Role::Tool | Role::System => "user",
        // Anthropic represents tool results as user messages (Role::Tool).
        // System content goes in the top-level `system` field, not a message (Role::System).
        Role::Assistant => "assistant",
    };
    let content = m.blocks.iter().filter_map(block_to_wire).collect();
    wire::WireMessage { role, content }
}

fn block_to_wire(b: &Block) -> Option<wire::WireBlock<'_>> {
    match b {
        Block::Text { text, .. } => Some(wire::WireBlock::Text { text }),
        Block::ToolUse {
            id, name, input_json, ..
        } => {
            let input: serde_json::Value = serde_json::from_slice(input_json).unwrap_or_else(|_| json!({}));
            Some(wire::WireBlock::ToolUse { id, name, input })
        }
        Block::ToolResult {
            tool_use_id, inline, ..
        } => {
            // Borrow the inline bytes; convert to &str via str::from_utf8 with a fallback.
            let content_str: &str = inline
                .as_deref()
                .and_then(|v| std::str::from_utf8(v).ok())
                .unwrap_or("");
            Some(wire::WireBlock::ToolResult {
                tool_use_id,
                content: content_str,
                is_error: false,
            })
        }
        // Do not re-send thinking blocks; Anthropic ignores them on inbound.
        Block::Thinking { .. } => None,
    }
}

fn decode_response(wire: wire::WireResponse) -> ChatResponse {
    let blocks = wire
        .content
        .into_iter()
        .map(|b| match b {
            wire::WireRespBlock::Text { text } => Block::Text {
                text,
                cache_marker: None,
            },
            wire::WireRespBlock::ToolUse { id, name, input } => Block::ToolUse {
                id,
                name,
                input_json: serde_json::to_vec(&input).unwrap_or_default(),
                cache_marker: None,
            },
            wire::WireRespBlock::Thinking { thinking, signature } => Block::Thinking {
                tokens: thinking,
                signature,
            },
        })
        .collect();

    let assistant = Message {
        role: Role::Assistant,
        blocks,
    };
    let usage = Usage {
        input_tokens: wire.usage.input_tokens,
        output_tokens: wire.usage.output_tokens,
        cache_read_input_tokens: wire.usage.cache_read_input_tokens,
        cache_creation_input_tokens: wire.usage.cache_creation_input_tokens,
    };
    ChatResponse { assistant, usage }
}
