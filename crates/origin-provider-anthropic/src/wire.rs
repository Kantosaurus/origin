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

#[derive(Serialize)]
pub struct WireRequest<'a> {
    pub model: &'a str,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<&'a str>,
    pub messages: Vec<WireMessage<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<WireTool<'a>>,
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
