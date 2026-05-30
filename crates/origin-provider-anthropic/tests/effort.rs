// SPDX-License-Identifier: Apache-2.0
//! Verifies the Anthropic encoder emits `effort` only when set, and that the
//! default (`None`) leaves the request body free of an `effort` key.

use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, ReasoningEffort, ToolSchema};

fn req_with(effort: Option<ReasoningEffort>) -> ChatRequest {
    ChatRequest {
        system: String::new(),
        messages: vec![Message {
            role: Role::User,
            blocks: vec![Block::Text {
                text: "hi".to_string(),
                cache_marker: None,
            }],
        }],
        model: "claude-x".to_string(),
        tools: Vec::<ToolSchema>::new(),
        effort,
        attachments: Vec::new(),
    }
}

#[test]
fn default_none_omits_effort_key() {
    let body = origin_provider_anthropic::encode_request_for_test(&req_with(None));
    assert!(
        body.get("effort").is_none(),
        "effort key must be absent when effort is None (byte-identical default)"
    );
}

#[test]
fn set_effort_emits_wire_token() {
    let body = origin_provider_anthropic::encode_request_for_test(&req_with(Some(ReasoningEffort::High)));
    assert_eq!(body["effort"], "high");

    let body = origin_provider_anthropic::encode_request_for_test(&req_with(Some(ReasoningEffort::Fast)));
    assert_eq!(body["effort"], "fast");

    let body = origin_provider_anthropic::encode_request_for_test(&req_with(Some(ReasoningEffort::Max)));
    assert_eq!(body["effort"], "max");
}
