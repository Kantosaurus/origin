// SPDX-License-Identifier: Apache-2.0
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
        // Encode the typed wire request, then drop to a JSON value so the
        // backend-specific quirk pass can remap the model alias and strip fields
        // the detected backend cannot accept (e.g. `store` for vLLM/Cerebras).
        // A no-op for OpenAi/Other, so canonical OpenAI requests are unchanged.
        let mut body = serde_json::to_value(wire::encode_request(&req, false))
            .map_err(|e| ProviderError::Api(format!("encode: {e}")))?;
        // Inject multimodal attachments (item G-live). No-op when empty, so the
        // default text-only request is byte-identical (item G-live).
        append_attachments(&mut body, &req.attachments);
        apply_shim_quirks(&self.cfg.base_url, &mut body);
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
        Err(status_error(resp, &url).await)
    }

    async fn chat_stream(&self, req: ChatRequest, ring: &origin_stream::Ring) -> Result<(), ProviderError> {
        let messages = origin_provider::inflate_tool_result_handles(&req.messages, self.cas.as_ref())?;
        let req = ChatRequest { messages, ..req };
        let mut body = serde_json::to_value(wire::encode_request(&req, true))
            .map_err(|e| ProviderError::Api(format!("encode: {e}")))?;
        apply_shim_quirks(&self.cfg.base_url, &mut body);
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
            let err = status_error(resp, &url).await;
            ring.close();
            return Err(err);
        }
        let result = streaming::parse_into_ring(resp, ring).await;
        ring.close();
        result
    }
}

async fn status_error(resp: reqwest::Response, url: &str) -> ProviderError {
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
            // Redact any credential query params / inline userinfo from the
            // endpoint before it lands in an error string a caller may log.
            let safe_url = origin_shimquirks::redact_url_secrets(url);
            ProviderError::Api(format!("status {s} ({safe_url}): {body}"))
        }
    }
}

/// Append multimodal attachments to the last user message (item G-live).
///
/// `OpenAI` multimodal messages carry `content` as an array of typed parts. When
/// attachments are present, the last `{"role":"user"}` message's string content
/// is promoted to a `[{"type":"text",...}]` array and the encoded image/text
/// parts (via [`origin_multimodal::encode_openai_block`]) are appended. A no-op
/// when `attachments` is empty, so the canonical text request is byte-identical.
/// When there is no user message, a fresh one carrying the parts is appended.
fn append_attachments(body: &mut serde_json::Value, attachments: &[origin_multimodal::ContentBlock]) {
    if attachments.is_empty() {
        return;
    }
    let encoded: Vec<serde_json::Value> = attachments
        .iter()
        .map(origin_multimodal::encode_openai_block)
        .collect();
    let Some(messages) = body.get_mut("messages").and_then(serde_json::Value::as_array_mut) else {
        return;
    };
    let last_user = messages
        .iter_mut()
        .rev()
        .find(|m| m.get("role").and_then(serde_json::Value::as_str) == Some("user"));
    if let Some(msg) = last_user {
        // Promote existing string content to a text part, then append the
        // encoded attachment parts.
        let mut parts: Vec<serde_json::Value> = match msg.get("content") {
            Some(serde_json::Value::String(s)) => {
                vec![serde_json::json!({ "type": "text", "text": s })]
            }
            Some(serde_json::Value::Array(arr)) => arr.clone(),
            _ => Vec::new(),
        };
        parts.extend(encoded);
        if let Some(obj) = msg.as_object_mut() {
            obj.insert("content".to_string(), serde_json::Value::Array(parts));
        }
        return;
    }
    messages.push(serde_json::json!({ "role": "user", "content": encoded }));
}

/// Apply backend-specific request quirks to an already-encoded request body.
///
/// Detects the concrete backend from `base_url`, remaps the `model` field to the
/// alias that backend expects, then strips fields the backend does not
/// understand via [`origin_shimquirks::apply_request_quirks`]. For
/// [`origin_shimquirks::Backend::OpenAi`] and `Backend::Other` this is a no-op,
/// so a canonical `OpenAI` request is left byte-identical.
fn apply_shim_quirks(base_url: &str, body: &mut serde_json::Value) {
    let backend = origin_shimquirks::Backend::from_base_url(base_url);
    if let Some(model) = body.get("model").and_then(serde_json::Value::as_str) {
        let mapped = origin_shimquirks::map_model_name(backend, model);
        if mapped != model {
            if let Some(obj) = body.as_object_mut() {
                obj.insert("model".to_string(), serde_json::Value::String(mapped));
            }
        }
    }
    origin_shimquirks::apply_request_quirks(backend, body);
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
