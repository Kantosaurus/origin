// SPDX-License-Identifier: Apache-2.0
//! Shared `OpenAI`-shape tool-call mapping.
//!
//! Several providers (`OpenAI` itself, plus `OpenAI`-compatible gateways) emit
//! tool calls under the same JSON shape. This module centralises the wire
//! types and the `WireToolCall → Block::ToolUse` conversion so per-provider
//! crates don't redefine them.

use origin_core::types::Block;
use serde::Deserialize;

/// Inner `function` payload of an OpenAI-shape tool call.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct WireToolFunction {
    /// Tool name (matches a `ToolSchema::name` advertised in the request).
    pub name: String,
    /// JSON-encoded arguments string (the `OpenAI` API serialises arguments as
    /// a string, not a nested object).
    pub arguments: String,
}

/// OpenAI-shape tool call as returned in a non-streaming chat completion.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct WireToolCall {
    /// Provider-assigned id (e.g. `"call_abc123"`).
    pub id: String,
    /// Always `"function"` in current `OpenAI` API; preserved for forward-compat.
    #[serde(rename = "type")]
    pub kind: String,
    /// Function payload (name + JSON-string arguments).
    pub function: WireToolFunction,
}

/// Convert an OpenAI-shape tool call into a canonical `Block::ToolUse`.
///
/// The `arguments` string is preserved verbatim as `input_json` bytes — callers
/// that need parsed JSON should decode it themselves.
#[must_use]
pub fn tool_call_to_block(tc: &WireToolCall) -> Block {
    Block::ToolUse {
        id: tc.id.clone(),
        name: tc.function.name.clone(),
        input_json: tc.function.arguments.clone().into_bytes(),
        cache_marker: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::panic)]
    fn maps_tool_call_to_block() {
        let tc = WireToolCall {
            id: "call_1".into(),
            kind: "function".into(),
            function: WireToolFunction {
                name: "fs_read".into(),
                arguments: r#"{"path":"x"}"#.into(),
            },
        };
        let block = tool_call_to_block(&tc);
        match block {
            Block::ToolUse {
                id,
                name,
                input_json,
                cache_marker,
            } => {
                assert_eq!(id, "call_1");
                assert_eq!(name, "fs_read");
                assert_eq!(input_json, br#"{"path":"x"}"#);
                assert!(cache_marker.is_none());
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn deserialises_openai_wire_shape() {
        let raw = r#"{"id":"call_1","type":"function","function":{"name":"fs_read","arguments":"{\"path\":\"x\"}"}}"#;
        let tc: WireToolCall = serde_json::from_str(raw).expect("parse");
        assert_eq!(tc.id, "call_1");
        assert_eq!(tc.kind, "function");
        assert_eq!(tc.function.name, "fs_read");
        assert_eq!(tc.function.arguments, r#"{"path":"x"}"#);
    }
}
