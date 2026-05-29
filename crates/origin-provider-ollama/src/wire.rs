// SPDX-License-Identifier: Apache-2.0
//! Private wire JSON types for the Ollama `/api/chat` endpoint.
//!
//! The `Wire` prefix on every type disambiguates these internal serialisation
//! types from the public provider types they mirror.
#![allow(clippy::module_name_repetitions)]

use origin_core::types::{Block, Message, Role};
use origin_provider::ChatRequest;
use serde::{Deserialize, Serialize};

/// Top-level request body for `POST {base}/api/chat`.
#[derive(Serialize)]
pub struct WireRequest<'a> {
    pub model: &'a str,
    pub messages: Vec<WireMessage>,
    pub stream: bool,
}

/// One message in the Ollama request.
#[derive(Serialize)]
pub struct WireMessage {
    pub role: &'static str,
    pub content: String,
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
}

/// Encode a canonical `ChatRequest` into the Ollama wire-shape request.
#[must_use]
pub fn encode_request(req: &ChatRequest, stream: bool) -> WireRequest<'_> {
    let mut messages = Vec::with_capacity(req.messages.len() + 1);
    if !req.system.is_empty() {
        messages.push(WireMessage {
            role: "system",
            content: req.system.clone(),
        });
    }
    for m in &req.messages {
        let role = role_str(m.role);
        let content = collect_content(m);
        messages.push(WireMessage { role, content });
    }
    WireRequest {
        model: &req.model,
        messages,
        stream,
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
    // For P8.5 we only flatten Text + inline ToolResult bytes. Thinking and
    // structured tool_use are dropped — Ollama's tool API is out of scope.
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
