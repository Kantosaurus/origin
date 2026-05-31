// SPDX-License-Identifier: Apache-2.0
//! Private wire JSON types for the Anthropic Messages API.
//!
//! The `Wire` prefix on every type is intentional: it disambiguates these
//! internal serialisation types from the public provider types they mirror.
//! Module-name repetition is accepted for this private serialisation module.
//!
//! The Anthropic API uses `_tokens` as a postfix on all usage fields; renaming
//! them would diverge from the documented wire format, so `struct_field_names`
//! is suppressed here too.
#![allow(clippy::module_name_repetitions, clippy::struct_field_names)]

use serde::{Deserialize, Serialize};

/// Anthropic API cache-control marker. The only supported type is `"ephemeral"`.
#[derive(Serialize, Clone, Copy)]
pub struct WireCacheControl {
    /// Must be `"ephemeral"` per the Anthropic Messages API spec.
    #[serde(rename = "type")]
    pub kind: &'static str,
}

impl WireCacheControl {
    /// Construct the ephemeral cache-control marker.
    #[must_use]
    pub const fn ephemeral() -> Self {
        Self { kind: "ephemeral" }
    }
}

#[derive(Serialize)]
pub struct WireRequest<'a> {
    pub model: &'a str,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<&'a str>,
    pub messages: Vec<WireMessage<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<WireTool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<WireMetadata>,
    /// Reasoning-effort hint. `None` is omitted from the wire entirely, keeping
    /// the request byte-identical to the pre-effort behavior.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<&'static str>,
    /// Extended-thinking control block. `None` is omitted from the wire
    /// entirely, keeping the request byte-identical to the pre-thinking-tokens
    /// behavior. `Some` emits `"thinking": {"type":"enabled","budget_tokens":n}`
    /// (Anthropic Messages API); callers must ensure `max_tokens` exceeds the
    /// budget.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<WireThinking>,
}

/// Anthropic extended-thinking control block.
///
/// Serialises to `{"type":"enabled","budget_tokens": n}`. Only constructed when
/// a turn's `thinking_tokens` is `Some`; the surrounding `WireRequest.thinking`
/// is omitted otherwise so the unset path stays byte-identical.
#[derive(Serialize, Clone, Copy)]
pub struct WireThinking {
    /// Always `"enabled"` per the Messages API.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// Token budget the model may spend on thinking. Must be strictly less than
    /// the request's top-level `max_tokens`.
    pub budget_tokens: u32,
}

impl WireThinking {
    /// Construct an enabled extended-thinking block with the given budget.
    #[must_use]
    pub const fn enabled(budget_tokens: u32) -> Self {
        Self {
            kind: "enabled",
            budget_tokens,
        }
    }
}

#[derive(Serialize)]
pub struct WireMetadata {
    pub user_id: String,
}

#[derive(Serialize)]
pub struct WireMessage<'a> {
    pub role: &'static str,
    pub content: Vec<WireBlock<'a>>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireBlock<'a> {
    Text {
        text: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<WireCacheControl>,
    },
    ToolUse {
        id: &'a str,
        name: &'a str,
        input: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<WireCacheControl>,
    },
    ToolResult {
        tool_use_id: &'a str,
        content: &'a str,
        is_error: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<WireCacheControl>,
    },
}

#[derive(Serialize)]
pub struct WireTool<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub input_schema: serde_json::Value,
}

#[derive(Deserialize)]
pub struct WireResponse {
    pub content: Vec<WireRespBlock>,
    pub usage: WireUsage,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireRespBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    Thinking {
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },
}

#[derive(Deserialize, Default)]
pub struct WireUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(default)]
    pub cache_read_input_tokens: u32,
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
}
