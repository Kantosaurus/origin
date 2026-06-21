// SPDX-License-Identifier: Apache-2.0
//! `OpenAI` **Responses API** wire client (`POST /responses`).
//!
//! The Responses API is a DISTINCT surface from Chat-Completions:
//!   * the request carries typed `input[]` items + a top-level `instructions`
//!     string (NOT a `messages[]` array), and
//!   * the response returns an `output[]` array of typed items (NOT `choices[]`).
//!
//! Codex / `ChatGPT`-OAuth providers speak only this surface, so routing them
//! through the Chat-Completions [`crate::OpenAiCompat`] (which emits `{messages}`
//! and reads `choices[]`) 400s / mis-parses on every turn. This module provides
//! a dedicated [`OpenAiResponses`] provider whose `chat()` encodes/decodes the
//! Responses shape. Streaming is served by the [`origin_provider::Provider`]
//! trait's default `chat_stream` (which buffers `chat()` into the ring), so the
//! agentic loop works end-to-end without re-implementing the (entirely different)
//! Responses SSE event model.
#![allow(clippy::module_name_repetitions)]

use std::sync::Arc;

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::OpenAiCompatConfig;

/// Provider speaking the `OpenAI` Responses API (`/responses`).
///
/// Reuses [`OpenAiCompatConfig`] (base URL, chat path, auth, extra headers) so
/// the factory can build it the same way it builds the Chat-Completions client.
pub struct OpenAiResponses {
    cfg: OpenAiCompatConfig,
    client: reqwest::Client,
    cas: Option<Arc<origin_cas::Store>>,
}

impl OpenAiResponses {
    #[must_use]
    pub fn new(cfg: OpenAiCompatConfig) -> Self {
        Self {
            cfg,
            client: reqwest::Client::new(),
            cas: None,
        }
    }

    /// Attach the content-addressed store so handle-backed `ToolResult` blocks
    /// are inflated to inline bytes before wire encoding (mirrors `OpenAiCompat`).
    #[must_use]
    pub fn with_cas(mut self, cas: Arc<origin_cas::Store>) -> Self {
        self.cas = Some(cas);
        self
    }
}

#[async_trait]
impl Provider for OpenAiResponses {
    fn name(&self) -> &'static str {
        self.cfg.name
    }

    fn base_url(&self) -> Option<&str> {
        Some(&self.cfg.base_url)
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let messages = origin_provider::inflate_tool_result_handles(&req.messages, self.cas.as_ref())?;
        let req = ChatRequest { messages, ..req };
        let body = serde_json::to_value(encode_request(&req))
            .map_err(|e| ProviderError::Api(format!("encode: {e}")))?;
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
            let wire: WireResponse = resp
                .json()
                .await
                .map_err(|e| ProviderError::Api(format!("decode: {e}")))?;
            return Ok(decode_response(wire));
        }
        Err(status_error(resp, &url).await)
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
            let safe_url = origin_shimquirks::redact_url_secrets(url);
            ProviderError::Api(format!("status {s} ({safe_url}): {body}"))
        }
    }
}

// ── Request wire types ──────────────────────────────────────────────────────

/// Top-level request body for `POST /responses`.
#[derive(Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    /// Top-level system prompt (the Responses API's replacement for a
    /// `{role:"system"}` message). Omitted when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    /// Typed input items (messages, function calls, function-call outputs).
    input: Vec<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<WireTool<'a>>,
    stream: bool,
}

/// Tool advertisement. The Responses API flattens the function schema to the top
/// level of the tool object (no nested `function` wrapper, unlike Chat).
#[derive(Serialize)]
struct WireTool<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    name: &'a str,
    description: &'a str,
    parameters: Value,
}

/// Encode a [`ChatRequest`] into the Responses-API request body.
#[must_use]
fn encode_request(req: &ChatRequest) -> WireRequest<'_> {
    let mut input: Vec<Value> = Vec::with_capacity(req.messages.len());
    for m in &req.messages {
        encode_message_into(m, &mut input);
    }

    let tools = req
        .tools
        .iter()
        .map(|t| WireTool {
            kind: "function",
            name: &t.name,
            description: &t.description,
            parameters: serde_json::from_str(&t.input_schema_json).unwrap_or_else(|_| json!({})),
        })
        .collect();

    WireRequest {
        model: &req.model,
        instructions: if req.system.is_empty() {
            None
        } else {
            Some(req.system.clone())
        },
        input,
        tools,
        stream: false,
    }
}

