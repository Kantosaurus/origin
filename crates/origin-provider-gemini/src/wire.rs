// SPDX-License-Identifier: Apache-2.0
//! Private wire JSON types for the Gemini `generateContent` API.
//!
//! The `Wire` prefix on every type disambiguates these internal serialisation
//! types from the canonical provider types they mirror.
#![allow(clippy::module_name_repetitions)]

use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, ReasoningEffort};
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
    /// `generationConfig` — carries the thinking budget derived from the
    /// request's effort / explicit thinking-token count. `None` (no effort and
    /// no thinking tokens) omits the field, leaving the wire byte-identical.
    #[serde(rename = "generationConfig", skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<WireGenerationConfig>,
}

/// `generationConfig` subset we populate.
#[derive(Serialize)]
pub struct WireGenerationConfig {
    #[serde(rename = "thinkingConfig", skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<WireThinkingConfig>,
}

/// `thinkingConfig.thinkingBudget`: `-1` = dynamic (model decides), `0` = off,
/// `N` = explicit token budget.
#[derive(Serialize)]
pub struct WireThinkingConfig {
    #[serde(rename = "thinkingBudget")]
    pub thinking_budget: i32,
}

/// One `contents` entry. Used for both user/model turns and `systemInstruction`.
#[derive(Serialize)]
pub struct WireContent<'a> {
    pub role: &'static str,
    pub parts: Vec<WirePartOut<'a>>,
}

/// A single outgoing `parts` entry — text, inline media, functionCall, or
/// functionResponse.
#[derive(Serialize)]
#[serde(untagged)]
pub enum WirePartOut<'a> {
    Text {
        text: String,
    },
    InlineData {
        #[serde(rename = "inlineData")]
        inline_data: WireInlineData,
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

/// `inlineData` part — base64 media (images/PDFs) for multimodal input.
#[derive(Serialize)]
pub struct WireInlineData {
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub data: String,
}

#[derive(Serialize)]
pub struct WireFnCallOut<'a> {
    pub name: &'a str,
    pub args: Value,
}

#[derive(Serialize)]
pub struct WireFnResponseOut<'a> {
    /// Echo the original `functionCall.id` when available so Gemini maps the
    /// response to the right call — essential for parallel calls to the same
    /// function. Omitted when we only have a name (legacy/synthesized id).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<&'a str>,
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
    /// Gemini's call id (Gemini 3 always returns one). Echoed back in the
    /// matching `functionResponse.id`. Absent on older models.
    #[serde(default)]
    pub id: Option<String>,
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
    append_attachments(&mut contents, &req.attachments);

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
        generation_config: thinking_budget(req).map(|budget| WireGenerationConfig {
            thinking_config: Some(WireThinkingConfig {
                thinking_budget: budget,
            }),
        }),
    }
}

