// SPDX-License-Identifier: Apache-2.0
//! Private wire JSON types for the Ollama `/api/chat` endpoint.
//!
//! The `Wire` prefix on every type disambiguates these internal serialisation
//! types from the public provider types they mirror.
#![allow(clippy::module_name_repetitions)]

use origin_core::types::{Block, Message, Role};
use origin_provider::ChatRequest;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Top-level request body for `POST {base}/api/chat`.
#[derive(Serialize)]
pub struct WireRequest<'a> {
    pub model: &'a str,
    pub messages: Vec<WireMessage>,
    pub stream: bool,
    /// Function tools the model may call. Omitted when empty (byte-identical to
    /// the pre-tools wire for tool-free turns).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<WireTool<'a>>,
}

/// One message in the Ollama request.
#[derive(Serialize)]
pub struct WireMessage {
    pub role: &'static str,
    pub content: String,
    /// Base64 images for multimodal (vision) models (no `data:` prefix).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
    /// Tool calls an assistant turn made, replayed so multi-turn tool
    /// conversations work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<WireToolCall>>,
}

/// `tools[]` function-tool definition (`{type:"function", function:{…}}`).
#[derive(Serialize)]
pub struct WireTool<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: WireToolFn<'a>,
}

#[derive(Serialize)]
pub struct WireToolFn<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub parameters: Value,
}

/// A tool call on the wire — used both outbound (assistant replay) and inbound
/// (model response). Ollama carries `arguments` as a JSON object, NOT a string.
#[derive(Serialize, Deserialize)]
pub struct WireToolCall {
    pub function: WireToolCallFn,
}

#[derive(Serialize, Deserialize)]
pub struct WireToolCallFn {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

/// One NDJSON streaming frame, also the shape of the non-streaming response.
#[derive(Deserialize, Default)]
pub struct WireFrame {
    #[serde(default)]
    pub message: WireFrameMessage,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub prompt_eval_count: u32,
    #[serde(default)]
    pub eval_count: u32,
}

#[derive(Deserialize, Default)]
pub struct WireFrameMessage {
    #[serde(default)]
    #[allow(dead_code)]
    pub role: String,
    #[serde(default)]
    pub content: String,
    /// Tool calls the model emitted this frame (Ollama sends them whole, not
    /// streamed token-by-token).
    #[serde(default)]
    pub tool_calls: Option<Vec<WireToolCall>>,
}

/// Encode a canonical `ChatRequest` into the Ollama wire-shape request.
#[must_use]
pub fn encode_request(req: &ChatRequest, stream: bool) -> WireRequest<'_> {
    let mut messages = Vec::with_capacity(req.messages.len() + 1);
    if !req.system.is_empty() {
        messages.push(WireMessage {
            role: "system",
            content: req.system.clone(),
            images: None,
            tool_calls: None,
        });
    }
    for m in &req.messages {
        let tool_calls = if matches!(m.role, Role::Assistant) {
            collect_tool_calls(m)
        } else {
            None
        };
        messages.push(WireMessage {
            role: role_str(m.role),
            content: collect_content(m),
            images: None,
            tool_calls,
        });
    }
    append_images(&mut messages, &req.attachments);

    let tools = req
        .tools
        .iter()
        .map(|t| WireTool {
            kind: "function",
            function: WireToolFn {
                name: &t.name,
                description: &t.description,
                parameters: serde_json::from_str(&t.input_schema_json).unwrap_or_else(|_| json!({})),
            },
        })
        .collect();

    WireRequest {
        model: &req.model,
        messages,
        stream,
        tools,
    }
}

const fn role_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn collect_content(m: &Message) -> String {
    let mut out = String::new();
    for b in &m.blocks {
        match b {
            Block::Text { text, .. } => {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
            Block::ToolResult { inline, .. } => {
                if let Some(bytes) = inline.as_deref() {
                    if let Ok(s) = std::str::from_utf8(bytes) {
                        if !out.is_empty() {
                            out.push('\n');
                        }
                        out.push_str(s);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Collect an assistant turn's `tool_use` blocks as Ollama `tool_calls`.
fn collect_tool_calls(m: &Message) -> Option<Vec<WireToolCall>> {
    let calls: Vec<WireToolCall> = m
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::ToolUse { name, input_json, .. } => Some(WireToolCall {
                function: WireToolCallFn {
                    name: name.clone(),
                    arguments: serde_json::from_slice(input_json).unwrap_or(Value::Null),
                },
            }),
            _ => None,
        })
        .collect();
    (!calls.is_empty()).then_some(calls)
}

/// Append request-level image attachments (base64, no `data:` prefix) to the
/// last user message, for vision models.
fn append_images(messages: &mut [WireMessage], attachments: &[origin_multimodal::ContentBlock]) {
    let imgs: Vec<String> = attachments.iter().filter_map(|b| b.base64.clone()).collect();
    if imgs.is_empty() {
        return;
    }
    if let Some(last_user) = messages.iter_mut().rev().find(|m| m.role == "user") {
        last_user.images = Some(imgs);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use origin_provider::ToolSchema;

    #[test]
    fn encodes_tools_images_and_assistant_tool_calls() {
        let req = ChatRequest {
            system: String::new(),
            messages: vec![
                Message::new(Role::User).with_block(Block::text("what's the weather?")),
                Message {
                    role: Role::Assistant,
                    blocks: vec![Block::ToolUse {
                        id: "call_get_weather_0".into(),
                        name: "get_weather".into(),
                        input_json: br#"{"city":"Paris"}"#.to_vec(),
                        cache_marker: None,
                    }],
                },
            ],
            model: "llama3.2".into(),
            tools: vec![ToolSchema {
                name: "get_weather".into(),
                description: "Current weather".into(),
                input_schema_json: r#"{"type":"object","properties":{"city":{"type":"string"}}}"#.into(),
            }],
            effort: None,
            thinking_tokens: None,
            attachments: vec![origin_multimodal::ContentBlock::image("image/png", "QUJD")],
        };
        let body = serde_json::to_value(encode_request(&req, false)).unwrap();

        // Tools advertised in OpenAI-ish function shape.
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "get_weather");
        // Image attached to the (last) user message as base64 (no data: prefix).
        assert_eq!(body["messages"][0]["images"][0], "QUJD");
        // Assistant tool call replayed with arguments as a JSON OBJECT.
        let tc = &body["messages"][1]["tool_calls"][0];
        assert_eq!(tc["function"]["name"], "get_weather");
        assert_eq!(tc["function"]["arguments"]["city"], "Paris");
    }

    #[test]
    fn decodes_model_tool_calls() {
        let frame: WireFrame = serde_json::from_str(
            r#"{"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"get_weather","arguments":{"city":"Paris"}}}]},"done":true}"#,
        )
        .unwrap();
        let calls = frame.message.tool_calls.expect("tool_calls present");
        assert_eq!(calls[0].function.name, "get_weather");
        assert_eq!(calls[0].function.arguments["city"], "Paris");
    }
}