/// Translate one canonical [`Message`] into zero-or-more Responses input items.
fn encode_message_into(m: &Message, out: &mut Vec<Value>) {
    match m.role {
        Role::System => {
            // A system message that slipped into the history is mapped to a
            // `developer`-role message item (the Responses-API analogue); the
            // primary system prompt rides on `instructions`.
            let text = collect_text(&m.blocks);
            out.push(json!({
                "type": "message",
                "role": "developer",
                "content": [{ "type": "input_text", "text": text }],
            }));
        }
        Role::User => {
            let text = collect_text(&m.blocks);
            out.push(json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": text }],
            }));
        }
        Role::Assistant => {
            let text = collect_text(&m.blocks);
            if !text.is_empty() {
                out.push(json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": text }],
                }));
            }
            // Each assistant tool call becomes a top-level `function_call` item.
            for b in &m.blocks {
                if let Block::ToolUse {
                    id, name, input_json, ..
                } = b
                {
                    out.push(json!({
                        "type": "function_call",
                        "call_id": id,
                        "name": name,
                        "arguments": String::from_utf8(input_json.clone()).unwrap_or_default(),
                    }));
                }
            }
        }
        Role::Tool => {
            // Each tool result becomes a `function_call_output` item keyed by the
            // originating call id.
            for b in &m.blocks {
                if let Block::ToolResult {
                    tool_use_id, inline, ..
                } = b
                {
                    let output = inline
                        .as_deref()
                        .and_then(|v| std::str::from_utf8(v).ok())
                        .unwrap_or("")
                        .to_string();
                    out.push(json!({
                        "type": "function_call_output",
                        "call_id": tool_use_id,
                        "output": output,
                    }));
                }
            }
        }
    }
}

fn collect_text(blocks: &[Block]) -> String {
    let mut out = String::new();
    for b in blocks {
        if let Block::Text { text, .. } = b {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
    }
    out
}

// ── Response wire types ─────────────────────────────────────────────────────

/// Non-streaming `/responses` response body.
#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    output: Vec<WireOutputItem>,
    #[serde(default)]
    usage: WireUsage,
}

/// One typed item in the `output[]` array.
///
/// We model the two shapes the agentic loop consumes — assistant `message`
/// (carrying `output_text` content parts) and `function_call` (a tool call).
/// Unknown item types deserialize with empty fields and are skipped on decode.
#[derive(Deserialize, Default)]
struct WireOutputItem {
    #[serde(rename = "type", default)]
    kind: String,
    // message item:
    #[serde(default)]
    content: Vec<WireContentPart>,
    // function_call item:
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize, Default)]
struct WireContentPart {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize, Default)]
struct WireUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

