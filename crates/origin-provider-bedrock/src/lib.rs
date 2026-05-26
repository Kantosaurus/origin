//! AWS Bedrock provider — `SigV4`-signed `InvokeModel` against an Anthropic-shape body.
//!
//! `POST {endpoint}/model/{model_id}/invoke` with the Anthropic
//! `bedrock-2023-05-31` JSON shape (system + messages + `max_tokens`). Each
//! request is signed with `SigV4` (`service = "bedrock"`, `region` from the
//! constructor); streaming falls back to the trait default in this phase.
#![allow(clippy::module_name_repetitions)]

mod sigv4;

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;

const ANTHROPIC_VERSION: &str = "bedrock-2023-05-31";
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// AWS Bedrock provider backed by `POST /model/{model_id}/invoke`.
pub struct Bedrock {
    endpoint: String,
    region: String,
    model_id: String,
    access_key: String,
    secret_key: String,
    client: reqwest::Client,
}

impl Bedrock {
    /// Construct with explicit endpoint, region, model id, and static credentials.
    #[must_use]
    pub fn new(
        endpoint: impl Into<String>,
        region: impl Into<String>,
        model_id: impl Into<String>,
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            region: region.into(),
            model_id: model_id.into(),
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Provider for Bedrock {
    fn name(&self) -> &'static str {
        "bedrock"
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let url = format!("{}/model/{}/invoke", self.endpoint, self.model_id);
        let body = encode_request(&req);
        let body_bytes = serde_json::to_vec(&body).map_err(|e| ProviderError::Api(format!("encode: {e}")))?;

        let extra_headers = sigv4::signed_headers(
            "POST",
            &url,
            &body_bytes,
            &self.region,
            &self.access_key,
            &self.secret_key,
        )
        .map_err(ProviderError::Api)?;

        let mut builder = self.client.post(&url).header("content-type", "application/json");
        for (name, value) in extra_headers {
            builder = builder.header(name, value);
        }
        let resp = builder
            .body(body_bytes)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        if resp.status() == StatusCode::OK {
            let wire: WireResponse = resp
                .json()
                .await
                .map_err(|e| ProviderError::Api(format!("decode: {e}")))?;
            return Ok(decode_response(wire));
        }
        Err(status_error(resp).await)
    }
}

/// Map a non-2xx `reqwest::Response` to the canonical `ProviderError` variant.
///
/// - 401 / 403 → `ProviderError::Auth`
/// - 429 → `ProviderError::RateLimit` (parses `retry-after`, defaults to 1)
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

fn decode_response(wire: WireResponse) -> ChatResponse {
    let blocks = wire
        .content
        .into_iter()
        .map(|b| match b {
            WireRespBlock::Text { text } => Block::Text {
                text,
                cache_marker: None,
            },
            WireRespBlock::ToolUse { id, name, input } => Block::ToolUse {
                id,
                name,
                input_json: serde_json::to_vec(&input).unwrap_or_default(),
                cache_marker: None,
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
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
    };
    ChatResponse { assistant, usage }
}

// ── wire types ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct WireRequest<'a> {
    anthropic_version: &'static str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    messages: Vec<WireMessage<'a>>,
}

#[derive(Serialize)]
struct WireMessage<'a> {
    role: &'static str,
    content: Vec<WireBlock<'a>>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireBlock<'a> {
    Text {
        text: &'a str,
    },
    ToolUse {
        id: &'a str,
        name: &'a str,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: &'a str,
        content: &'a str,
        is_error: bool,
    },
}

#[derive(Deserialize)]
struct WireResponse {
    content: Vec<WireRespBlock>,
    #[serde(default)]
    usage: WireUsage,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireRespBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

#[derive(Deserialize, Default)]
struct WireUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

fn encode_request(req: &ChatRequest) -> WireRequest<'_> {
    let mut messages = Vec::with_capacity(req.messages.len());
    for m in &req.messages {
        encode_message_into(m, &mut messages);
    }
    WireRequest {
        anthropic_version: ANTHROPIC_VERSION,
        max_tokens: DEFAULT_MAX_TOKENS,
        system: if req.system.is_empty() {
            None
        } else {
            Some(req.system.as_str())
        },
        messages,
    }
}

fn encode_message_into<'a>(m: &'a Message, out: &mut Vec<WireMessage<'a>>) {
    let role = match m.role {
        Role::Assistant => "assistant",
        // Anthropic-on-Bedrock represents tool results as user messages, and the
        // system prompt is carried in the top-level `system` field — not a message.
        Role::User | Role::Tool | Role::System => "user",
    };
    let content = m.blocks.iter().filter_map(block_to_wire).collect();
    out.push(WireMessage { role, content });
}

fn block_to_wire(b: &Block) -> Option<WireBlock<'_>> {
    match b {
        Block::Text { text, .. } => Some(WireBlock::Text { text }),
        Block::ToolUse {
            id, name, input_json, ..
        } => {
            let input: serde_json::Value = serde_json::from_slice(input_json).unwrap_or_else(|_| json!({}));
            Some(WireBlock::ToolUse { id, name, input })
        }
        Block::ToolResult {
            tool_use_id, inline, ..
        } => {
            let content_str: &str = inline
                .as_deref()
                .and_then(|v| std::str::from_utf8(v).ok())
                .unwrap_or("");
            Some(WireBlock::ToolResult {
                tool_use_id,
                content: content_str,
                is_error: false,
            })
        }
        Block::Thinking { .. } => None,
    }
}
