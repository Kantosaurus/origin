//! Private wire JSON types for the Gemini `generateContent` API.
//!
//! The `Wire` prefix on every type disambiguates these internal serialisation
//! types from the canonical provider types they mirror.
#![allow(clippy::module_name_repetitions)]

use origin_core::types::{Block, Message, Role};
use origin_provider::ChatRequest;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Top-level request body.
#[derive(Serialize)]
pub struct WireRequest<'a> {
    pub contents: Vec<WireContent<'a>>,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<WireContent<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<WireToolGroup<'a>>,
}

/// One `contents` entry. Used for both user/model turns and `systemInstruction`.
#[derive(Serialize)]
pub struct WireContent<'a> {
    pub role: &'static str,
    pub parts: Vec<WirePartOut<'a>>,
}

/// A single outgoing `parts` entry — text, functionCall, or functionResponse.
#[derive(Serialize)]
#[serde(untagged)]
pub enum WirePartOut<'a> {
    Text {
        text: String,
    },
    FunctionCall {
        #[serde(rename = "functionCall")]
        function_call: WireFnCallOut<'a>,
    },
    FunctionResponse {
        #[serde(rename = "functionResponse")]
        function_response: WireFnResponseOut<'a>,
    },
}

#[derive(Serialize)]
pub struct WireFnCallOut<'a> {
    pub name: &'a str,
    pub args: Value,
}

#[derive(Serialize)]
pub struct WireFnResponseOut<'a> {
    pub name: &'a str,
    pub response: Value,
}

/// `tools: [{ functionDeclarations: [...] }]`.
#[derive(Serialize)]
pub struct WireToolGroup<'a> {
    #[serde(rename = "functionDeclarations")]
    pub function_declarations: Vec<WireFnDecl<'a>>,
}

#[derive(Serialize)]
pub struct WireFnDecl<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub parameters: Value,
}

// ---------- Response ----------

/// Non-streaming response body.
#[derive(Deserialize)]
pub struct WireResponse {
    #[serde(default)]
    pub candidates: Vec<WireCandidate>,
    #[serde(rename = "usageMetadata", default)]
    pub usage_metadata: WireUsage,
}

#[derive(Deserialize)]
pub struct WireCandidate {
    pub content: WireRespContent,
    #[serde(rename = "finishReason", default)]
    #[allow(dead_code)]
    pub finish_reason: Option<String>,
}

#[derive(Deserialize)]
pub struct WireRespContent {
    #[serde(default)]
    pub parts: Vec<WirePartIn>,
}

#[derive(Deserialize)]
pub struct WirePartIn {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(rename = "functionCall", default)]
    pub function_call: Option<WireFnCallIn>,
}

#[derive(Deserialize)]
pub struct WireFnCallIn {
    pub name: String,
    #[serde(default)]
    pub args: Value,
}

#[derive(Deserialize, Default)]
#[allow(clippy::struct_field_names)]
pub struct WireUsage {
    #[serde(rename = "promptTokenCount", default)]
    pub prompt_token_count: u32,
    #[serde(rename = "candidatesTokenCount", default)]
    pub candidates_token_count: u32,
    #[serde(rename = "cachedContentTokenCount", default)]
    pub cached_content_token_count: u32,
}

/// Encode a `ChatRequest` into the Gemini wire-shape request.
#[must_use]
pub fn encode_request(req: &ChatRequest) -> WireRequest<'_> {
    let mut contents = Vec::with_capacity(req.messages.len());
    for m in &req.messages {
        encode_message_into(m, &mut contents);
    }

    let system_instruction = if req.system.is_empty() {
        None
    } else {
        Some(WireContent {
            role: "user",
            parts: vec![WirePartOut::Text {
                text: req.system.clone(),
            }],
        })
    };

    let tool_group = if req.tools.is_empty() {
        Vec::new()
    } else {
        vec![WireToolGroup {
            function_declarations: req
                .tools
                .iter()
                .map(|t| WireFnDecl {
                    name: &t.name,
                    description: &t.description,
                    parameters: serde_json::from_str(&t.input_schema_json).unwrap_or_else(|_| json!({})),
                })
                .collect(),
        }]
    };

    WireRequest {
        contents,
        system_instruction,
        tools: tool_group,
    }
}

fn encode_message_into<'a>(m: &'a Message, out: &mut Vec<WireContent<'a>>) {
    match m.role {
        Role::System => {
            // System messages collapse into `systemInstruction` at the top
            // level. If a user routes a system Message through `messages`
            // anyway, fold its text into a regular `user` content block so
            // information is not silently dropped.
            let text = collect_text(&m.blocks);
            if !text.is_empty() {
                out.push(WireContent {
                    role: "user",
                    parts: vec![WirePartOut::Text { text }],
                });
            }
        }
        Role::User => {
            let text = collect_text(&m.blocks);
            let mut parts: Vec<WirePartOut<'a>> = Vec::new();
            if !text.is_empty() {
                parts.push(WirePartOut::Text { text });
            }
            if !parts.is_empty() {
                out.push(WireContent { role: "user", parts });
            }
        }
        Role::Assistant => {
            let mut parts: Vec<WirePartOut<'a>> = Vec::new();
            let text = collect_text(&m.blocks);
            if !text.is_empty() {
                parts.push(WirePartOut::Text { text });
            }
            for b in &m.blocks {
                if let Block::ToolUse { name, input_json, .. } = b {
                    let args: Value = serde_json::from_slice(input_json).unwrap_or(Value::Null);
                    parts.push(WirePartOut::FunctionCall {
                        function_call: WireFnCallOut { name, args },
                    });
                }
            }
            if !parts.is_empty() {
                out.push(WireContent { role: "model", parts });
            }
        }
        Role::Tool => {
            // Each tool_result becomes a `user` content with a functionResponse
            // part. Gemini doesn't have a dedicated tool role.
            for b in &m.blocks {
                if let Block::ToolResult {
                    tool_use_id, inline, ..
                } = b
                {
                    // Recover the function name from the synthesized id. The
                    // non-streaming path mints `call_<name>`; the streaming path
                    // mints `call_<name>_<idx>` (the index disambiguates parallel
                    // calls). Strip the `call_` prefix, then a trailing `_<idx>`
                    // ONLY when it is all digits, so function names that contain
                    // underscores (e.g. `fs_read`) are preserved. Gemini matches
                    // function responses by name, so this must equal the original
                    // `functionCall.name` or the turn is rejected.
                    let stripped = tool_use_id.strip_prefix("call_").unwrap_or(tool_use_id);
                    let name = match stripped.rsplit_once('_') {
                        Some((head, tail))
                            if !head.is_empty()
                                && !tail.is_empty()
                                && tail.bytes().all(|b| b.is_ascii_digit()) =>
                        {
                            head
                        }
                        _ => stripped,
                    };
                    let response: Value = inline
                        .as_deref()
                        .and_then(|v| serde_json::from_slice::<Value>(v).ok())
                        .unwrap_or_else(|| {
                            let s = inline
                                .as_deref()
                                .and_then(|v| std::str::from_utf8(v).ok())
                                .unwrap_or("");
                            json!({ "content": s })
                        });
                    out.push(WireContent {
                        role: "user",
                        parts: vec![WirePartOut::FunctionResponse {
                            function_response: WireFnResponseOut { name, response },
                        }],
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
