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
    /// Optional explicit cache TTL (e.g. `"1h"`). Omitted ⇒ the API's default
    /// 5-minute ephemeral cache. A 1-hour TTL keeps the stable system+tools+
    /// early-history prefix warm across agentic gaps longer than 5 minutes, at
    /// the cost of a ~2× cache write — worth it only on the most stable marker.
    #[serde(rename = "ttl", skip_serializing_if = "Option::is_none")]
    pub ttl: Option<&'static str>,
}

impl WireCacheControl {
    /// Construct the default (5-minute) ephemeral cache-control marker.
    #[must_use]
    pub const fn ephemeral() -> Self {
        Self {
            kind: "ephemeral",
            ttl: None,
        }
    }

    /// Ephemeral marker with an explicit 1-hour TTL, for the single most stable
    /// prefix boundary. Requires the account/request to have the extended cache
    /// TTL beta enabled.
    #[must_use]
    pub const fn ephemeral_1h() -> Self {
        Self {
            kind: "ephemeral",
            ttl: Some("1h"),
        }
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
    /// Output config — currently only the reasoning `effort` level. The Messages
    /// API takes effort as `output_config.effort`, NOT a top-level field; a
    /// top-level `effort` is an unknown body param and is rejected with 400.
    /// `None` is omitted entirely (byte-identical to the pre-effort behavior).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<WireOutputConfig>,
    /// Extended-thinking control block. `None` is omitted from the wire entirely
    /// (byte-identical to the pre-thinking behavior). See [`WireThinking`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<WireThinking>,
}

/// Anthropic `output_config` block (Messages API). Reasoning effort lives here,
/// not at the top level: `{"output_config":{"effort":"high"}}`.
#[derive(Serialize, Clone, Copy)]
pub struct WireOutputConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<&'static str>,
}

/// Anthropic extended-thinking control block.
///
/// Two on-modes: `{"type":"adaptive"}` — the only mode accepted by Claude
/// 4.7/4.8 and the recommended mode for 4.6/Sonnet 4.6; and the legacy
/// `{"type":"enabled","budget_tokens":n}`, which is valid only on Claude 4.5 and
/// earlier (it returns a 400 on 4.7+). The surrounding `WireRequest.thinking` is
/// omitted when thinking is unset so the default path stays byte-identical.
#[derive(Serialize, Clone, Copy)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireThinking {
    /// `{"type":"adaptive"}` — model decides when/how much to think.
    Adaptive,
    /// `{"type":"enabled","budget_tokens":n}` — legacy fixed-budget thinking.
    /// `budget_tokens` must be strictly less than the request's `max_tokens`.
    Enabled { budget_tokens: u32 },
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
