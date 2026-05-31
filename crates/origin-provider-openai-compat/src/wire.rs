// SPDX-License-Identifier: Apache-2.0
//! Private wire JSON types for the `OpenAI` Chat Completions API.
//!
//! The `Wire` prefix on every type disambiguates these internal serialisation
//! types from the public provider types they mirror.
#![allow(clippy::module_name_repetitions)]

use origin_core::types::{Block, Message, Role};
use origin_provider::openai_tools::WireToolCall;
use origin_provider::ChatRequest;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Top-level request body for `POST /v1/chat/completions`.
#[derive(Serialize)]
pub struct WireRequest<'a> {
    pub model: &'a str,
    pub messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<WireTool<'a>>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stream: bool,
    /// Only emitted when `stream=true`. Asks the server to include a final
    /// `usage` SSE frame with token counts (otherwise omitted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<WireStreamOptions>,
    /// Reasoning-effort hint (`reasoning_effort` on the wire). `None` is omitted,
    /// keeping the request byte-identical to the pre-effort behavior.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<&'static str>,
}

/// `stream_options` block on the streaming request body.
#[derive(Serialize)]
pub struct WireStreamOptions {
    pub include_usage: bool,
}

/// One message in the `OpenAI` request.
#[derive(Serialize)]
pub struct WireMessage {
    pub role: &'static str,
    /// `content` is either a string or null when only `tool_calls` are present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<WireOutToolCall>>,
}

/// Outgoing tool call (assistant → server in a follow-up turn).
#[derive(Serialize)]
pub struct WireOutToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: WireOutToolFunction,
}

#[derive(Serialize)]
pub struct WireOutToolFunction {
    pub name: String,
    pub arguments: String,
}

/// Tool advertisement in the request body.
#[derive(Serialize)]
pub struct WireTool<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: WireToolFunctionSchema<'a>,
}

#[derive(Serialize)]
pub struct WireToolFunctionSchema<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub parameters: Value,
}

/// Non-streaming response body.
#[derive(Deserialize)]
pub struct WireResponse {
    pub choices: Vec<WireChoice>,
    #[serde(default)]
    pub usage: WireUsage,
}

#[derive(Deserialize)]
pub struct WireChoice {
    pub message: WireRespMessage,
    #[serde(default)]
    #[allow(dead_code)]
    pub finish_reason: Option<String>,
}

#[derive(Deserialize)]
pub struct WireRespMessage {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<WireToolCall>>,
}

#[derive(Deserialize, Default)]
pub struct WireUsage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
}

/// Encode a `ChatRequest` into the `OpenAI` wire-shape request.
///
/// `ChatRequest.thinking_tokens` is intentionally **not** mapped here: the
/// explicit extended-thinking `budget_tokens` control is an Anthropic Messages
/// API concept with no `OpenAI` Chat Completions equivalent (`OpenAI`-style
/// reasoning models expose only the coarse `reasoning_effort` knob, which is
/// already wired from `ChatRequest.effort`). The field is therefore a no-op for
/// `OpenAI`-compatible backends, leaving the body byte-identical whether or not
/// a thinking budget was requested.
#[must_use]
pub fn encode_request(req: &ChatRequest, stream: bool) -> WireRequest<'_> {
    let mut messages = Vec::with_capacity(req.messages.len() + 1);
    if !req.system.is_empty() {
        messages.push(WireMessage {
            role: "system",
            content: Some(req.system.clone()),
            tool_call_id: None,
            tool_calls: None,
        });
    }
    for m in &req.messages {
        encode_message_into(m, &mut messages);
    }

    let tools = req
        .tools
        .iter()
        .map(|t| WireTool {
            kind: "function",
            function: WireToolFunctionSchema {
                name: &t.name,
                description: &t.description,
                parameters: serde_json::from_str(&t.input_schema_json).unwrap_or_else(|_| json!({})),
            },
        })
        .collect();

    WireRequest {
        model: &req.model,
        messages,
        tools,
        stream,
        stream_options: if stream {
            Some(WireStreamOptions { include_usage: true })
        } else {
            None
        },
        reasoning_effort: req.effort.map(origin_provider::ReasoningEffort::as_wire_str),
    }
}

fn encode_message_into(m: &Message, out: &mut Vec<WireMessage>) {
    match m.role {
        Role::System => {
            let text = collect_text(&m.blocks);
            out.push(WireMessage {
                role: "system",
                content: Some(text),
                tool_call_id: None,
                tool_calls: None,
            });
        }
        Role::User => {
            let text = collect_text(&m.blocks);
            out.push(WireMessage {
                role: "user",
                content: Some(text),
                tool_call_id: None,
                tool_calls: None,
            });
        }
        Role::Assistant => {
            let text = collect_text(&m.blocks);
            let tool_calls: Vec<WireOutToolCall> = m
                .blocks
                .iter()
                .filter_map(|b| match b {
                    Block::ToolUse {
                        id, name, input_json, ..
                    } => Some(WireOutToolCall {
                        id: id.clone(),
                        kind: "function",
                        function: WireOutToolFunction {
                            name: name.clone(),
                            arguments: String::from_utf8(input_json.clone()).unwrap_or_default(),
                        },
                    }),
                    _ => None,
                })
                .collect();
            let content = if text.is_empty() { None } else { Some(text) };
            let tc = if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            };
            out.push(WireMessage {
                role: "assistant",
                content,
                tool_call_id: None,
                tool_calls: tc,
            });
        }
        Role::Tool => {
            // OpenAI requires one `{role:"tool"}` message per tool_call_id.
            for b in &m.blocks {
                if let Block::ToolResult {
                    tool_use_id, inline, ..
                } = b
                {
                    let content_str = inline
                        .as_deref()
                        .and_then(|v| std::str::from_utf8(v).ok())
                        .unwrap_or("")
                        .to_string();
                    out.push(WireMessage {
                        role: "tool",
                        content: Some(content_str),
                        tool_call_id: Some(tool_use_id.clone()),
                        tool_calls: None,
                    });
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