/// Decode a Responses-API body into the canonical [`ChatResponse`].
fn decode_response(wire: WireResponse) -> ChatResponse {
    let mut blocks: Vec<Block> = Vec::new();
    for item in wire.output {
        match item.kind.as_str() {
            "message" => {
                let mut text = String::new();
                for part in &item.content {
                    // `output_text` is the assistant text part; tolerate other
                    // part kinds by only collecting text-bearing ones.
                    if part.kind == "output_text" || (part.kind.is_empty() && !part.text.is_empty()) {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(&part.text);
                    }
                }
                if !text.is_empty() {
                    blocks.push(Block::Text {
                        text,
                        cache_marker: None,
                    });
                }
            }
            "function_call" => {
                let id = item.call_id.clone().unwrap_or_default();
                let name = item.name.clone().unwrap_or_default();
                let arguments = item.arguments.clone().unwrap_or_default();
                blocks.push(Block::ToolUse {
                    id,
                    name,
                    input_json: arguments.into_bytes(),
                    cache_marker: None,
                });
            }
            _ => {}
        }
    }
    ChatResponse {
        assistant: Message {
            role: Role::Assistant,
            blocks,
        },
        usage: Usage {
            input_tokens: wire.usage.input_tokens,
            output_tokens: wire.usage.output_tokens,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{decode_response, encode_request, WireResponse};
    use origin_core::types::{Block, Message, Role};
    use origin_provider::{ChatRequest, ToolSchema};
    use serde_json::json;

    fn user_msg(text: &str) -> Message {
        Message {
            role: Role::User,
            blocks: vec![Block::text(text)],
        }
    }

    #[test]
    fn request_uses_input_items_and_instructions_not_messages() {
        let req = ChatRequest {
            system: "you are codex".to_string(),
            messages: vec![user_msg("hello")],
            model: "gpt-5-codex".to_string(),
            ..ChatRequest::default()
        };
        let body = serde_json::to_value(encode_request(&req)).unwrap();
        // Responses shape: `input` + `instructions`, NOT chat-completions `messages`.
        assert!(
            body.get("messages").is_none(),
            "must not emit a chat `messages` array"
        );
        assert_eq!(body["instructions"], json!("you are codex"));
        let input = body["input"].as_array().expect("input array");
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], json!("message"));
        assert_eq!(input[0]["role"], json!("user"));
        assert_eq!(input[0]["content"][0]["type"], json!("input_text"));
        assert_eq!(input[0]["content"][0]["text"], json!("hello"));
        assert_eq!(body["model"], json!("gpt-5-codex"));
    }

    #[test]
    fn request_flattens_tool_schema_to_top_level() {
        let req = ChatRequest {
            messages: vec![user_msg("x")],
            model: "gpt-5-codex".to_string(),
            tools: vec![ToolSchema {
                name: "search".to_string(),
                description: "find things".to_string(),
                input_schema_json: r#"{"type":"object"}"#.to_string(),
            }],
            ..ChatRequest::default()
        };
        let body = serde_json::to_value(encode_request(&req)).unwrap();
        let tool = &body["tools"][0];
        assert_eq!(tool["type"], json!("function"));
        // Responses API: name/description/parameters at top level (no nested
        // `function` wrapper like Chat-Completions uses).
        assert_eq!(tool["name"], json!("search"));
        assert!(tool.get("function").is_none());
        assert_eq!(tool["parameters"]["type"], json!("object"));
    }

    #[test]
    fn request_encodes_tool_call_and_result_round_trip() {
        let assistant = Message {
            role: Role::Assistant,
            blocks: vec![Block::ToolUse {
                id: "call_1".to_string(),
                name: "search".to_string(),
                input_json: br#"{"q":"rust"}"#.to_vec(),
                cache_marker: None,
            }],
        };
        let tool_result = Message {
            role: Role::Tool,
            blocks: vec![Block::ToolResult {
                tool_use_id: "call_1".to_string(),
                handle: None,
                inline: Some(b"42 results".to_vec()),
                cache_marker: None,
                is_error: false,
            }],
        };
        let req = ChatRequest {
            messages: vec![user_msg("find rust"), assistant, tool_result],
            model: "gpt-5-codex".to_string(),
            ..ChatRequest::default()
        };
        let body = serde_json::to_value(encode_request(&req)).unwrap();
        let input = body["input"].as_array().unwrap();
        // user message, function_call, function_call_output.
        let kinds: Vec<&str> = input.iter().map(|i| i["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["message", "function_call", "function_call_output"]);
        assert_eq!(input[1]["call_id"], json!("call_1"));
        assert_eq!(input[1]["name"], json!("search"));
        assert_eq!(input[2]["call_id"], json!("call_1"));
        assert_eq!(input[2]["output"], json!("42 results"));
    }

    #[test]
    fn decode_reads_output_items_not_choices() {
        // A real Responses body returns `output[]`, never `choices[]`.
        let wire: WireResponse = serde_json::from_value(json!({
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "hi there" }]
                }
            ],
            "usage": { "input_tokens": 11, "output_tokens": 3 }
        }))
        .unwrap();
        let resp = decode_response(wire);
        assert_eq!(resp.usage.input_tokens, 11);
        assert_eq!(resp.usage.output_tokens, 3);
        assert_eq!(resp.assistant.role, Role::Assistant);
        match &resp.assistant.blocks[0] {
            Block::Text { text, .. } => assert_eq!(text, "hi there"),
            other => panic!("expected text block, got {other:?}"),
        }
    }

    #[test]
    fn decode_recovers_function_call_tool_use() {
        let wire: WireResponse = serde_json::from_value(json!({
            "output": [
                {
                    "type": "function_call",
                    "call_id": "call_9",
                    "name": "search",
                    "arguments": "{\"q\":\"x\"}"
                }
            ]
        }))
        .unwrap();
        let resp = decode_response(wire);
        match &resp.assistant.blocks[0] {
            Block::ToolUse {
                id, name, input_json, ..
            } => {
                assert_eq!(id, "call_9");
                assert_eq!(name, "search");
                assert_eq!(String::from_utf8(input_json.clone()).unwrap(), r#"{"q":"x"}"#);
            }
            other => panic!("expected tool use, got {other:?}"),
        }
    }
}