/// Derive the Gemini `thinkingBudget` from the request. An explicit
/// `thinking_tokens` count wins; otherwise effort maps to off (`Fast`) or
/// dynamic (everything else). `None` means "no signal" — emit no
/// `generationConfig`, so the request is unchanged from before.
fn thinking_budget(req: &ChatRequest) -> Option<i32> {
    if let Some(tokens) = req.thinking_tokens {
        return Some(i32::try_from(tokens).unwrap_or(i32::MAX));
    }
    req.effort.map(|e| match e {
        ReasoningEffort::Fast => 0,
        _ => -1,
    })
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
                    // Recover the function name (required) and the original call
                    // id (preferred). The non-streaming decoder packs a real
                    // Gemini id as `name#realid` (Gemini function names are
                    // word-chars only, so `#` is an unambiguous separator); the
                    // streaming path / legacy mints `call_<name>[_<idx>]` with no
                    // real id. Strip `call_` then a trailing all-digit `_<idx>`
                    // so names with underscores (e.g. `fs_read`) survive.
                    let (resp_id, name): (Option<&str>, &str) = match tool_use_id.split_once('#') {
                        Some((n, real)) if !n.is_empty() && !real.is_empty() => (Some(real), n),
                        _ => {
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
                            (None, name)
                        }
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
                            function_response: WireFnResponseOut {
                                id: resp_id,
                                name,
                                response,
                            },
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

/// Append request-level multimodal attachments (images/PDFs) as `inlineData`
/// parts on the last user content, mirroring the other providers' encoders.
/// Blocks without both a media type and base64 payload are skipped.
fn append_attachments<'a>(
    contents: &mut Vec<WireContent<'a>>,
    attachments: &[origin_multimodal::ContentBlock],
) {
    let parts: Vec<WirePartOut<'a>> = attachments
        .iter()
        .filter_map(|b| {
            let mime = b.media_type.as_ref()?;
            let data = b.base64.as_ref()?;
            Some(WirePartOut::InlineData {
                inline_data: WireInlineData {
                    mime_type: mime.clone(),
                    data: data.clone(),
                },
            })
        })
        .collect();
    if parts.is_empty() {
        return;
    }
    if let Some(last_user) = contents.iter_mut().rev().find(|c| c.role == "user") {
        last_user.parts.extend(parts);
    } else {
        contents.push(WireContent { role: "user", parts });
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn req(messages: Vec<Message>, attachments: Vec<origin_multimodal::ContentBlock>) -> ChatRequest {
        ChatRequest {
            system: String::new(),
            messages,
            model: "gemini-2.0-flash".into(),
            tools: vec![],
            effort: None,
            thinking_tokens: None,
            attachments,
        }
    }

    #[test]
    fn effort_and_thinking_tokens_map_to_thinking_budget() {
        // No effort + no thinking tokens ⇒ no generationConfig (byte-identical).
        assert!(encode_request(&req(vec![], vec![])).generation_config.is_none());

        // Explicit thinking tokens win.
        let mut r = req(vec![], vec![]);
        r.thinking_tokens = Some(4096);
        let budget = encode_request(&r)
            .generation_config
            .unwrap()
            .thinking_config
            .unwrap()
            .thinking_budget;
        assert_eq!(budget, 4096);

        // Fast ⇒ thinking off (0); higher levels ⇒ dynamic (-1).
        let mut fast = req(vec![], vec![]);
        fast.effort = Some(ReasoningEffort::Fast);
        assert_eq!(
            encode_request(&fast)
                .generation_config
                .unwrap()
                .thinking_config
                .unwrap()
                .thinking_budget,
            0
        );
        let mut high = req(vec![], vec![]);
        high.effort = Some(ReasoningEffort::High);
        assert_eq!(
            encode_request(&high)
                .generation_config
                .unwrap()
                .thinking_config
                .unwrap()
                .thinking_budget,
            -1
        );

        // Serialized JSON carries generationConfig.thinkingConfig.thinkingBudget.
        let json = serde_json::to_string(&encode_request(&r)).unwrap();
        assert!(json.contains("\"generationConfig\""), "{json}");
        assert!(json.contains("\"thinkingBudget\":4096"), "{json}");
    }

    #[test]
    fn attachments_become_inline_data_on_the_user_turn() {
        let r = req(
            vec![Message::new(Role::User).with_block(Block::text("describe"))],
            vec![origin_multimodal::ContentBlock::image("image/png", "QUJD")],
        );
        let body = serde_json::to_value(encode_request(&r)).unwrap();
        let parts = &body["contents"][0]["parts"];
        assert_eq!(parts[0]["text"], "describe");
        assert_eq!(parts[1]["inlineData"]["mimeType"], "image/png");
        assert_eq!(parts[1]["inlineData"]["data"], "QUJD");
    }

    #[test]
    fn function_response_echoes_the_packed_real_id() {
        let r = req(
            vec![Message {
                role: Role::Tool,
                blocks: vec![Block::ToolResult {
                    tool_use_id: "get_weather#abc123".into(),
                    handle: None,
                    inline: Some(br#"{"temp":20}"#.to_vec()),
                    cache_marker: None,
                }],
            }],
            vec![],
        );
        let body = serde_json::to_value(encode_request(&r)).unwrap();
        let fr = &body["contents"][0]["parts"][0]["functionResponse"];
        assert_eq!(fr["id"], "abc123", "the real Gemini call id must be echoed");
        assert_eq!(fr["name"], "get_weather");
    }

    #[test]
    fn legacy_synthesized_id_sends_name_only() {
        // `call_<name>` (no packed real id, name has an underscore) → name only.
        let r = req(
            vec![Message {
                role: Role::Tool,
                blocks: vec![Block::ToolResult {
                    tool_use_id: "call_fs_read".into(),
                    handle: None,
                    inline: Some(b"ok".to_vec()),
                    cache_marker: None,
                }],
            }],
            vec![],
        );
        let body = serde_json::to_value(encode_request(&r)).unwrap();
        let fr = &body["contents"][0]["parts"][0]["functionResponse"];
        assert!(fr.get("id").is_none(), "no real id → omit functionResponse.id");
        assert_eq!(fr["name"], "fs_read", "name with underscore must survive");
    }
}
